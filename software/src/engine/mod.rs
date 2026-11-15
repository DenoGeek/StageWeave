//! Backend seams. The UI only talks to these traits; real implementations
//! (symphonia decoder, cpal playback, Python separation worker) and the mocks
//! used by `--demo` live in the submodules.

mod aligner;
mod decoder;
pub mod ffmpeg;
pub mod local_align;
pub mod mock;
pub mod models;
mod playback;
pub mod python_env;
mod python_splitter;
pub mod queue;
pub mod worker;
pub mod ytdlp;

pub use aligner::NaiveAligner;
pub use decoder::SymphoniaDecoder;
#[cfg(test)]
pub fn decode_file_for_tests(p: &Path) -> (AudioBuffer, u16) {
    decoder::decode_file(p).expect("decode")
}
pub use playback::{CpalPlayback, pan_gains};
pub use python_splitter::PythonSplitter;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, channel};

use crate::model::{AudioBuffer, LyricLine, Peaks, SourceAudio, StemKind};

pub const SUPPORTED_EXTENSIONS: &[&str] = &["wav", "mp3", "flac", "ogg", "m4a", "aac", "aif", "aiff"];

/// Resolution of waveform / meter envelopes.
pub const PEAKS_PER_SEC: f32 = 100.0;

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Formats the Python worker reads itself; others are handed to it as WAV.
pub fn worker_reads(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["wav", "mp3", "flac", "ogg"].contains(&e.to_ascii_lowercase().as_str()))
}

/// Write decoded audio as a 16-bit PCM WAV, stereo or mixed down to mono.
pub fn write_wav(path: &Path, audio: &AudioBuffer, mono: bool) -> std::io::Result<()> {
    let to_i16 = |v: f32| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes();
    let pcm: Vec<u8> = if mono {
        audio.data.chunks_exact(2).flat_map(|f| to_i16((f[0] + f[1]) * 0.5)).collect()
    } else {
        audio.data.iter().flat_map(|v| to_i16(*v)).collect()
    };
    let channels: u16 = if mono { 1 } else { 2 };
    let rate = audio.sample_rate;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    std::fs::write(path, wav)
}

// ---------------------------------------------------------------------------
// Traits

/// Reads an audio file into memory plus a peak envelope for display.
pub trait AudioDecoder: Send + Sync {
    fn load(&self, path: &Path) -> Result<SourceAudio, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Demucs,
    Uvr,
    Mock,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Demucs => "Demucs",
            Self::Uvr => "UVR",
            Self::Mock => "Preview",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SplitModel {
    pub id: String,
    pub engine: Engine,
    pub name: String,
    pub description: String,
    /// Short quality / speed tag shown on the card, e.g. "Best quality".
    pub tag: String,
    pub stems: Vec<StemKind>,
}

impl SplitModel {
    /// `engine:id`, the key used by the worker and the database.
    pub fn key(&self) -> String {
        let engine = match self.engine {
            Engine::Demucs => "demucs",
            Engine::Uvr => "uvr",
            Engine::Mock => "mock",
        };
        format!("{engine}:{}", self.id)
    }
}

pub struct StemOutput {
    pub kind: StemKind,
    pub peaks: Peaks,
    pub path: Option<PathBuf>,
    pub audio: Option<Arc<AudioBuffer>>,
}

/// Separates a source file into stems. Runs on a worker thread.
pub trait StemSplitter: Send + Sync {
    fn models(&self) -> Vec<SplitModel>;

    /// `progress(fraction, stage)` should be called regularly; a negative
    /// fraction means "indeterminate". Implementations should bail out with
    /// `Err` when `cancel` is set.
    /// Writes stems into `out_dir` and returns them *without* decoding audio
    /// (`StemOutput::audio` is `None`); use `StemLoadJob` to load them.
    /// Mock splitters return peaks and no paths.
    fn split(
        &self,
        source: &Path,
        model: &SplitModel,
        out_dir: &Path,
        progress: &dyn Fn(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<Vec<StemOutput>, String>;
}

/// Plays all tracks in sync. The UI reads `position()` every frame.
/// A section's mix as the engine sees it (same shape as `set_mix`).
#[derive(Clone, Debug, PartialEq)]
pub struct SectionMix {
    pub start: f64,
    pub end: f64,
    pub gains: Vec<f32>,
    pub pans: Vec<f32>,
}

pub trait PlaybackEngine {
    fn play(&mut self);
    fn pause(&mut self);
    fn is_playing(&self) -> bool;
    fn seek(&mut self, seconds: f64);
    fn position(&self) -> f64;
    /// Replace the set of tracks being played (stems, or just the source).
    fn set_tracks(&mut self, tracks: Vec<Arc<AudioBuffer>>);
    /// `gains` is linear gain per track after mute/solo is applied.
    fn set_mix(&mut self, gains: &[f32], pans: &[f32], master: f32);
    /// Loop `start..end` sample-accurately while playing (`None` = off).
    fn set_loop(&mut self, region: Option<(f64, f64)>);
    /// Time ranges that play with their own gains instead of `set_mix`'s,
    /// switched sample-accurately by the audio thread (sorted, no overlaps).
    fn set_sections(&mut self, sections: &[SectionMix], master: f32);
    /// Human readable output description for the status bar.
    fn describe(&self) -> String;
}

/// Produces timings for lyric lines.
pub trait LyricAligner: Send + Sync {
    fn align(&self, lines: &[String], duration: f64, vocals: Option<&Peaks>) -> Vec<LyricLine>;
}

// ---------------------------------------------------------------------------
// Background jobs

/// Decodes a dropped file off the UI thread.
pub struct LoadJob {
    pub rx: Receiver<Result<SourceAudio, String>>,
    /// Caller-defined id (the song being loaded).
    pub tag: i64,
}

impl LoadJob {
    pub fn spawn(decoder: Arc<dyn AudioDecoder>, path: PathBuf, tag: i64, repaint: egui::Context) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(decoder.load(&path));
            repaint.request_repaint();
        });
        Self { rx, tag }
    }
}

#[cfg(test)]
mod wav_tests {
    use super::*;

    #[test]
    fn written_wavs_decode_and_only_worker_formats_skip_conversion() {
        let dir = std::env::temp_dir().join(format!("sw-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 0.5 s of a stereo ramp at 22.05 kHz.
        let data: Vec<f32> = (0..11_025).flat_map(|i| [i as f32 / 11_025.0, -0.25]).collect();
        let audio = AudioBuffer { sample_rate: 22_050, data };
        let (stereo, mono) = (dir.join("s.wav"), dir.join("m.wav"));
        write_wav(&stereo, &audio, false).unwrap();
        write_wav(&mono, &audio, true).unwrap();
        let (back, channels) = decode_file_for_tests(&stereo);
        assert_eq!((back.sample_rate, back.frames(), channels), (22_050, 11_025, 2));
        assert!((back.data[2 * 5_000] - 5_000.0 / 11_025.0).abs() < 1e-3 && (back.data[1] + 0.25).abs() < 1e-3);
        assert_eq!(std::fs::metadata(&mono).unwrap().len(), 44 + 11_025 * 2);
        assert!(worker_reads(Path::new("a.MP3")) && !worker_reads(Path::new("song.m4a")) && !worker_reads(Path::new("x.aiff")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
