//! Mock backends used by `--demo` and as fallbacks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::model::{AudioBuffer, AudioInfo, Peaks, SourceAudio, StemKind};
use std::sync::Arc;

use super::{
    AudioDecoder, Engine, PEAKS_PER_SEC, PlaybackEngine, SUPPORTED_EXTENSIONS, SplitModel, StemOutput,
    StemSplitter, is_supported,
};

/// Tiny deterministic PRNG so mock waveforms are stable per file.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn seed_for(path: &Path) -> u64 {
    path.to_string_lossy()
        .bytes()
        .fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
}

const MOCK_BPM: f64 = 112.0;

/// Song sections (verse/chorus) shape the mock envelopes so they look musical.
fn section_energy(t: f64, duration: f64) -> f32 {
    let p = t / duration;
    let base = if p < 0.06 {
        0.35
    } else if (0.28..0.42).contains(&p) || (0.62..0.78).contains(&p) || p > 0.86 && p < 0.96 {
        1.0
    } else if p >= 0.96 {
        0.4 * (1.0 - (p - 0.96) / 0.04) as f32
    } else {
        0.7
    };
    base.clamp(0.0, 1.0)
}

fn mock_peaks(kind: Option<StemKind>, duration: f64, seed: u64) -> Peaks {
    let n = (duration * PEAKS_PER_SEC as f64) as usize;
    let mut rng = Rng::new(seed ^ kind.map_or(0, |k| k as u64 * 7919));
    let beat = 60.0 / MOCK_BPM;
    let mut data = Vec::with_capacity(n);
    let mut smooth = 0.0f32;
    for i in 0..n {
        let t = i as f64 / PEAKS_PER_SEC as f64;
        let e = section_energy(t, duration);
        let phase = (t % beat) / beat;
        let r = rng.next();
        let v = match kind {
            Some(StemKind::Drums) => {
                let half = (t % (beat / 2.0)) / (beat / 2.0);
                let kick = (-(phase * 14.0)).exp() as f32;
                let hat = 0.35 * (-(half * 22.0)).exp() as f32;
                (kick + hat + 0.08 * r) * e
            }
            Some(StemKind::Bass) => {
                let pulse = 0.55 + 0.45 * (-(phase * 4.0)).exp() as f32;
                pulse * e * (0.85 + 0.15 * r)
            }
            Some(StemKind::Vocals) => {
                // Phrases of ~4 bars with breaths in between, silent intro.
                let bar = beat * 4.0;
                let phrase = (t / (bar * 2.0)).fract();
                let singing = t > duration * 0.06 && phrase < 0.82 && t < duration * 0.95;
                let syll = 0.6 + 0.4 * ((t * 5.3).sin() * 0.5 + 0.5) as f32;
                if singing { syll * e * (0.75 + 0.25 * r) } else { 0.02 * r }
            }
            Some(StemKind::Piano) | Some(StemKind::Guitar) => {
                let chord = (-(((t % (beat * 2.0)) / (beat * 2.0)) * 3.0)).exp() as f32;
                (0.3 + 0.6 * chord) * e * (0.8 + 0.2 * r)
            }
            Some(StemKind::Other) | Some(StemKind::Instrumental) => {
                (0.45 + 0.25 * (-(phase * 6.0)).exp() as f32) * e * (0.8 + 0.2 * r)
            }
            None => {
                let kick = (-(phase * 10.0)).exp() as f32;
                (0.5 + 0.4 * kick) * e * (0.85 + 0.15 * r)
            }
        };
        // Light smoothing so waveforms read nicely.
        smooth = smooth * 0.35 + v * 0.65;
        data.push(smooth.clamp(0.0, 1.0));
    }
    Peaks { per_sec: PEAKS_PER_SEC, data }
}

pub struct MockDecoder;

impl AudioDecoder for MockDecoder {
    fn load(&self, path: &Path) -> Result<SourceAudio, String> {
        if !is_supported(path) {
            return Err(format!(
                "Unsupported file type. Try one of: {}",
                SUPPORTED_EXTENSIONS.join(", ")
            ));
        }
        let seed = seed_for(path);
        let duration = 170.0 + (seed % 90) as f64 + 0.37;
        let format = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        Ok(SourceAudio {
            info: AudioInfo {
                file_name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                format,
                duration,
                sample_rate: 44_100,
                channels: 2,
            },
            peaks: mock_peaks(None, duration, seed),
            audio: None,
        })
    }
}

pub struct MockSplitter;

impl StemSplitter for MockSplitter {
    fn models(&self) -> Vec<SplitModel> {
        use StemKind::*;
        vec![
            SplitModel {
                id: "htdemucs".into(),
                engine: Engine::Mock,
                tag: "Preview".into(),
                name: "4 Stems".into(),
                description: "Vocals · Drums · Bass · Other".into(),
                stems: vec![Vocals, Drums, Bass, Other],
            },
            SplitModel {
                id: "htdemucs_6s".into(),
                engine: Engine::Mock,
                tag: "Preview".into(),
                name: "6 Stems".into(),
                description: "Adds Guitar and Piano".into(),
                stems: vec![Vocals, Drums, Bass, Guitar, Piano, Other],
            },
            SplitModel {
                id: "karaoke".into(),
                engine: Engine::Mock,
                tag: "Preview".into(),
                name: "2 Stems · Karaoke".into(),
                description: "Vocals · Instrumental".into(),
                stems: vec![Vocals, Instrumental],
            },
        ]
    }

    fn split(
        &self,
        source: &Path,
        model: &SplitModel,
        _out_dir: &Path,
        progress: &dyn Fn(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<Vec<StemOutput>, String> {
        let mut stages: Vec<String> = vec!["Loading model".into(), "Decoding audio".into()];
        stages.extend(model.stems.iter().map(|k| format!("Separating {}", k.label().to_lowercase())));
        stages.push("Writing stems".into());

        let steps_per_stage = 12;
        let total = stages.len() * steps_per_stage;
        for (si, stage) in stages.iter().enumerate() {
            for step in 0..steps_per_stage {
                if cancel.load(Ordering::Relaxed) {
                    return Err("Cancelled".into());
                }
                let done = si * steps_per_stage + step;
                progress(done as f32 / total as f32, stage);
                std::thread::sleep(Duration::from_millis(45));
            }
        }
        progress(1.0, "Done");

        let duration = MockDecoder.load(source).map_or(200.0, |s| s.info.duration);
        let seed = seed_for(source);
        Ok(model
            .stems
            .iter()
            .map(|&kind| StemOutput {
                kind,
                peaks: mock_peaks(Some(kind), duration, seed),
                path: None,
                audio: None,
            })
            .collect())
    }
}

/// Wall-clock transport with no audio output.
pub struct MockPlayback {
    playing_since: Option<Instant>,
    offset: f64,
    region: Option<(f64, f64)>,
}

impl MockPlayback {
    pub fn new() -> Self {
        Self { playing_since: None, offset: 0.0, region: None }
    }
}

impl PlaybackEngine for MockPlayback {
    fn play(&mut self) {
        if self.playing_since.is_none() {
            self.playing_since = Some(Instant::now());
        }
    }
    fn pause(&mut self) {
        self.offset = self.position();
        self.playing_since = None;
    }
    fn is_playing(&self) -> bool {
        self.playing_since.is_some()
    }
    fn seek(&mut self, seconds: f64) {
        self.offset = seconds.max(0.0);
        if self.playing_since.is_some() {
            self.playing_since = Some(Instant::now());
        }
    }
    fn position(&self) -> f64 {
        let t = self.offset + self.playing_since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match self.region {
            Some((a, b)) if self.offset < b && t >= b => a + (t - b) % (b - a),
            _ => t,
        }
    }
    fn set_loop(&mut self, region: Option<(f64, f64)>) {
        self.region = region;
    }
    fn set_tracks(&mut self, _tracks: Vec<Arc<AudioBuffer>>) {}
    fn set_mix(&mut self, _gains: &[f32], _pans: &[f32], _master: f32) {}
    fn set_sections(&mut self, _sections: &[super::SectionMix], _master: f32) {}
    fn describe(&self) -> String {
        "Silent preview clock".into()
    }
}

