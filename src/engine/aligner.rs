use crate::model::{LyricLine, Peaks};

use super::LyricAligner;

/// Spreads lines across the vocal regions (or evenly if no vocals stem).
pub struct NaiveAligner;

impl LyricAligner for NaiveAligner {
    fn align(&self, lines: &[String], duration: f64, vocals: Option<&Peaks>) -> Vec<LyricLine> {
        if lines.is_empty() || duration <= 0.0 {
            return Vec::new();
        }
        // Find the first and last moment the vocals are clearly audible.
        let (mut start, mut end) = (duration * 0.06, duration * 0.95);
        if let Some(v) = vocals {
            let ps = v.per_sec as f64;
            if let Some(i) = v.data.iter().position(|&a| a > 0.2) {
                start = i as f64 / ps;
            }
            if let Some(i) = v.data.iter().rposition(|&a| a > 0.2) {
                end = i as f64 / ps;
            }
        }
        let span = (end - start).max(1.0);
        let weights: Vec<f64> = lines.iter().map(|l| 2.0 + l.chars().count() as f64 * 0.1).collect();
        let total: f64 = weights.iter().sum();
        let mut t = start;
        lines
            .iter()
            .zip(weights)
            .map(|(text, w)| {
                let len = span * w / total;
                let line = LyricLine { text: text.clone(), start: t, end: t + len };
                t += len;
                line
            })
            .collect()
    }
}
