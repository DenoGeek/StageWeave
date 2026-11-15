//! CTC forced alignment: time the user's own lyrics against a speech model's
//! per-frame character probabilities (from the vocals stem).
//!
//! The acoustic model (wav2vec2, run by the Python worker) gives, for every
//! 20 ms frame, the log-probability of each character and of the CTC *blank*.
//! Nothing is transcribed: the known lyrics become a token sequence and a
//! Viterbi search finds the most likely moment each token is sung, under the
//! CTC rules (each token in order, blanks in between, repeats need a blank).
//!
//! On top of plain CTC:
//! - Between lines (and before the first / after the last) sits a *wildcard*
//!   state that can absorb any sound, as in MMS-style alignment. Singing that
//!   isn't in the lyrics (a verse left out, ad-libs, a choir answering) goes
//!   there instead of dragging the next line early.
//! - A whole line may be *skipped* at a cost, so a verse in the lyrics that
//!   isn't sung comes back flagged instead of crammed in.
//!
//! Pure and unit-tested; `engine::local_align` supplies the emissions.

use std::collections::HashMap;

use crate::model::{LyricLine, LyricWord};

/// Version of the `AlignmentResult` JSON contract.
pub const RESULT_VERSION: u32 = 1;

/// Per-frame log-probabilities, row-major `[frames × classes]`.
pub struct Emissions {
    pub frames: usize,
    pub classes: usize,
    /// Seconds per frame (0.02 for wav2vec2).
    pub stride: f64,
    pub logp: Vec<f32>,
}

impl Emissions {
    fn at(&self, t: usize, class: usize) -> f32 {
        self.logp[t * self.classes + class]
    }
}

/// The model's character vocabulary (from its `vocab.json`).
pub struct Vocab {
    chars: HashMap<char, usize>,
    pub blank: usize,
    /// Word separator token (`|`), if the model has one.
    pub sep: Option<usize>,
}

impl Vocab {
    pub fn from_map(map: &HashMap<String, usize>) -> Result<Self, String> {
        let blank = *map.get("<pad>").or_else(|| map.get("<blank>")).ok_or("vocab has no CTC blank (<pad>)")?;
        let sep = map.get("|").copied();
        let chars = map
            .iter()
            .filter_map(|(k, &v)| {
                let mut it = k.chars();
                let c = it.next()?;
                (it.next().is_none() && c != '|').then_some((c, v))
            })
            .collect();
        Ok(Self { chars, blank, sep })
    }
}

// --------------------------------------------------------------------------- result contract

/// How a line was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineStatus {
    /// Its words were found in the singing.
    Aligned,
    /// Not found in the singing (skipped by the search): times are only the
    /// gap between its neighbours, with no word timing.
    NotFound,
    /// Nothing the English model can match (other scripts, only brackets…):
    /// placed between its neighbours.
    Unmatchable,
}

/// One word. `confidence` is `None` when the word couldn't be matched itself
/// (no supported letters, or a bracketed translation) and was placed between
/// its neighbours.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResultWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResultLine {
    pub index: usize,
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Mean confidence of its matched words; 0 when not aligned.
    pub confidence: f32,
    pub status: LineStatus,
    pub words: Vec<ResultWord>,
}

/// Versioned output of a local alignment (seconds throughout).
///
/// Confidence (0–1): for each matched word, the mean probability the model
/// gave the word's letters on the frames the alignment assigned to them. High
/// means the model clearly heard those letters there; low (≲ 0.35) means the
/// timing was forced and should be checked.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AlignmentResult {
    pub version: u32,
    pub model: String,
    pub language: String,
    pub duration: f64,
    pub lines: Vec<ResultLine>,
}

impl AlignmentResult {
    /// Lines with timing for the editor, word timings attached.
    pub fn lyric_lines(&self) -> Vec<LyricLine> {
        self.lines
            .iter()
            .map(|l| LyricLine {
                words: l.words.iter().map(|w| LyricWord { text: w.text.clone(), start: w.start, end: w.end }).collect(),
                ..LyricLine::new(l.text.clone(), l.start, l.end)
            })
            .collect()
    }
}

/// A line's confidence below which the review asks for a check.
pub const CHECK_BELOW: f32 = 0.35;

// --------------------------------------------------------------------------- text

/// One word of the user's lyrics and the tokens it is matched with.
#[derive(Debug, PartialEq)]
pub struct WordRef {
    pub line: usize,
    pub original: String,
    /// Empty when nothing in the word can be matched.
    pub tokens: Vec<usize>,
}

/// Split lines into words and map each to model tokens, keeping the
/// original text. Bracketed translations and unsupported scripts get no
/// tokens; repeats are kept as written.
pub fn normalize(lines: &[String], vocab: &Vocab) -> Vec<WordRef> {
    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        let mut depth = 0i32;
        for word in line.split_whitespace() {
            let starts_inside = depth > 0 || word.starts_with('(');
            depth += word.matches('(').count() as i32 - word.matches(')').count() as i32;
            depth = depth.max(0);
            let tokens = if starts_inside { Vec::new() } else { word_tokens(word, vocab) };
            out.push(WordRef { line: li, original: word.to_owned(), tokens });
        }
    }
    out
}

fn word_tokens(word: &str, vocab: &Vocab) -> Vec<usize> {
    let spoken = spoken_form(word);
    let mut tokens = Vec::new();
    for (k, part) in spoken.split(' ').filter(|p| !p.is_empty()).enumerate() {
        if k > 0
            && let Some(sep) = vocab.sep
        {
            tokens.push(sep);
        }
        tokens.extend(part.chars().filter_map(|c| vocab.chars.get(&c).copied()));
    }
    // A separator left dangling by an unmatchable part.
    while tokens.last().is_some_and(|t| Some(*t) == vocab.sep) {
        tokens.pop();
    }
    tokens
}

/// Upper-case letters and apostrophes as they'd be sung: accents folded,
/// numbers spelled out ("30" → "THIRTY"), "&" → "AND", punctuation dropped.
/// Several spoken words are separated by spaces.
pub fn spoken_form(word: &str) -> String {
    let mut out = String::new();
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        if !digits.is_empty() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(&number_words(digits));
            out.push(' ');
            digits.clear();
        }
    };
    for c in word.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        flush(&mut digits, &mut out);
        match c {
            '&' => out.push_str(" AND "),
            '\'' | '’' | '‘' | 'ʼ' => out.push('\''),
            _ => {
                for f in fold(c).chars() {
                    if f.is_ascii_alphabetic() {
                        out.push(f.to_ascii_uppercase());
                    } else if f == '-' {
                        out.push(' ');
                    }
                }
            }
        }
    }
    flush(&mut digits, &mut out);
    // Quote marks around a word aren't sung; inner apostrophes ("DON'T") are.
    out.split(' ').map(|w| w.trim_matches('\'')).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Latin letters with diacritics → their base letters (common European set).
fn fold(c: char) -> String {
    let base = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'æ' => "ae",
        'Æ' => "AE",
        'ç' | 'ć' | 'č' | 'ĉ' => "c",
        'Ç' | 'Ć' | 'Č' | 'Ĉ' => "C",
        'ď' | 'đ' => "d",
        'Ď' | 'Đ' => "D",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => "E",
        'ğ' | 'ĝ' => "g",
        'Ğ' | 'Ĝ' => "G",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Į' | 'İ' => "I",
        'ł' | 'ľ' | 'ĺ' => "l",
        'Ł' | 'Ľ' | 'Ĺ' => "L",
        'ñ' | 'ń' | 'ň' => "n",
        'Ñ' | 'Ń' | 'Ň' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ő' => "O",
        'œ' => "oe",
        'Œ' => "OE",
        'ř' | 'ŕ' => "r",
        'Ř' | 'Ŕ' => "R",
        'ß' => "ss",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'Ś' | 'Š' | 'Ş' | 'Ș' => "S",
        'ť' | 'ţ' | 'ț' => "t",
        'Ť' | 'Ţ' | 'Ț' => "T",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ů' | 'Ű' | 'Ų' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' | 'Ÿ' => "Y",
        'ź' | 'ż' | 'ž' => "z",
        'Ź' | 'Ż' | 'Ž' => "Z",
        _ => return c.to_string(),
    };
    base.to_owned()
}

/// English words for 0–9999 (larger numbers are read digit by digit).
pub fn number_words(digits: &str) -> String {
    const ONES: [&str; 20] = [
        "ZERO", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE", "TEN", "ELEVEN", "TWELVE", "THIRTEEN",
        "FOURTEEN", "FIFTEEN", "SIXTEEN", "SEVENTEEN", "EIGHTEEN", "NINETEEN",
    ];
    const TENS: [&str; 10] = ["", "", "TWENTY", "THIRTY", "FORTY", "FIFTY", "SIXTY", "SEVENTY", "EIGHTY", "NINETY"];
    fn below_100(n: usize) -> String {
        if n < 20 {
            ONES[n].to_owned()
        } else if n.is_multiple_of(10) {
            TENS[n / 10].to_owned()
        } else {
            format!("{} {}", TENS[n / 10], ONES[n % 10])
        }
    }
    fn below_1000(n: usize) -> String {
        match (n / 100, n % 100) {
            (0, r) => below_100(r),
            (h, 0) => format!("{} HUNDRED", ONES[h]),
            (h, r) => format!("{} HUNDRED {}", ONES[h], below_100(r)),
        }
    }
    let Ok(n) = digits.parse::<usize>() else { return String::new() };
    if digits.len() > 4 || (digits.len() > 1 && digits.starts_with('0')) {
        return digits.chars().map(|d| ONES[d.to_digit(10).unwrap_or(0) as usize]).collect::<Vec<_>>().join(" ");
    }
    match (n / 1000, n % 1000) {
        (0, r) => below_1000(r),
        (t, 0) => format!("{} THOUSAND", ONES[t]),
        (t, r) => format!("{} THOUSAND {}", ONES[t], below_1000(r)),
    }
}

// --------------------------------------------------------------------------- search

/// Cost (log-probability per token) of skipping a line instead of singing it.
/// Sung lines cost far less than this per token; a line that isn't in the
/// audio costs far more when forced in.
const SKIP_COST_PER_TOKEN: f32 = 4.0;

/// Backpointer codes.
const STAY: u8 = 0;
const STEP: u8 = 1;
const JUMP: u8 = 2;
const SKIP_LINE: u8 = 3;

/// The token sequence with blanks interleaved, plus where each line begins.
struct Trellis {
    /// Label per state: even states are blanks, odd states tokens.
    labels: Vec<usize>,
    /// Word index for each token state (`None` for separators and blanks).
    owner: Vec<Option<usize>>,
    /// For each line with tokens: the blank state just before its first token.
    line_entry: Vec<usize>,
    /// Token count per entry above (for the skip cost).
    line_tokens: Vec<usize>,
}

fn build_trellis(words: &[WordRef], vocab: &Vocab) -> Trellis {
    let mut tokens: Vec<(usize, Option<usize>)> = Vec::new();
    let mut line_entry = Vec::new();
    let mut line_tokens = Vec::new();
    let mut current_line = None;
    for (wi, w) in words.iter().enumerate() {
        if w.tokens.is_empty() {
            continue;
        }
        if !tokens.is_empty()
            && let Some(sep) = vocab.sep
        {
            tokens.push((sep, None));
        }
        if current_line != Some(w.line) {
            current_line = Some(w.line);
            line_entry.push(2 * tokens.len());
            line_tokens.push(0);
        }
        *line_tokens.last_mut().unwrap() += w.tokens.len();
        tokens.extend(w.tokens.iter().map(|&t| (t, Some(wi))));
    }
    let mut labels = vec![vocab.blank; 2 * tokens.len() + 1];
    let mut owner = vec![None; labels.len()];
    for (k, (t, o)) in tokens.into_iter().enumerate() {
        labels[2 * k + 1] = t;
        owner[2 * k + 1] = o;
    }
    Trellis { labels, owner, line_entry, line_tokens }
}

/// Best state per frame (Viterbi), with optional whole-line skips.
fn viterbi(em: &Emissions, tr: &Trellis) -> Vec<usize> {
    let s_len = tr.labels.len();
    let t_len = em.frames;
    const NEG: f32 = f32::NEG_INFINITY;
    // Wildcard states: the blank before each line and the final blank score
    // the frame's best class, so unwritten singing costs nothing extra there.
    let mut wild = vec![false; s_len];
    for &entry in &tr.line_entry {
        wild[entry] = true;
    }
    wild[s_len - 1] = true;
    let frame_best: Vec<f32> =
        (0..t_len).map(|t| em.logp[t * em.classes..(t + 1) * em.classes].iter().copied().fold(NEG, f32::max)).collect();
    let emit = |t: usize, s: usize| if wild[s] { frame_best[t] } else { em.at(t, tr.labels[s]) };
    // Skip edges: entry of line k → entry of line k+1 (or the final blank).
    let mut skip_from = vec![usize::MAX; s_len];
    let mut skip_cost = vec![0f32; s_len];
    for (k, &entry) in tr.line_entry.iter().enumerate() {
        let target = tr.line_entry.get(k + 1).copied().unwrap_or(s_len - 1);
        if target != entry {
            skip_from[target] = entry;
            skip_cost[target] = -SKIP_COST_PER_TOKEN * tr.line_tokens[k] as f32;
        }
    }
    let can_jump: Vec<bool> = (0..s_len).map(|s| s >= 2 && s % 2 == 1 && tr.labels[s] != tr.labels[s - 2]).collect();

    let mut back = vec![STAY; t_len * s_len];
    let mut score = vec![NEG; s_len];
    score[0] = emit(0, 0);
    if s_len > 1 {
        score[1] = emit(0, 1);
    }
    // Skipping leading lines can happen before the first frame too.
    for s in 0..s_len {
        if skip_from[s] != usize::MAX && score[skip_from[s]] > NEG {
            let via = score[skip_from[s]] + skip_cost[s];
            if via > score[s] {
                score[s] = via;
                back[s] = SKIP_LINE;
            }
        }
    }
    let mut next = vec![NEG; s_len];
    for t in 1..t_len {
        let row = &mut back[t * s_len..(t + 1) * s_len];
        for s in 0..s_len {
            let mut best = score[s];
            let mut code = STAY;
            if s >= 1 && score[s - 1] > best {
                best = score[s - 1];
                code = STEP;
            }
            if can_jump[s] && score[s - 2] > best {
                best = score[s - 2];
                code = JUMP;
            }
            if skip_from[s] != usize::MAX {
                let via = score[skip_from[s]] + skip_cost[s];
                if via > best {
                    best = via;
                    code = SKIP_LINE;
                }
            }
            next[s] = if best > NEG { best + emit(t, s) } else { NEG };
            row[s] = code;
        }
        std::mem::swap(&mut score, &mut next);
    }
    let mut s = if s_len > 1 && score[s_len - 2] > score[s_len - 1] { s_len - 2 } else { s_len - 1 };
    let mut path = vec![0; t_len];
    for t in (0..t_len).rev() {
        path[t] = s;
        // A line skipped before the first frame leaves the start state behind.
        loop {
            let code = back[t * s_len + s];
            let prev = match code {
                STAY => s,
                STEP => s - 1,
                JUMP => s - 2,
                _ => skip_from[s],
            };
            if t == 0 {
                if code == SKIP_LINE {
                    s = prev;
                    continue;
                }
                break;
            }
            s = prev;
            break;
        }
    }
    path
}

/// Align `lines` (the user's lyrics, one per entry) to the emissions.
pub fn align(lines: &[String], em: &Emissions, vocab: &Vocab, duration: f64, model: &str) -> Result<AlignmentResult, String> {
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err("Paste or import the lyrics first".into());
    }
    if em.frames == 0 || em.classes == 0 || em.logp.len() != em.frames * em.classes {
        return Err("The alignment model returned no usable output".into());
    }
    let words = normalize(lines, vocab);
    let tr = build_trellis(&words, vocab);
    if tr.labels.len() <= 1 {
        return Err("None of these lyrics can be matched by the English alignment model".into());
    }
    let needed = tr.labels.len() / 2;
    if needed > em.frames {
        return Err(format!(
            "The lyrics are longer than the singing ({needed} letters for {:.0} s of audio). Remove lines that aren't sung.",
            em.frames as f64 * em.stride
        ));
    }
    let path = viterbi(em, &tr);

    // Frames per word: (first, last) token frame, and the letter probabilities.
    let mut first = vec![usize::MAX; words.len()];
    let mut last = vec![0usize; words.len()];
    let mut probs: Vec<Vec<f32>> = vec![Vec::new(); words.len()];
    // Where each token state was first entered (to know which were skipped).
    for (t, &s) in path.iter().enumerate() {
        if let Some(wi) = tr.owner[s] {
            first[wi] = first[wi].min(t);
            last[wi] = last[wi].max(t);
            probs[wi].push(em.at(t, tr.labels[s]).exp());
        }
    }
    // Token frames of all matched words, in order: a word ends where the
    // singing of its last letter stops — at the next word's first letter,
    // but no more than `HOLD` after its last letter.
    const HOLD: f64 = 1.0;
    let starts: Vec<usize> = first.iter().copied().filter(|&f| f != usize::MAX).collect();
    let audio_end = (em.frames as f64 * em.stride).min(duration.max(0.0));
    let clamp = |x: f64| x.clamp(0.0, audio_end);
    let mut timed: Vec<Option<(f64, f64, f32)>> = vec![None; words.len()];
    for wi in 0..words.len() {
        if first[wi] == usize::MAX {
            continue;
        }
        let start = clamp(first[wi] as f64 * em.stride);
        let next_start = starts.iter().find(|&&f| f > last[wi]).map_or(audio_end, |&f| f as f64 * em.stride);
        let end = clamp(((last[wi] + 1) as f64 * em.stride + HOLD).min(next_start)).max(start);
        let conf = probs[wi].iter().sum::<f32>() / probs[wi].len().max(1) as f32;
        timed[wi] = Some((start, end, conf));
    }

    // Lines.
    let mut result = Vec::with_capacity(lines.len());
    let mut wi = 0;
    for (li, text) in lines.iter().enumerate() {
        let line_words: Vec<usize> = (wi..words.len()).take_while(|&k| words[k].line == li).collect();
        wi += line_words.len();
        let any_tokens = line_words.iter().any(|&k| !words[k].tokens.is_empty());
        let matched: Vec<usize> = line_words.iter().copied().filter(|&k| timed[k].is_some()).collect();
        let status = if !matched.is_empty() {
            LineStatus::Aligned
        } else if any_tokens {
            LineStatus::NotFound
        } else {
            LineStatus::Unmatchable
        };
        let mut out_words = Vec::new();
        let (mut start, mut end, mut confidence) = (0.0, 0.0, 0.0);
        if status == LineStatus::Aligned {
            start = timed[matched[0]].unwrap().0;
            end = timed[*matched.last().unwrap()].unwrap().1;
            confidence = matched.iter().map(|&k| timed[k].unwrap().2).sum::<f32>() / matched.len() as f32;
            // Unmatched words sit between their matched neighbours.
            let mut prev_end = start;
            for (n, &k) in line_words.iter().enumerate() {
                let (s, e, c) = match timed[k] {
                    Some((s, e, c)) => (s, e, Some(c)),
                    None => {
                        let next = line_words[n + 1..].iter().find_map(|&j| timed[j].map(|t| t.0)).unwrap_or(end);
                        (prev_end, next.max(prev_end), None)
                    }
                };
                prev_end = e;
                out_words.push(ResultWord { text: words[k].original.clone(), start: s, end: e, confidence: c });
            }
        }
        result.push(ResultLine { index: li, text: text.clone(), start, end, confidence, status, words: out_words });
    }
    if result.iter().all(|l| l.status != LineStatus::Aligned) {
        return Err("Couldn't match these lyrics to the singing. Check they're the right lyrics, in English.".into());
    }
    place_unaligned_lines(&mut result, audio_end);
    // Millisecond precision: plenty for lyrics, and stable through JSON.
    let ms = |x: &mut f64| *x = (*x * 1000.0).round() / 1000.0;
    for l in &mut result {
        ms(&mut l.start);
        ms(&mut l.end);
        for w in &mut l.words {
            ms(&mut w.start);
            ms(&mut w.end);
        }
    }
    Ok(AlignmentResult { version: RESULT_VERSION, model: model.to_owned(), language: "en".into(), duration: audio_end, lines: result })
}

/// Lines without a match go in the gap between their aligned neighbours,
/// sharing it evenly (flagged; no word timing).
fn place_unaligned_lines(lines: &mut [ResultLine], audio_end: f64) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].status == LineStatus::Aligned {
            i += 1;
            continue;
        }
        let j = (i..lines.len()).find(|&k| lines[k].status == LineStatus::Aligned).unwrap_or(lines.len());
        let lo = if i > 0 { lines[i - 1].end } else { 0.0 };
        let hi = lines.get(j).map_or(audio_end, |l| l.start).max(lo);
        let n = (j - i) as f64;
        for (k, line) in lines[i..j].iter_mut().enumerate() {
            line.start = lo + (hi - lo) * k as f64 / n;
            line.end = lo + (hi - lo) * (k + 1) as f64 / n;
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wav2vec2-base-960h vocabulary.
    fn vocab() -> Vocab {
        let json = r#"{"'": 27, "</s>": 2, "<pad>": 0, "<s>": 1, "<unk>": 3, "A": 7, "B": 24, "C": 19, "D": 14, "E": 5, "F": 20, "G": 21, "H": 11, "I": 10, "J": 29, "K": 26, "L": 15, "M": 17, "N": 9, "O": 8, "P": 23, "Q": 30, "R": 13, "S": 12, "T": 6, "U": 16, "V": 25, "W": 18, "X": 28, "Y": 22, "Z": 31, "|": 4}"#;
        Vocab::from_map(&serde_json::from_str(json).unwrap()).unwrap()
    }

    /// Emissions where each `(frame, text)` token is "heard" on that frame
    /// (consecutive letters on consecutive frames), blanks everywhere else.
    fn sung(frames: usize, at: &[(usize, &str)]) -> Emissions {
        let v = vocab();
        let classes = 32;
        let mut logp = vec![(0.01f32).ln(); frames * classes];
        for t in 0..frames {
            logp[t * classes + v.blank] = (0.9f32).ln();
        }
        for &(start, text) in at {
            let mut t = start;
            for (k, w) in text.split(' ').enumerate() {
                if k > 0 {
                    logp[t * classes + v.blank] = (0.01f32).ln();
                    logp[t * classes + v.sep.unwrap()] = (0.9f32).ln();
                    t += 1;
                }
                for c in spoken_form(w).chars() {
                    let id = v.chars[&c];
                    logp[t * classes + v.blank] = (0.01f32).ln();
                    logp[t * classes + id] = (0.9f32).ln();
                    t += 2; // letter, then a blank frame
                }
            }
        }
        Emissions { frames, classes, stride: 0.02, logp }
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn words_and_lines_land_where_they_are_sung() {
        // "HERE COMES" at frame 100 (2.0 s), "THE SUN" at frame 400 (8.0 s).
        let em = sung(600, &[(100, "here comes"), (400, "the sun")]);
        let r = align(&lines(&["Here comes,", "the sun!"]), &em, &vocab(), 12.0, "test").unwrap();
        assert_eq!(r.version, 1);
        assert_eq!(r.lines.len(), 2);
        let w = &r.lines[0].words;
        assert_eq!(w.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Here", "comes,"], "original text kept");
        assert!((w[0].start - 2.0).abs() < 1e-9, "{}", w[0].start);
        // "HERE" = 4 letters on frames 100,102,104,106; separator at 108; COMES from 109.
        assert!((w[1].start - 109.0 * 0.02).abs() < 1e-9, "{}", w[1].start);
        assert!((r.lines[1].start - 8.0).abs() < 1e-9);
        assert_eq!(r.lines[0].status, LineStatus::Aligned);
        assert!(r.lines[0].confidence > 0.8);
        for l in &r.lines {
            assert!(l.start <= l.end && l.end <= 12.0);
            for w in &l.words {
                assert!(w.start <= w.end && w.start >= l.start - 1e-9 && w.end <= l.end + 1e-9);
            }
        }
        // A word ends at most a second after its last letter.
        assert!(w[1].end <= (109.0 + 9.0) * 0.02 + 1.0 + 1e-9);
    }

    #[test]
    fn repeated_chorus_lines_keep_their_own_times() {
        let em = sung(1000, &[(50, "oh oh"), (300, "glory"), (600, "oh oh"), (800, "glory")]);
        let r = align(&lines(&["Oh oh", "Glory", "Oh oh", "Glory"]), &em, &vocab(), 20.0, "t").unwrap();
        let starts: Vec<f64> = r.lines.iter().map(|l| (l.start * 50.0).round()).collect();
        assert_eq!(starts, vec![50.0, 300.0, 600.0, 800.0]);
    }

    #[test]
    fn an_unsung_line_is_skipped_not_crammed_in() {
        // Line 2 is in the lyrics but never sung.
        let em = sung(900, &[(100, "you are holy"), (500, "worthy is the lamb")]);
        let r = align(&lines(&["You are holy", "This verse is not sung at all", "Worthy is the Lamb"]), &em, &vocab(), 18.0, "t").unwrap();
        assert_eq!(r.lines[1].status, LineStatus::NotFound);
        assert!(r.lines[1].words.is_empty());
        assert_eq!(r.lines[0].status, LineStatus::Aligned);
        assert_eq!(r.lines[2].status, LineStatus::Aligned);
        assert!((r.lines[0].start - 2.0).abs() < 1e-9);
        assert!((r.lines[2].start - 10.0).abs() < 1e-9);
        // Placed in the gap between its neighbours.
        assert!(r.lines[1].start >= r.lines[0].end && r.lines[1].end <= r.lines[2].start);
    }

    #[test]
    fn a_first_line_that_is_not_sung_is_skipped_too() {
        let em = sung(600, &[(200, "amazing grace")]);
        let r = align(&lines(&["Intro words nobody sings here", "Amazing grace"]), &em, &vocab(), 12.0, "t").unwrap();
        assert_eq!(r.lines[0].status, LineStatus::NotFound);
        assert!((r.lines[1].start - 4.0).abs() < 1e-9);
    }

    #[test]
    fn text_normalisation() {
        assert_eq!(spoken_form("Don’t"), "DON'T");
        assert_eq!(spoken_form("'cause"), "CAUSE");
        assert_eq!(spoken_form("30"), "THIRTY");
        assert_eq!(spoken_form("1999,"), "ONE THOUSAND NINE HUNDRED NINETY NINE");
        assert_eq!(spoken_form("café"), "CAFE");
        assert_eq!(spoken_form("&"), "AND");
        assert_eq!(spoken_form("well-known"), "WELL KNOWN");
        assert_eq!(spoken_form("(Chorus)"), "CHORUS");
        assert_eq!(number_words("0"), "ZERO");
        assert_eq!(number_words("115"), "ONE HUNDRED FIFTEEN");
        assert_eq!(number_words("007"), "ZERO ZERO SEVEN");

        let words = normalize(&lines(&["Ana mipango (He has plans) 30", "Ὦ θεέ"]), &vocab());
        let has: Vec<(&str, bool)> = words.iter().map(|w| (w.original.as_str(), !w.tokens.is_empty())).collect();
        assert_eq!(
            has,
            vec![("Ana", true), ("mipango", true), ("(He", false), ("has", false), ("plans)", false), ("30", true), ("Ὦ", false), ("θεέ", false)]
        );
    }

    #[test]
    fn unmatchable_words_and_lines_are_placed_between_neighbours() {
        let em = sung(700, &[(100, "holy holy"), (500, "amen")]);
        let r = align(&lines(&["Holy (Mtakatifu) holy", "Ὦ θεέ", "Amen"]), &em, &vocab(), 14.0, "t").unwrap();
        let w = &r.lines[0].words;
        assert_eq!(w.len(), 3);
        assert!(w[1].confidence.is_none());
        assert!(w[1].start >= w[0].end - 1e-9 && w[1].end <= w[2].start + 1e-9);
        assert_eq!(r.lines[1].status, LineStatus::Unmatchable);
        assert!(r.lines[1].start >= r.lines[0].end && r.lines[1].end <= r.lines[2].start);
    }

    #[test]
    fn errors_instead_of_made_up_times() {
        let em = sung(20, &[]);
        assert!(align(&lines(&["", "  "]), &em, &vocab(), 1.0, "t").unwrap_err().contains("lyrics first"));
        let long = "a very long line of lyrics that cannot possibly fit into twenty frames";
        assert!(align(&lines(&[long]), &em, &vocab(), 0.4, "t").unwrap_err().contains("longer than the singing"));
        assert!(align(&lines(&["Ὦ θεέ"]), &em, &vocab(), 0.4, "t").is_err());
    }

    #[test]
    fn result_json_round_trips_and_feeds_the_editor() {
        let em = sung(300, &[(50, "here comes the sun")]);
        let r = align(&lines(&["Here comes the sun"]), &em, &vocab(), 6.0, "wav2vec2-base-960h-int8").unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"version\":1") && json.contains("\"status\":\"aligned\""));
        let back: AlignmentResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        let l = &r.lyric_lines()[0];
        assert_eq!(l.words.len(), 4);
        assert_eq!(l.text, "Here comes the sun");
        assert!(l.words.windows(2).all(|p| p[0].start <= p[1].start));
    }

    /// Quality check on real data: `STAGEWEAVE_EM=emissions.f32 STAGEWEAVE_VOCAB=vocab.json
    /// STAGEWEAVE_LYRICS=lyrics.txt STAGEWEAVE_REF=lyric_lines.json cargo test eval_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn eval_real() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let bytes = std::fs::read(var("STAGEWEAVE_EM")).unwrap();
        let logp: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let em = Emissions { frames: logp.len() / 32, classes: 32, stride: 0.02, logp };
        let vocab = Vocab::from_map(&serde_json::from_str(&std::fs::read_to_string(var("STAGEWEAVE_VOCAB")).unwrap()).unwrap()).unwrap();
        let lines: Vec<String> =
            std::fs::read_to_string(var("STAGEWEAVE_LYRICS")).unwrap().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        let reference: Vec<LyricLine> = serde_json::from_str(&std::fs::read_to_string(var("STAGEWEAVE_REF")).unwrap()).unwrap();
        let t0 = std::time::Instant::now();
        let r = align(&lines, &em, &vocab, em.frames as f64 * 0.02, "eval").unwrap();
        println!("align {:.2} s for {} frames", t0.elapsed().as_secs_f64(), em.frames);
        let mut errs = Vec::new();
        for (l, rf) in r.lines.iter().zip(&reference) {
            let err = (l.start - rf.start).abs();
            if l.status == LineStatus::Aligned {
                errs.push(err);
            }
            println!("{:7.2} (ref {:7.2}, Δ {:5.2})  {:.2} {:?}  {}", l.start, rf.start, err, l.confidence, l.status, l.text);
        }
        errs.sort_by(f64::total_cmp);
        let within = |s: f64| errs.iter().filter(|&&e| e <= s).count() as f64 / errs.len() as f64 * 100.0;
        println!("lines {} aligned {} · median Δ {:.2} s · ≤0.5 s {:.0}% · ≤1 s {:.0}%", r.lines.len(), errs.len(), errs[errs.len() / 2], within(0.5), within(1.0));
    }
}
