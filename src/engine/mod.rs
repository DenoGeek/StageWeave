//! Backend seams. The UI only talks to these traits; real implementations
//! (symphonia decoder, cpal playback, Python separation worker) and the mocks
//! used by `--demo` live in the submodules.

mod aligner;
mod decoder;
pub mod ffmpeg;
pub mod mock;
pub mod models;
mod playback;
pub mod python_env;
mod python_splitter;
pub mod queue;
pub mod worker;

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
