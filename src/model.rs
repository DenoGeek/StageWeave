//! Core data model: the project, its stem tracks and the timed lyrics.

use std::path::PathBuf;
use std::sync::Arc;

use egui::Color32;

/// Amplitude envelope used for drawing waveforms and driving meters.
#[derive(Clone, Debug, Default)]
pub struct Peaks {
    /// Number of peak values per second of audio.
    pub per_sec: f32,
    /// Normalised amplitudes in `0.0..=1.0`.
    pub data: Vec<f32>,
}

impl Peaks {
    pub fn at(&self, t: f64) -> f32 {
        if self.data.is_empty() || t < 0.0 {
            return 0.0;
        }
        let i = (t * self.per_sec as f64) as usize;
        self.data.get(i).copied().unwrap_or(0.0)
    }

    /// Max amplitude in the time range `[t0, t1)`.
    pub fn max_in(&self, t0: f64, t1: f64) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        let ps = self.per_sec as f64;
        let i0 = (t0.max(0.0) * ps) as usize;
        let i1 = ((t1.max(0.0) * ps).ceil() as usize).max(i0 + 1).min(self.data.len());
        if i0 >= i1 {
            return 0.0;
        }
        self.data[i0..i1].iter().copied().fold(0.0, f32::max)
    }
}

/// Decoded PCM, interleaved stereo f32.
#[derive(Debug, Default)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub data: Vec<f32>,
}

impl AudioBuffer {
    pub fn frames(&self) -> usize {
        self.data.len() / 2
    }

    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 { 0.0 } else { self.frames() as f64 / self.sample_rate as f64 }
    }

    /// Envelope at `per_sec` resolution (max of |L|,|R| per window).
    pub fn peaks(&self, per_sec: f32) -> Peaks {
        let window = ((self.sample_rate as f32 / per_sec) as usize).max(1);
        let data = self
            .data
            .chunks(window * 2)
            .map(|c| c.iter().fold(0.0f32, |m, s| m.max(s.abs())).min(1.0))
            .collect();
        Peaks { per_sec, data }
    }
}

#[derive(Clone, Debug)]
pub struct AudioInfo {
    pub file_name: String,
    pub format: String,
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug)]
pub struct SourceAudio {
    pub info: AudioInfo,
    pub peaks: Peaks,
    /// `None` for mock sources.
    pub audio: Option<Arc<AudioBuffer>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum StemKind {
    Vocals,
    Drums,
    Bass,
    Guitar,
    Piano,
    Other,
    Instrumental,
}

impl StemKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vocals => "Vocals",
            Self::Drums => "Drums",
            Self::Bass => "Bass",
            Self::Guitar => "Guitar",
            Self::Piano => "Piano",
            Self::Other => "Other",
            Self::Instrumental => "Instrumental",
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Self::Vocals => Color32::from_rgb(232, 93, 160),
            Self::Drums => Color32::from_rgb(240, 146, 56),
            Self::Bass => Color32::from_rgb(140, 110, 245),
            Self::Guitar => Color32::from_rgb(92, 196, 120),
            Self::Piano => Color32::from_rgb(64, 190, 210),
            Self::Other => Color32::from_rgb(222, 196, 70),
            Self::Instrumental => Color32::from_rgb(90, 160, 240),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub name: String,
    pub kind: StemKind,
    pub color: Color32,
    pub peaks: Peaks,
    /// Location of the rendered stem file, once a real splitter provides one.
    pub path: Option<PathBuf>,
    pub audio: Option<Arc<AudioBuffer>>,
    pub gain_db: f32,
    /// -1.0 (left) ..= 1.0 (right)
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    /// Smoothed meter level (display only).
    pub meter: f32,
}

impl Track {
    pub fn new(kind: StemKind, peaks: Peaks, path: Option<PathBuf>, audio: Option<Arc<AudioBuffer>>) -> Self {
        Self {
            audio,
            name: kind.label().to_owned(),
            kind,
            color: kind.color(),
            peaks,
            path,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            solo: false,
            meter: 0.0,
        }
    }

    pub fn gain_linear(&self) -> f32 {
        if self.gain_db <= MIN_DB {
            0.0
        } else {
            10f32.powf(self.gain_db / 20.0)
        }
    }
}

pub const MIN_DB: f32 = -60.0;
pub const MAX_DB: f32 = 6.0;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LyricLine {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Word timings from forced alignment, in order. Only kept while they
    /// still match the line's text (see `Lyrics::tidy_words`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<LyricWord>,
}

/// One word's timing inside a line (seconds, song time).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LyricWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

impl LyricLine {
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self { text: text.into(), start, end, words: Vec::new() }
    }

    /// Do the word timings still describe this line's text?
    fn words_match(&self) -> bool {
        self.text.split_whitespace().eq(self.words.iter().map(|w| w.text.as_str()))
    }

    /// Keep word timings honest after an edit: clear them if the text no
    /// longer matches, else clamp them inside the line in order.
    pub fn fit_words(&mut self) {
        if self.words.is_empty() {
            return;
        }
        if !self.words_match() {
            self.words.clear();
            return;
        }
        let mut floor = self.start;
        for w in &mut self.words {
            w.start = w.start.clamp(floor, self.end);
            w.end = w.end.clamp(w.start, self.end);
            floor = w.start;
        }
    }
}

/// Split a lyric line into the sung part and a translation written in
/// parentheses: "Ana mipango mema (He has good plans)" →
/// ("Ana mipango mema", Some("He has good plans")). Several bracketed parts
/// are joined; a line that is only brackets (e.g. "(Chorus)") has an empty
/// sung part.
pub fn split_translation(text: &str) -> (String, Option<String>) {
    let mut sung = String::new();
    let mut translation: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' => {
                if depth > 0 {
                    current.push(c);
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let t = current.trim().to_owned();
                    if !t.is_empty() {
                        translation.push(t);
                    }
                    current.clear();
                } else {
                    current.push(c);
                }
            }
            _ if depth > 0 => current.push(c),
            _ => sung.push(c),
        }
    }
    // An unclosed "(" keeps its text as translation.
    if depth > 0 && !current.trim().is_empty() {
        translation.push(current.trim().to_owned());
    }
    let sung = sung.split_whitespace().collect::<Vec<_>>().join(" ");
    let sung = sung.trim_end_matches([',', ';', '-', '–', '—']).trim().to_owned();
    (sung, if translation.is_empty() { None } else { Some(translation.join(" · ")) })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LyricTheme {
    Aurora,
    Sunset,
    Midnight,
    Stage,
}

impl LyricTheme {
    pub const ALL: [Self; 4] = [Self::Aurora, Self::Sunset, Self::Midnight, Self::Stage];

    pub fn label(self) -> &'static str {
        match self {
            Self::Aurora => "Aurora",
            Self::Sunset => "Sunset",
            Self::Midnight => "Midnight",
            Self::Stage => "Stage",
        }
    }

    /// (top gradient, bottom gradient, highlight, blob accent)
    pub fn palette(self) -> [Color32; 4] {
        match self {
            Self::Aurora => [
                Color32::from_rgb(10, 22, 40),
                Color32::from_rgb(18, 60, 70),
                Color32::from_rgb(110, 240, 200),
                Color32::from_rgb(60, 140, 255),
            ],
            Self::Sunset => [
                Color32::from_rgb(40, 12, 50),
                Color32::from_rgb(120, 40, 50),
                Color32::from_rgb(255, 200, 90),
                Color32::from_rgb(255, 90, 120),
            ],
            Self::Midnight => [
                Color32::from_rgb(6, 6, 16),
                Color32::from_rgb(20, 18, 48),
                Color32::from_rgb(150, 160, 255),
                Color32::from_rgb(100, 60, 200),
            ],
            Self::Stage => [
                Color32::from_rgb(0, 0, 0),
                Color32::from_rgb(16, 16, 16),
                Color32::from_rgb(255, 220, 0),
                Color32::from_rgb(255, 255, 255),
            ],
        }
    }
}

/// What sits behind the lyrics. Image and video files live in the
/// workspace's `Backgrounds/` folder and are referenced by file name.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum Background {
    /// The theme's gradient (with optional animated glow).
    #[default]
    Theme,
    Image { file: String },
    /// A short clip, looped for the whole song.
    Video { file: String },
}

impl Background {
    pub fn file(&self) -> Option<&str> {
        match self {
            Self::Theme => None,
            Self::Image { file } | Self::Video { file } => Some(file),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LyricStyle {
    pub theme: LyricTheme,
    /// Relative text size (1.0 = default).
    pub scale: f32,
    pub show_next_line: bool,
    pub show_progress: bool,
    pub animated_background: bool,
    pub background: Background,
    /// How much an image / video background is darkened for legibility (0–1).
    pub background_dim: f32,
}

impl Default for LyricStyle {
    fn default() -> Self {
        Self {
            theme: LyricTheme::Aurora,
            scale: 1.0,
            show_next_line: true,
            show_progress: true,
            animated_background: true,
            background: Background::Theme,
            background_dim: 0.45,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub title: String,
    pub artist: String,
    /// Raw pasted lyrics (one line per row; blank rows are ignored).
    pub raw_text: String,
    pub lines: Vec<LyricLine>,
    pub style: LyricStyle,
}

impl Lyrics {
    pub fn is_generated(&self) -> bool {
        !self.lines.is_empty()
    }

    /// Drop or clamp word timings that edits made stale (cheap; run before saving).
    pub fn tidy_words(&mut self) {
        for l in &mut self.lines {
            l.fit_words();
        }
    }

    pub fn line_at(&self, t: f64) -> Option<usize> {
        self.lines.iter().position(|l| t >= l.start && t < l.end)
    }

    pub fn next_line_after(&self, t: f64) -> Option<usize> {
        self.lines.iter().position(|l| l.start > t)
    }

    /// The line being sung at `t`, else the last one that has started.
    pub fn line_for_edit(&self, t: f64) -> Option<usize> {
        self.line_at(t).or_else(|| self.lines.iter().rposition(|l| l.start <= t)).or(if self.lines.is_empty() { None } else { Some(0) })
    }

    // ----- editing (used by the monitor's edit mode) -----

    /// Smallest allowed line length.
    pub const MIN_LEN: f64 = 0.2;

    /// Set a line's start; the previous line is trimmed so they don't overlap.
    pub fn set_start(&mut self, i: usize, t: f64) {
        let Some(line) = self.lines.get_mut(i) else { return };
        line.start = t.max(0.0);
        if line.end < line.start + Self::MIN_LEN {
            line.end = line.start + Self::MIN_LEN.max(2.0);
        }
        let start = line.start;
        if i > 0 && self.lines[i - 1].end > start {
            self.lines[i - 1].end = start.max(self.lines[i - 1].start + Self::MIN_LEN);
        }
        self.fix_order(i);
    }

    /// Set a line's end; the next line is pushed later if needed.
    pub fn set_end(&mut self, i: usize, t: f64) {
        let Some(line) = self.lines.get_mut(i) else { return };
        line.end = t.max(line.start + Self::MIN_LEN);
        let end = line.end;
        if let Some(next) = self.lines.get_mut(i + 1)
            && next.start < end
        {
            next.start = end;
            if next.end < next.start + Self::MIN_LEN {
                next.end = next.start + Self::MIN_LEN;
            }
        }
    }

    /// Insert an empty line at `t` (until the next line, at most 4 s). Returns its index.
    pub fn insert_at(&mut self, t: f64) -> usize {
        let i = self.lines.iter().position(|l| l.start > t).unwrap_or(self.lines.len());
        let limit = self.lines.get(i).map_or(t + 4.0, |n| n.start);
        let end = (t + 4.0).min(limit).max(t + Self::MIN_LEN);
        if i > 0 && self.lines[i - 1].end > t {
            self.lines[i - 1].end = t.max(self.lines[i - 1].start + Self::MIN_LEN);
        }
        self.lines.insert(i, LyricLine::new(String::new(), t, end));
        i
    }

    /// Insert an empty line just before line `i`: in the gap before it if
    /// there's room, else by taking the first half of line `i`. Returns its index.
    pub fn insert_before(&mut self, i: usize) -> usize {
        let Some(line) = self.lines.get(i).cloned() else { return self.insert_at(0.0) };
        let prev_end = if i == 0 { 0.0 } else { self.lines[i - 1].end };
        let gap = line.start - prev_end;
        let (start, end) = if gap >= 1.0 {
            ((line.start - 3.0).max(prev_end), line.start)
        } else {
            let mid = line.start + (line.end - line.start) / 2.0;
            self.lines[i].start = mid;
            (line.start, mid)
        };
        self.lines.insert(i, LyricLine::new(String::new(), start, end.max(start + Self::MIN_LEN)));
        i
    }

    /// Insert an empty line just after line `i` (gap after it, else its second half).
    pub fn insert_after(&mut self, i: usize, song_end: f64) -> usize {
        let Some(line) = self.lines.get(i).cloned() else { return self.insert_at(0.0) };
        let next_start = self.lines.get(i + 1).map_or(song_end, |n| n.start);
        let gap = next_start - line.end;
        let (start, end) = if gap >= 1.0 {
            (line.end, (line.end + 3.0).min(next_start))
        } else {
            let mid = line.start + (line.end - line.start) / 2.0;
            self.lines[i].end = mid;
            (mid, line.end)
        };
        self.lines.insert(i + 1, LyricLine::new(String::new(), start, end.max(start + Self::MIN_LEN)));
        i + 1
    }

    /// Split line `i` at time `t`, dividing its words by time share.
    pub fn split_at(&mut self, i: usize, t: f64) -> bool {
        let Some(line) = self.lines.get(i).cloned() else { return false };
        if t <= line.start + Self::MIN_LEN || t >= line.end - Self::MIN_LEN {
            return false;
        }
        let words: Vec<&str> = line.text.split_whitespace().collect();
        let frac = (t - line.start) / (line.end - line.start);
        let cut = ((words.len() as f64 * frac).round() as usize).clamp(1.min(words.len()), words.len().saturating_sub(1).max(1));
        let (a, b) = words.split_at(cut.min(words.len()));
        let mut first = LyricLine::new(a.join(" "), line.start, t);
        let mut second = LyricLine::new(b.join(" "), t, line.end);
        if line.words_match() {
            // Timed words go to the side they're sung on; the cut follows them.
            let n = line.words.iter().filter(|w| w.start < t).count().clamp(1, line.words.len().saturating_sub(1).max(1));
            let (wa, wb) = line.words.split_at(n.min(line.words.len()));
            first = LyricLine { text: wa.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), words: wa.to_vec(), ..first };
            second = LyricLine { text: wb.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "), words: wb.to_vec(), ..second };
            first.fit_words();
            second.fit_words();
        }
        self.lines[i] = first;
        self.lines.insert(i + 1, second);
        true
    }

    /// Merge line `i` with the one after it.
    pub fn merge_with_next(&mut self, i: usize) -> bool {
        if i + 1 >= self.lines.len() {
            return false;
        }
        let next = self.lines.remove(i + 1);
        let line = &mut self.lines[i];
        let both_timed = line.words_match() && next.words_match() && !line.words.is_empty() && !next.words.is_empty();
        line.text = format!("{} {}", line.text.trim(), next.text.trim()).trim().to_owned();
        line.end = next.end;
        if both_timed {
            line.words.extend(next.words);
        }
        line.fit_words();
        true
    }

    pub fn delete(&mut self, i: usize) {
        if i < self.lines.len() {
            self.lines.remove(i);
        }
    }

    /// Keep lines sorted after a start time moved past a neighbour.
    fn fix_order(&mut self, _moved: usize) {
        self.lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    /// Rewrite the raw lyrics text from the timed lines, keeping blank-line
    /// verse breaks. Lines are matched to the old text in order, so an insert
    /// or delete keeps every other verse break where it was.
    pub fn sync_raw_from_lines(&mut self) {
        // (text, blank line before it) from the current raw text.
        let mut old: Vec<(String, bool)> = Vec::new();
        let mut pending = false;
        for l in self.raw_text.lines() {
            if l.trim().is_empty() {
                pending = !old.is_empty();
            } else {
                old.push((l.trim().to_owned(), pending));
                pending = false;
            }
        }
        let same_count = old.len() == self.lines.len();
        let mut cursor = 0;
        let mut out: Vec<String> = Vec::new();
        for (k, line) in self.lines.iter().enumerate() {
            let brk = if same_count {
                // Edits in place: keep breaks by position.
                old[k].1
            } else if let Some(pos) = old[cursor..].iter().position(|(t, _)| *t == line.text.trim()) {
                cursor += pos + 1;
                old[cursor - 1].1
            } else {
                false
            };
            if brk && !out.is_empty() {
                out.push(String::new());
            }
            out.push(line.text.clone());
        }
        self.raw_text = out.join("\n");
    }
}

/// Per-song mixer settings, persisted as JSON with the song.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SongMixer {
    pub master_db: f32,
    pub tracks: Vec<TrackMix>,
    /// Cue loops for the song (saved with the mixer so they travel with it).
    pub cues: Vec<Cue>,
    /// Sections (S to set start and end): parts of the song with their own
    /// stem mix, applied as playback passes through. Never overlapping.
    pub sections: Vec<Section>,
}

/// A section of the song to loop (Q to set start and end), optionally with
/// its own stem mix.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Cue {
    pub name: String,
    pub start: f64,
    pub end: f64,
    /// Per-stem settings used while this cue plays (`None` = the song's mix).
    #[serde(default)]
    pub mix: Option<Vec<TrackMix>>,
}

/// A part of the song that plays with its own stem mix. Unlike a cue it
/// doesn't loop: playback passes through and the mix switches on the way.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Section {
    pub name: String,
    pub start: f64,
    pub end: f64,
    /// Per-stem settings inside this section (`None` = the song's mix).
    #[serde(default)]
    pub mix: Option<Vec<TrackMix>>,
}

/// The section playing at `t`, if any.
pub fn section_at(sections: &[Section], t: f64) -> Option<usize> {
    sections.iter().position(|s| t >= s.start && t < s.end)
}

/// Add a section, taking over the time it covers: overlapping sections are
/// trimmed, split around it, or dropped when fully covered. Keeps the list
/// sorted. Returns the new section's index.
pub fn insert_section(sections: &mut Vec<Section>, new: Section) -> usize {
    let mut out: Vec<Section> = Vec::with_capacity(sections.len() + 2);
    for s in sections.drain(..) {
        if s.end <= new.start || s.start >= new.end {
            out.push(s);
            continue;
        }
        if s.start < new.start - 0.25 {
            out.push(Section { end: new.start, ..s.clone() });
        }
        if s.end > new.end + 0.25 {
            out.push(Section { start: new.end, ..s });
        }
    }
    out.push(new.clone());
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    let i = out.iter().position(|s| *s == new).unwrap_or(0);
    *sections = out;
    i
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackMix {
    pub kind: StemKind,
    pub gain_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
}

impl SongMixer {
    pub fn capture(tracks: &[Track], master_db: f32) -> Self {
        Self {
            master_db,
            tracks: tracks
                .iter()
                .map(|t| TrackMix { kind: t.kind, gain_db: t.gain_db, pan: t.pan, muted: t.muted, solo: t.solo })
                .collect(),
            cues: Vec::new(),
            sections: Vec::new(),
        }
    }

    /// Apply saved settings to matching stems (by kind).
    pub fn apply(&self, tracks: &mut [Track]) {
        apply_track_mix(&self.tracks, tracks);
    }
}

/// Apply per-stem settings to matching stems (by kind).
pub fn apply_track_mix(mix: &[TrackMix], tracks: &mut [Track]) {
    {
        for t in tracks {
            if let Some(m) = mix.iter().find(|m| m.kind == t.kind) {
                t.gain_db = m.gain_db;
                t.pan = m.pan;
                t.muted = m.muted;
                t.solo = m.solo;
            }
        }
    }
}

#[derive(Default)]
pub struct Session {
    pub source: Option<SourceAudio>,
    pub tracks: Vec<Track>,
    pub lyrics: Lyrics,
    pub cues: Vec<Cue>,
    pub sections: Vec<Section>,
}

impl Session {
    pub fn duration(&self) -> f64 {
        self.source.as_ref().map_or(0.0, |s| s.info.duration)
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.solo)
    }

    /// Whether the track is audible given mute/solo state.
    pub fn is_audible(&self, idx: usize) -> bool {
        let t = &self.tracks[idx];
        if self.any_solo() { t.solo } else { !t.muted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_take_window_maximum() {
        // 1 s of stereo at 10 Hz, one loud sample in the second window.
        let mut data = vec![0.1f32; 20];
        data[13] = -0.8;
        let buf = AudioBuffer { sample_rate: 10, data };
        let p = buf.peaks(2.0); // 5 frames per window
        assert_eq!(p.data.len(), 2);
        assert!((p.data[0] - 0.1).abs() < 1e-6);
        assert!((p.data[1] - 0.8).abs() < 1e-6);
        assert!((buf.duration() - 1.0).abs() < 1e-9);
    }

    fn sec(name: &str, start: f64, end: f64) -> Section {
        Section { name: name.into(), start, end, mix: None }
    }

    #[test]
    fn a_new_section_takes_over_the_time_it_covers() {
        let mut s = vec![sec("A", 0.0, 10.0), sec("B", 10.0, 20.0), sec("C", 30.0, 60.0)];
        // Covers the end of A, all of B, and the middle of C.
        let i = insert_section(&mut s, sec("N", 8.0, 40.0));
        let got: Vec<(&str, f64, f64)> = s.iter().map(|x| (x.name.as_str(), x.start, x.end)).collect();
        assert_eq!(got, vec![("A", 0.0, 8.0), ("N", 8.0, 40.0), ("C", 40.0, 60.0)]);
        assert_eq!(i, 1);
        // Inside one: split around it.
        let i = insert_section(&mut s, sec("M", 45.0, 50.0));
        assert_eq!(i, 3);
        assert_eq!((s[2].start, s[2].end, s[4].start, s[4].end), (40.0, 45.0, 50.0, 60.0));
        assert_eq!(section_at(&s, 9.0), Some(1));
        assert_eq!(section_at(&s, 40.0), Some(2));
        assert_eq!(section_at(&s, 61.0), None);
    }

    fn timed(text: &str, words: &[(&str, f64, f64)]) -> LyricLine {
        let (start, end) = (words[0].1, words[words.len() - 1].2);
        LyricLine {
            words: words.iter().map(|&(t, s, e)| LyricWord { text: t.into(), start: s, end: e }).collect(),
            ..LyricLine::new(text, start, end)
        }
    }

    #[test]
    fn word_timings_survive_edits_or_go_away() {
        // Old saved lines have no words.
        let old: Vec<LyricLine> = serde_json::from_str(r#"[{"text":"Amazing grace","start":1.0,"end":3.0}]"#).unwrap();
        assert!(old[0].words.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("words"), "not written when empty");

        let mut l = Lyrics {
            lines: vec![timed("Here comes the sun", &[("Here", 1.0, 1.5), ("comes", 1.5, 2.0), ("the", 2.5, 2.7), ("sun", 2.7, 4.0)])],
            ..Default::default()
        };
        // Split between "comes" and "the": each half keeps its own words.
        assert!(l.split_at(0, 2.2));
        assert_eq!(l.lines[0].text, "Here comes");
        assert_eq!(l.lines[1].text, "the sun");
        assert_eq!(l.lines[1].words[0].start, 2.5);
        // Merge back.
        assert!(l.merge_with_next(0));
        assert_eq!(l.lines[0].words.len(), 4);
        // Moving the start later clamps the words into the line.
        l.set_start(0, 1.8);
        l.tidy_words();
        assert!(l.lines[0].words.iter().all(|w| w.start >= 1.8 && w.end <= l.lines[0].end));
        // A text edit makes them stale: dropped.
        l.lines[0].text = "Here comes the moon".into();
        l.tidy_words();
        assert!(l.lines[0].words.is_empty());
    }

    #[test]
    fn mixers_saved_before_sections_still_load() {
        let m: SongMixer = serde_json::from_str(r#"{"master_db":-3.0,"tracks":[],"cues":[]}"#).unwrap();
        assert!(m.sections.is_empty());
        assert_eq!(m.master_db, -3.0);
    }
}

#[cfg(test)]
mod mixer_tests {
    use super::*;

    #[test]
    fn mixer_round_trips_and_applies_by_kind() {
        let mut tracks = vec![
            Track::new(StemKind::Vocals, Peaks::default(), None, None),
            Track::new(StemKind::Drums, Peaks::default(), None, None),
        ];
        tracks[1].gain_db = -6.0;
        tracks[1].muted = true;
        let mix = SongMixer::capture(&tracks, -2.0);
        let json = serde_json::to_string(&mix).unwrap();
        let back: SongMixer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mix);

        // Applied to a fresh split in a different order, matching by kind.
        let mut fresh = vec![
            Track::new(StemKind::Drums, Peaks::default(), None, None),
            Track::new(StemKind::Bass, Peaks::default(), None, None),
        ];
        back.apply(&mut fresh);
        assert!(fresh[0].muted && fresh[0].gain_db == -6.0);
        assert!(!fresh[1].muted && fresh[1].gain_db == 0.0);
    }
}

#[cfg(test)]
mod lyric_edit_tests {
    use super::*;

    fn lyrics(spec: &[(&str, f64, f64)]) -> Lyrics {
        Lyrics {
            lines: spec.iter().map(|(t, s, e)| LyricLine::new(*t, *s, *e)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn stamping_start_and_end_keeps_lines_apart() {
        let mut l = lyrics(&[("a", 0.0, 5.0), ("b", 5.0, 10.0), ("c", 10.0, 15.0)]);
        l.set_start(1, 4.0); // earlier start trims line a
        assert_eq!((l.lines[0].end, l.lines[1].start), (4.0, 4.0));
        l.set_end(1, 11.0); // later end pushes line c
        assert_eq!((l.lines[1].end, l.lines[2].start), (11.0, 11.0));
        l.set_end(2, 10.0); // can't end before its start
        assert!(l.lines[2].end >= l.lines[2].start + Lyrics::MIN_LEN);
    }

    #[test]
    fn insert_split_merge_delete() {
        let mut l = lyrics(&[("one two three four", 0.0, 4.0), ("five", 10.0, 12.0)]);
        let i = l.insert_at(6.0);
        assert_eq!(i, 1);
        assert_eq!((l.lines[1].start, l.lines[1].end), (6.0, 10.0));
        assert!(l.split_at(0, 2.0));
        assert_eq!(l.lines[0].text, "one two");
        assert_eq!(l.lines[1].text, "three four");
        assert_eq!((l.lines[0].end, l.lines[1].start), (2.0, 2.0));
        assert!(l.merge_with_next(0));
        assert_eq!(l.lines[0].text, "one two three four");
        assert_eq!(l.lines[0].end, 4.0);
        l.delete(1);
        assert_eq!(l.lines.len(), 2);
        assert!(!l.split_at(0, 0.05), "too close to the edge");
    }

    #[test]
    fn insert_above_and_below_use_gaps_or_share_the_line() {
        // Gaps available: before (0-5) and after (8-20).
        let mut l = lyrics(&[("a", 5.0, 8.0), ("b", 20.0, 24.0)]);
        let i = l.insert_before(0);
        assert_eq!(i, 0);
        assert_eq!((l.lines[0].start, l.lines[0].end), (2.0, 5.0));
        let j = l.insert_after(1, 60.0); // after "a"
        assert_eq!(j, 2);
        assert_eq!((l.lines[2].start, l.lines[2].end), (8.0, 11.0));
        // No gap: share the line.
        let mut l = lyrics(&[("a", 0.0, 4.0), ("b", 4.0, 8.0)]);
        let k = l.insert_after(0, 60.0);
        assert_eq!((l.lines[0].end, l.lines[k].start, l.lines[k].end), (2.0, 2.0, 4.0));
        let m = l.insert_before(2); // before "b"
        assert_eq!((l.lines[m].start, l.lines[m].end, l.lines[m + 1].start), (4.0, 6.0, 6.0));
        assert!(l.lines.windows(2).all(|w| w[0].end <= w[1].start + 1e-9));
    }

    #[test]
    fn translations_in_parentheses_are_split_off() {
        assert_eq!(
            split_translation("Ana mipango mema, anakupenda (He has good plans, He loves You)"),
            ("Ana mipango mema, anakupenda".into(), Some("He has good plans, He loves You".into()))
        );
        assert_eq!(split_translation("No translation here"), ("No translation here".into(), None));
        assert_eq!(split_translation("(Chorus)"), ("".into(), Some("Chorus".into())));
        assert_eq!(split_translation("Yesu (Jesus) ni Bwana (is Lord)"), ("Yesu ni Bwana".into(), Some("Jesus · is Lord".into())));
        assert_eq!(split_translation("Bwana (Lord (God))"), ("Bwana".into(), Some("Lord (God)".into())));
    }

    #[test]
    fn raw_text_keeps_verse_breaks() {
        let mut l = lyrics(&[("A!", 0.0, 1.0), ("B", 1.0, 2.0), ("C", 2.0, 3.0)]);
        l.raw_text = "a\nb\n\nc".into();
        l.sync_raw_from_lines();
        assert_eq!(l.raw_text, "A!\nB\n\nC", "text edits keep breaks by position");
        // Insert a new line in verse 1: the break before C survives.
        let i = l.insert_after(0, 10.0);
        l.lines[i].text = "new".into();
        l.sync_raw_from_lines();
        assert_eq!(l.raw_text, "A!\nnew\nB\n\nC");
        // Delete B: still a break before C.
        l.delete(2);
        l.sync_raw_from_lines();
        assert_eq!(l.raw_text, "A!\nnew\n\nC");
    }
}
