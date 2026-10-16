//! Lyric alignment: give each line of the user's lyrics a start and end time.
//!
//! Signals, strongest first:
//! 1. Whisper word timestamps (when available), matched to the user's words
//!    with a global sequence alignment, so repeats and misheard words work out.
//! 2. Vocal-stem energy: where singing phrases start and stop.
//! 3. Syllable counts: how long a line should take relative to its neighbours.
//! 4. Blank lines (verse breaks): prefer the longest gaps in the singing.
//!
//! Everything here is pure and unit-tested; `ai.rs` supplies the inputs.

use crate::model::{LyricLine, Lyrics, Peaks};

/// A word Whisper heard.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignSource {
    /// Matched against Whisper's word timestamps.
    Whisper,
    /// From the vocal stem's energy only (nothing uploaded).
    OnDevice,
    /// Local CTC forced alignment (the alignment model, offline).
    Local,
}

#[derive(Clone, Debug)]
pub struct AlignedLine {
    pub line: LyricLine,
    /// 0 = estimated, 1 = every word matched to the singing.
    pub confidence: f32,
    /// False when the line couldn't be found in the singing at all and was
    /// only placed between its neighbours (local alignment).
    pub found: bool,
}

#[derive(Clone, Debug)]
pub struct Alignment {
    /// Extra info for the review screen (piece count, language…).
    pub note: Option<String>,
    pub lines: Vec<AlignedLine>,
    /// Lines anchored to recognised words.
    pub matched: usize,
    pub source: AlignSource,
}

impl Alignment {
    pub fn total(&self) -> usize {
        self.lines.len()
    }
}

/// Confidence at or above which a line counts as "matched" in the UI.
pub const MATCHED: f32 = 0.5;

// --------------------------------------------------------------------------- text

struct UserLine {
    text: String,
    words: Vec<String>,
    syllables: f64,
    /// A blank line came before this one (new verse).
    stanza_break: bool,
}

fn parse_lyrics(text: &str) -> Vec<UserLine> {
    let mut out = Vec::new();
    let mut pending_break = false;
    for raw in text.lines() {
        let t = raw.trim();
        if t.is_empty() {
            pending_break = !out.is_empty();
            continue;
        }
        // Bracketed translations aren't sung: leave them out of matching and timing.
        let (sung, _) = crate::model::split_translation(t);
        let words: Vec<String> = sung.split_whitespace().map(normalize).filter(|w| !w.is_empty()).collect();
        let syllables = sung.split_whitespace().map(syllables).sum::<f64>().max(1.0);
        out.push(UserLine { text: t.to_owned(), words, syllables, stanza_break: pending_break });
        pending_break = false;
    }
    out
}

/// Lowercase, strip punctuation, fold common accents.
pub fn normalize(word: &str) -> String {
    word.chars()
        .filter_map(|c| {
            let c = match c {
                'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' | 'À' | 'Á' | 'Â' | 'Ä' => 'a',
                'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => 'e',
                'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => 'i',
                'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Ö' => 'o',
                'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => 'u',
                'ñ' | 'Ñ' => 'n',
                'ç' | 'Ç' => 'c',
                c => c,
            };
            c.is_alphanumeric().then(|| c.to_ascii_lowercase())
        })
        .collect()
}

/// Rough syllable count (vowel groups), at least 1.
fn syllables(word: &str) -> f64 {
    let w = normalize(word);
    let mut count = 0;
    let mut prev_vowel = false;
    for c in w.chars() {
        let v = "aeiouy".contains(c);
        if v && !prev_vowel {
            count += 1;
        }
        prev_vowel = v;
    }
    count.max(1) as f64
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// 1.0 = identical, 0.0 = nothing in common.
pub fn similarity(a: &str, b: &str) -> f32 {
    let n = a.chars().count().max(b.chars().count());
    if n == 0 {
        return 0.0;
    }
    1.0 - levenshtein(a, b) as f32 / n as f32
}

// --------------------------------------------------------------------------- word alignment

const EXACT: i32 = 3;
const CLOSE: i32 = 2;
const MISMATCH: i32 = -1;
const GAP: i32 = -1;

fn pair_score(a: &str, b: &str) -> i32 {
    if a == b {
        EXACT
    } else if similarity(a, b) >= 0.7 {
        CLOSE
    } else {
        MISMATCH
    }
}

/// Needleman–Wunsch. For each user word: the heard word it matched (exact or close).
fn align_words(user: &[String], heard: &[String]) -> Vec<Option<usize>> {
    let (n, m) = (user.len(), heard.len());
    let mut dp = vec![0i32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 1..=n {
        dp[at(i, 0)] = i as i32 * GAP;
    }
    for j in 1..=m {
        dp[at(0, j)] = j as i32 * GAP;
    }
    for i in 1..=n {
        for j in 1..=m {
            let diag = dp[at(i - 1, j - 1)] + pair_score(&user[i - 1], &heard[j - 1]);
            dp[at(i, j)] = diag.max(dp[at(i - 1, j)] + GAP).max(dp[at(i, j - 1)] + GAP);
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let s = pair_score(&user[i - 1], &heard[j - 1]);
        if dp[at(i, j)] == dp[at(i - 1, j - 1)] + s {
            if s > 0 {
                out[i - 1] = Some(j - 1);
            }
            i -= 1;
            j -= 1;
        } else if dp[at(i, j)] == dp[at(i - 1, j)] + GAP {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    out
}

// --------------------------------------------------------------------------- vocal phrases

/// Spans where the singer is audible: smoothed energy over an adaptive
/// threshold, short gaps merged, blips dropped.
pub fn detect_phrases(peaks: &Peaks, from: f64, to: f64) -> Vec<(f64, f64)> {
    if peaks.data.is_empty() || to <= from {
        return Vec::new();
    }
    let ps = peaks.per_sec as f64;
    let (i0, i1) = (((from * ps) as usize).min(peaks.data.len()), ((to * ps) as usize).min(peaks.data.len()));
    if i1 <= i0 {
        return Vec::new();
    }
    // 50 ms moving average.
    let win = ((0.05 * ps) as usize).max(1);
    let raw = &peaks.data[i0..i1];
    let smooth: Vec<f32> = (0..raw.len())
        .map(|k| {
            let a = k.saturating_sub(win / 2);
            let b = (k + win / 2 + 1).min(raw.len());
            raw[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect();
    let mut sorted = smooth.clone();
    sorted.sort_by(f32::total_cmp);
    let p90 = sorted[(sorted.len() as f32 * 0.9) as usize % sorted.len()];
    let threshold = (p90 * 0.22).max(0.02);

    let mut phrases: Vec<(f64, f64)> = Vec::new();
    let mut start = None;
    for (k, v) in smooth.iter().enumerate() {
        let t = from + k as f64 / ps;
        match (start, *v > threshold) {
            (None, true) => start = Some(t),
            (Some(s), false) => {
                phrases.push((s, t));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        phrases.push((s, from + smooth.len() as f64 / ps));
    }
    // Merge breaths shorter than 0.25 s, drop blips under 0.15 s.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for p in phrases {
        match merged.last_mut() {
            Some(last) if p.0 - last.1 < 0.25 => last.1 = p.1,
            _ => merged.push(p),
        }
    }
    merged.retain(|(a, b)| b - a >= 0.15);
    merged
}

/// Map cumulative "work" (syllables) onto cumulative singing time.
/// Returns, for each line, (start, end) inside `[lo, hi]`.
fn spread_by_singing(lines: &[&UserLine], phrases: &[(f64, f64)], lo: f64, hi: f64) -> Vec<(f64, f64)> {
    let active: Vec<(f64, f64)> = phrases.iter().map(|&(a, b)| (a.max(lo), b.min(hi))).filter(|(a, b)| b > a).collect();
    let active = if active.is_empty() { vec![(lo, hi)] } else { active };
    let total_active: f64 = active.iter().map(|(a, b)| b - a).sum();
    // Position `x` seconds into the singing → song time.
    let at = |x: f64| -> f64 {
        let mut left = x;
        for &(a, b) in &active {
            if left <= b - a {
                return a + left;
            }
            left -= b - a;
        }
        active.last().map_or(hi, |p| p.1)
    };
    let total_syl: f64 = lines.iter().map(|l| l.syllables).sum::<f64>().max(1.0);
    let mut acc = 0.0;
    lines
        .iter()
        .map(|l| {
            let s = at(acc / total_syl * total_active);
            acc += l.syllables;
            let e = at(acc / total_syl * total_active);
            (s, e)
        })
        .collect()
}

/// With at least as many phrases as lines: split the phrases into
/// contiguous groups, one per line, so each line's share of the syllables
/// matches its share of the singing time. Verse breaks favour long gaps.
fn assign_phrases(lines: &[&UserLine], phrases: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let (n, p) = (lines.len(), phrases.len());
    let total_syl: f64 = lines.iter().map(|l| l.syllables).sum::<f64>().max(1.0);
    let total_time: f64 = phrases.iter().map(|(a, b)| b - a).sum::<f64>().max(1e-6);
    let mut dur_prefix = vec![0.0; p + 1];
    for (k, (a, b)) in phrases.iter().enumerate() {
        dur_prefix[k + 1] = dur_prefix[k] + (b - a);
    }
    // cost[i][j]: best cost for the first i lines using the first j phrases.
    let inf = f64::INFINITY;
    let mut cost = vec![vec![inf; p + 1]; n + 1];
    let mut from = vec![vec![0usize; p + 1]; n + 1];
    cost[0][0] = 0.0;
    for i in 1..=n {
        let line = lines[i - 1];
        let syl_share = line.syllables / total_syl;
        // Leave at least one phrase for each remaining line.
        for j in i..=p - (n - i) {
            for k in (i - 1)..j {
                if cost[i - 1][k] == inf {
                    continue;
                }
                let dur_share = (dur_prefix[j] - dur_prefix[k]) / total_time;
                let gap_before = if k == 0 { 0.0 } else { phrases[k].0 - phrases[k - 1].1 };
                let mut c = cost[i - 1][k] + (syl_share - dur_share).abs();
                // Line starts are better after real gaps; verse breaks after big ones.
                c -= gap_before.min(4.0) * if line.stanza_break { 0.08 } else { 0.02 };
                if c < cost[i][j] {
                    cost[i][j] = c;
                    from[i][j] = k;
                }
            }
        }
    }
    let mut out = vec![(0.0, 0.0); n];
    let mut j = p;
    for i in (1..=n).rev() {
        let k = from[i][j];
        out[i - 1] = (phrases[k].0, phrases[j - 1].1);
        j = k;
    }
    out
}

/// Nudge a start to a nearby phrase onset; verse breaks prefer the onset
/// after the longest gap.
fn snap_start(t: f64, phrases: &[(f64, f64)], window: f64, stanza: bool) -> f64 {
    let mut best: Option<(f64, f64)> = None; // (score, onset)
    for (k, &(onset, _)) in phrases.iter().enumerate() {
        let d = (onset - t).abs();
        if d > window {
            continue;
        }
        let gap = if k == 0 { 2.0 } else { onset - phrases[k - 1].1 };
        let score = if stanza { d - gap.min(4.0) } else { d - gap.min(1.0) * 0.3 };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, onset));
        }
    }
    best.map_or(t, |(_, o)| o)
}

// --------------------------------------------------------------------------- main entry

/// Time the user's lyrics. `words` = Whisper words (None for on-device);
/// `vocals` = vocal-stem envelope if the song is split.
pub fn align(lyrics_text: &str, words: Option<&[Word]>, vocals: Option<&Peaks>, duration: f64) -> Alignment {
    let user = parse_lyrics(lyrics_text);
    let source = if words.is_some() { AlignSource::Whisper } else { AlignSource::OnDevice };
    if user.is_empty() {
        return Alignment { note: None, lines: Vec::new(), matched: 0, source };
    }
    let phrases = vocals.map(|v| detect_phrases(v, 0.0, duration)).unwrap_or_default();

    // 1. Anchors from Whisper words.
    let mut anchors: Vec<Option<(f64, f64, f32)>> = vec![None; user.len()];
    if let Some(heard) = words.filter(|w| !w.is_empty()) {
        let heard_norm: Vec<String> = heard.iter().map(|w| normalize(&w.text)).collect();
        let flat: Vec<String> = user.iter().flat_map(|l| l.words.iter().cloned()).collect();
        let matches = align_words(&flat, &heard_norm);
        let mut durs: Vec<f64> = heard.iter().map(|w| (w.end - w.start).max(0.05)).collect();
        durs.sort_by(f64::total_cmp);
        let word_dur = durs.get(durs.len() / 2).copied().unwrap_or(0.4).clamp(0.15, 1.0) + 0.05;

        let mut flat_i = 0;
        for (li, line) in user.iter().enumerate() {
            let range = flat_i..flat_i + line.words.len();
            flat_i += line.words.len();
            let hits: Vec<(usize, usize)> =
                range.clone().filter_map(|k| matches[k].map(|h| (k - range.start, h))).collect();
            let Some(&(first_pos, first_h)) = hits.first() else { continue };
            let &(last_pos, last_h) = hits.last().unwrap();
            // A single short common word ("the", "and") on its own is not enough.
            if hits.len() == 1 && line.words.len() >= 4 && line.words[first_pos].chars().count() <= 3 {
                continue;
            }
            let start = heard[first_h].start - first_pos as f64 * word_dur;
            let end = heard[last_h].end + (line.words.len() - 1 - last_pos) as f64 * word_dur;
            anchors[li] = Some((start.max(0.0), end.min(duration), hits.len() as f32 / line.words.len().max(1) as f32));
        }
        // Keep anchors in order; drop any that would go back in time.
        let mut last_end = f64::MIN;
        for a in anchors.iter_mut() {
            if let Some((s, e, _)) = *a {
                if s + 0.5 < last_end {
                    *a = None;
                } else {
                    last_end = e;
                }
            }
        }
    }

    // 2. Fill runs of unanchored lines between anchors.
    let lead_in = phrases.first().map_or(duration * 0.04, |p| p.0);
    let lead_out = phrases.last().map_or(duration * 0.96, |p| p.1);
    let mut times: Vec<(f64, f64, f32)> = vec![(0.0, 0.0, 0.0); user.len()];
    let mut i = 0;
    while i < user.len() {
        if let Some(a) = anchors[i] {
            times[i] = a;
            i += 1;
            continue;
        }
        let j = (i..user.len()).find(|&k| anchors[k].is_some()).unwrap_or(user.len());
        let lo = if i == 0 { lead_in } else { times[i - 1].1 };
        let hi = if j == user.len() { lead_out.max(lo + 1.0) } else { anchors[j].unwrap().0 };
        let hi = hi.max(lo + 0.5);
        let run: Vec<&UserLine> = user[i..j].iter().collect();
        // Phrases that begin inside the gap (one that started earlier belongs to the line before).
        let inside: Vec<(f64, f64)> = phrases
            .iter()
            .filter(|(a, _)| *a >= lo - 0.05 && *a < hi)
            .map(|&(a, b)| (a, b.min(hi)))
            .collect();
        if inside.len() >= run.len() {
            for (k, (s, e)) in assign_phrases(&run, &inside).into_iter().enumerate() {
                times[i + k] = (s, e.max(s + Lyrics::MIN_LEN), 0.0);
            }
        } else {
            let spans = spread_by_singing(&run, &inside, lo, hi);
            for (k, (s, e)) in spans.into_iter().enumerate() {
                let s = snap_start(s, &inside, 1.5, run[k].stanza_break).clamp(lo, hi);
                times[i + k] = (s, e.max(s + Lyrics::MIN_LEN), 0.0);
            }
        }
        i = j;
    }

    // 3. Tidy: ordered, non-overlapping, minimum length, gentle hold after the last word.
    for k in 0..times.len() {
        let next_start = times.get(k + 1).map_or(duration, |t| t.0);
        let (s, e, _) = &mut times[k];
        *s = s.clamp(0.0, duration);
        *e = (*e + 0.4).min(next_start).max(*s + Lyrics::MIN_LEN).min(duration.max(*s + Lyrics::MIN_LEN));
        if k + 1 < times.len() && times[k + 1].0 < times[k].1 {
            times[k + 1].0 = times[k].1;
        }
    }

    let matched = anchors.iter().filter(|a| a.is_some_and(|(_, _, c)| c >= MATCHED)).count();
    let lines = user
        .iter()
        .zip(times)
        .map(|(u, (s, e, c))| AlignedLine { line: LyricLine::new(u.text.clone(), s, e), confidence: c, found: true })
        .collect();
    Alignment { note: None, lines, matched, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whisper-like words for `text` starting at `t0`, 0.4 s per word.
    fn sing(text: &str, t0: f64) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| Word { text: w.into(), start: t0 + i as f64 * 0.4, end: t0 + i as f64 * 0.4 + 0.35 })
            .collect()
    }

    fn check_tidy(a: &Alignment) {
        for w in a.lines.windows(2) {
            assert!(w[0].line.end <= w[1].line.start + 1e-9, "overlap: {:?}", a.lines);
        }
        assert!(a.lines.iter().all(|l| l.line.end - l.line.start >= Lyrics::MIN_LEN - 1e-9));
    }

    #[test]
    fn exact_words_anchor_every_line() {
        let lyrics = "Amazing grace how sweet the sound\nThat saved a wretch like me";
        let mut words = sing("Amazing grace how sweet the sound", 10.0);
        words.extend(sing("That saved a wretch like me", 16.0));
        let a = align(lyrics, Some(&words), None, 60.0);
        assert_eq!(a.matched, 2);
        assert!((a.lines[0].line.start - 10.0).abs() < 1e-6);
        assert!((a.lines[1].line.start - 16.0).abs() < 1e-6);
        assert_eq!(a.lines[0].line.text, "Amazing grace how sweet the sound", "user text is kept exactly");
        check_tidy(&a);
    }

    #[test]
    fn misheard_words_still_match() {
        let lyrics = "Wakamau ni mjaka\nNakupenda Yesu";
        let mut words = sing("wakamao ni mjaka", 5.0);
        words.extend(sing("nakupend yesu", 9.0));
        let a = align(lyrics, Some(&words), None, 30.0);
        assert_eq!(a.matched, 2);
        assert!((a.lines[0].line.start - 5.0).abs() < 1e-6);
        assert!((a.lines[1].line.start - 9.0).abs() < 1e-6);
    }

    #[test]
    fn missed_line_lands_in_the_vocal_phrase_between_neighbours() {
        // Singing at 10-12, 14-16 (Whisper missed this one), 18-20.
        let mut env = vec![0.0f32; 3000];
        for (a, b) in [(10.0, 12.0), (14.0, 16.0), (18.0, 20.0)] {
            env[(a * 100.0) as usize..(b * 100.0) as usize].iter_mut().for_each(|v| *v = 0.8);
        }
        let peaks = Peaks { per_sec: 100.0, data: env };
        let lyrics = "first line here\nsecond line here\nthird line here";
        let mut words = sing("first line here", 10.0);
        words.extend(sing("third line here", 18.0));
        let a = align(lyrics, Some(&words), Some(&peaks), 30.0);
        let mid = &a.lines[1];
        assert_eq!(mid.confidence, 0.0, "estimated, not matched");
        assert!((mid.line.start - 14.0).abs() < 0.3, "snapped to the phrase onset: {mid:?}");
        check_tidy(&a);
    }

    #[test]
    fn repeated_chorus_maps_to_the_right_occurrence() {
        let lyrics = "verse one words\nchorus goes here\nverse two other words\nchorus goes here";
        let mut words = sing("verse one words", 0.0);
        words.extend(sing("chorus goes here", 5.0));
        words.extend(sing("verse two other words", 10.0));
        words.extend(sing("chorus goes here", 20.0));
        let a = align(lyrics, Some(&words), None, 40.0);
        assert!((a.lines[1].line.start - 5.0).abs() < 1e-6);
        assert!((a.lines[3].line.start - 20.0).abs() < 1e-6, "second chorus, not the first: {:?}", a.lines[3]);
    }

    #[test]
    fn on_device_puts_one_line_per_phrase() {
        let mut env = vec![0.0f32; 4000];
        let spans = [(5.0, 8.0), (10.0, 13.0), (16.0, 19.0), (22.0, 25.0)];
        for (a, b) in spans {
            env[(a * 100.0) as usize..(b * 100.0) as usize].iter_mut().for_each(|v| *v = 0.7);
        }
        let peaks = Peaks { per_sec: 100.0, data: env };
        let lyrics = "one two three four\nfive six seven eight\n\nnine ten eleven twelve\nthirteen fourteen fifteen sixteen";
        let a = align(lyrics, None, Some(&peaks), 40.0);
        assert_eq!(a.source, AlignSource::OnDevice);
        for (l, (s, _)) in a.lines.iter().zip(spans) {
            assert!((l.line.start - s).abs() < 0.6, "{:?} vs phrase at {s}", l.line);
        }
        check_tidy(&a);
    }

    #[test]
    fn phrases_merge_breaths_and_ignore_blips() {
        let mut env = vec![0.0f32; 1000];
        env[100..300].iter_mut().for_each(|v| *v = 0.8); // 1-3 s
        env[310..400].iter_mut().for_each(|v| *v = 0.8); // breath 0.1 s, then to 4 s
        env[600..605].iter_mut().for_each(|v| *v = 0.8); // 50 ms blip
        let p = detect_phrases(&Peaks { per_sec: 100.0, data: env }, 0.0, 10.0);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!((p[0].0 - 1.0).abs() < 0.1 && (p[0].1 - 4.0).abs() < 0.1);
    }

    /// Phrase detection on a real vocals stem: `STAGEWEAVE_VOCALS=…/vocals.wav cargo test real_vocals -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_vocals_phrases() {
        let path = std::env::var("STAGEWEAVE_VOCALS").expect("set STAGEWEAVE_VOCALS");
        let (audio, _) = crate::engine::decode_file_for_tests(std::path::Path::new(&path));
        let peaks = audio.peaks(crate::engine::PEAKS_PER_SEC);
        let phrases = detect_phrases(&peaks, 0.0, audio.duration());
        let lens: Vec<f64> = phrases.iter().map(|(a, b)| b - a).collect();
        println!(
            "{:.0}s song → {} phrases; first at {:.1}s, last ends {:.1}s; median length {:.1}s",
            audio.duration(),
            phrases.len(),
            phrases.first().map_or(0.0, |p| p.0),
            phrases.last().map_or(0.0, |p| p.1),
            { let mut l = lens.clone(); l.sort_by(f64::total_cmp); l.get(l.len() / 2).copied().unwrap_or(0.0) }
        );
        for p in phrases.iter().take(12) {
            println!("  {:6.1} – {:6.1}", p.0, p.1);
        }
        assert!(phrases.len() > 10, "a sung song has many phrases");
    }

    #[test]
    fn translations_do_not_take_part_in_matching() {
        let lyrics = "Ana mipango mema (He has good plans)\nAnakupenda (He loves You)";
        let mut words = sing("Ana mipango mema", 4.0);
        words.extend(sing("anakupenda", 8.0));
        let a = align(lyrics, Some(&words), None, 20.0);
        assert_eq!(a.matched, 2);
        assert_eq!(a.lines[0].confidence, 1.0, "all sung words matched; the translation isn't expected");
        assert!((a.lines[1].line.start - 8.0).abs() < 1e-6);
        assert_eq!(a.lines[0].line.text, "Ana mipango mema (He has good plans)", "text kept as written");
    }

    #[test]
    fn text_helpers() {
        assert_eq!(normalize("Wakamaú,"), "wakamau");
        assert!(similarity("wakamau", "wakamao") >= 0.7);
        assert_eq!(syllables("amazing"), 3.0);
        assert_eq!(syllables("the"), 1.0);
    }
}
