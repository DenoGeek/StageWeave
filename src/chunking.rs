//! Cut the vocals stem into short pieces for transcription.
//!
//! Pieces are built from the singer's phrases (`align::detect_phrases`), so a
//! cut always falls in a silent gap and never splits a word. Silence and
//! instrumental sections are left out entirely, so there's nothing for
//! Whisper to hallucinate over.

use crate::align::detect_phrases;
use crate::model::Peaks;

/// Aim for pieces at least this long (merging short phrases)…
pub const MIN_PIECE: f64 = 4.0;
/// …and no longer than this when grouping phrases…
pub const TARGET_MAX: f64 = 20.0;
/// …and never longer than this (single long phrases get cut).
pub const HARD_MAX: f64 = 25.0;
/// Extend each piece into the surrounding gap by up to this much.
pub const PAD: f64 = 0.3;

#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    /// Audio sent for this piece (padded), in song time.
    pub start: f64,
    pub end: f64,
    /// The sung phrases inside it, in song time (unpadded).
    pub phrases: Vec<(f64, f64)>,
}

impl Piece {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// Pieces for the whole song from the vocals envelope.
pub fn build_pieces(vocals: &Peaks, duration: f64) -> Vec<Piece> {
    let phrases: Vec<(f64, f64)> =
        detect_phrases(vocals, 0.0, duration).into_iter().flat_map(|p| split_long(p, vocals)).collect();
    pad(group(&phrases), duration)
}

/// Seconds of singing that will be sent.
pub fn sung_seconds(pieces: &[Piece]) -> f64 {
    pieces.iter().map(Piece::len).sum()
}

/// Cut a phrase longer than `HARD_MAX` at its quietest half-second.
fn split_long((start, end): (f64, f64), peaks: &Peaks) -> Vec<(f64, f64)> {
    if end - start <= HARD_MAX {
        return vec![(start, end)];
    }
    let (from, to, window) = (start + HARD_MAX * 0.4, start + HARD_MAX - 0.5, 0.5);
    let mut best = (f32::MAX, start + HARD_MAX * 0.8);
    let mut t = from;
    while t + window <= to {
        let e = peaks.max_in(t, t + window);
        if e < best.0 {
            best = (e, t + window / 2.0);
        }
        t += 0.05;
    }
    let mut out = vec![(start, best.1)];
    out.extend(split_long((best.1, end), peaks));
    out
}

/// Join consecutive phrases into groups of `MIN_PIECE..=TARGET_MAX` seconds.
/// Among the places a group could end, pick the one followed by the longest
/// silence: the safest cut. (Up to `HARD_MAX` if that's the only way to reach
/// `MIN_PIECE`.)
fn group(phrases: &[(f64, f64)]) -> Vec<Vec<(f64, f64)>> {
    let gap_after = |j: usize| phrases.get(j + 1).map_or(f64::INFINITY, |n| n.0 - phrases[j].1);
    let mut groups = Vec::new();
    let mut i = 0;
    while i < phrases.len() {
        let start = phrases[i].0;
        let mut best: Option<(f64, usize)> = None; // (gap, end index) among spans ≥ MIN_PIECE
        let mut fallback = i; // longest span within the hard limit
        for (j, &(_, end)) in phrases.iter().enumerate().skip(i) {
            let span = end - start;
            if j > i && span > HARD_MAX {
                break;
            }
            fallback = j;
            if span > TARGET_MAX && j > i {
                break;
            }
            if span >= MIN_PIECE && best.is_none_or(|(g, _)| gap_after(j) > g) {
                best = Some((gap_after(j), j));
            }
        }
        let end = best.map_or(fallback, |(_, j)| j);
        groups.push(phrases[i..=end].to_vec());
        i = end + 1;
    }
    groups
}

/// Extend each group into its gaps (≤ PAD and ≤ half the gap) so neighbours never overlap.
fn pad(groups: Vec<Vec<(f64, f64)>>, duration: f64) -> Vec<Piece> {
    let bounds: Vec<(f64, f64)> = groups.iter().map(|g| (g[0].0, g.last().unwrap().1)).collect();
    groups
        .into_iter()
        .enumerate()
        .map(|(k, phrases)| {
            let (s, e) = bounds[k];
            let before = if k == 0 { s } else { (s - bounds[k - 1].1) / 2.0 };
            let after = if k + 1 == bounds.len() { duration - e } else { (bounds[k + 1].0 - e) / 2.0 };
            Piece { start: (s - before.min(PAD)).max(0.0), end: (e + after.min(PAD)).min(duration), phrases }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Envelope at 100/s with singing in `spans`.
    fn env(spans: &[(f64, f64)], dur: f64) -> Peaks {
        let mut data = vec![0.0f32; (dur * 100.0) as usize];
        for (a, b) in spans {
            data[(a * 100.0) as usize..(b * 100.0) as usize].iter_mut().for_each(|v| *v = 0.8);
        }
        Peaks { per_sec: 100.0, data }
    }

    #[test]
    fn pieces_end_in_gaps_never_overlap_and_skip_silence() {
        // Intro silence 0-12, verse phrases, instrumental 60-90, more phrases.
        let mut spans = Vec::new();
        let mut t = 12.0;
        while t < 58.0 {
            spans.push((t, t + 3.0));
            t += 4.0;
        }
        t = 90.0;
        while t < 130.0 {
            spans.push((t, t + 3.5));
            t += 4.5;
        }
        let pieces = build_pieces(&env(&spans, 150.0), 150.0);
        assert!(pieces.len() >= 4);
        assert!(pieces[0].start >= 11.6, "intro not sent: {:?}", pieces[0]);
        assert!(pieces.iter().all(|p| !(p.start < 85.0 && p.end > 65.0)), "instrumental not sent");
        for w in pieces.windows(2) {
            assert!(w[0].end <= w[1].start + 1e-9, "no overlap: {:?}", w);
        }
        for p in &pieces {
            assert!(p.len() <= HARD_MAX + 2.0 * PAD + 1e-9, "{p:?}");
            // Every edge lies in silence (no word clipped).
            let first = p.phrases[0];
            let last = *p.phrases.last().unwrap();
            assert!(p.start <= first.0 + 0.05 && p.end >= last.1 - 0.05, "{p:?}");
        }
    }

    #[test]
    fn padding_is_at_most_half_the_gap() {
        // Two groups forced apart by length: phrases 0.4 s apart at the boundary.
        let spans: Vec<(f64, f64)> = (0..12).map(|i| (1.0 + i as f64 * 3.4, 1.0 + i as f64 * 3.4 + 3.0)).collect();
        let pieces = build_pieces(&env(&spans, 50.0), 50.0);
        assert!(pieces.len() >= 2);
        let gap_mid = (pieces[0].phrases.last().unwrap().1 + pieces[1].phrases[0].0) / 2.0;
        assert!(pieces[0].end <= gap_mid + 1e-6 && pieces[1].start >= gap_mid - 1e-6);
    }

    #[test]
    fn short_phrases_join_and_long_phrases_get_cut() {
        // A 40 s run with a quiet dip at 21 s, then two tiny phrases.
        let mut e = env(&[(5.0, 45.0), (50.0, 50.8), (51.5, 52.3)], 60.0);
        for v in &mut e.data[2100..2160] {
            *v = 0.05;
        }
        let pieces = build_pieces(&e, 60.0);
        assert!(pieces.iter().all(|p| p.len() <= HARD_MAX + 2.0 * PAD + 1e-9), "{pieces:?}");
        let cut = pieces[0].end;
        assert!((cut - 21.3).abs() < 0.6, "long phrase cut in its quiet dip: {pieces:?}");
        let tail = pieces.last().unwrap();
        assert_eq!(tail.phrases.len(), 2, "tiny phrases joined: {pieces:?}");
    }

    #[test]
    fn group_never_exceeds_target_unless_needed() {
        let phrases: Vec<(f64, f64)> = (0..10).map(|i| (i as f64 * 5.0, i as f64 * 5.0 + 4.0)).collect();
        for g in group(&phrases) {
            assert!(g.last().unwrap().1 - g[0].0 <= TARGET_MAX + 1e-9);
        }
    }
}

#[cfg(test)]
mod real_audio {
    use super::*;

    /// `MUSICKIT_VOCALS=…/vocals.wav cargo test real_vocals_pieces -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_vocals_pieces() {
        let path = std::env::var("MUSICKIT_VOCALS").expect("set MUSICKIT_VOCALS");
        let (audio, _) = crate::engine::decode_file_for_tests(std::path::Path::new(&path));
        let peaks = audio.peaks(crate::engine::PEAKS_PER_SEC);
        let pieces = build_pieces(&peaks, audio.duration());
        let lens: Vec<f64> = pieces.iter().map(Piece::len).collect();
        let (min, max) = lens.iter().fold((f64::MAX, 0.0f64), |(a, b), &l| (a.min(l), b.max(l)));
        // Loudest moment at each cut (should be near-silent compared with the singing).
        let loud_at_cuts = pieces.windows(2).map(|w| peaks.max_in(w[0].end - 0.05, w[1].start + 0.05)).fold(0.0f32, f32::max);
        let gaps: Vec<f64> = pieces.windows(2).map(|w| w[1].phrases[0].0 - w[0].phrases.last().unwrap().1).collect();
        println!("gaps at cuts: min {:.2}s, median {:.2}s", gaps.iter().copied().fold(f64::MAX, f64::min), { let mut g = gaps.clone(); g.sort_by(f64::total_cmp); g[g.len() / 2] });
        let typical = { let mut d = peaks.data.clone(); d.sort_by(f32::total_cmp); d[d.len() * 9 / 10] };
        println!(
            "{:.0}s song → {} pieces, {:.0}s of singing sent ({:.0}%); lengths {:.1}–{:.1}s; loudest cut {:.3} vs typical singing {:.3}",
            audio.duration(), pieces.len(), sung_seconds(&pieces), sung_seconds(&pieces) / audio.duration() * 100.0,
            min, max, loud_at_cuts, typical
        );
        for p in pieces.iter().take(6) {
            println!("  {:6.1}–{:6.1}  ({} phrases)", p.start, p.end, p.phrases.len());
        }
        assert!(pieces.windows(2).all(|w| w[0].end <= w[1].start + 1e-9));
        assert!(max <= HARD_MAX + 2.0 * PAD + 1e-9);
    }
}
