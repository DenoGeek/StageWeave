use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::ai::{AiSettings, JobEvent, TranscribeJob, TranscribeRequest, Transcript};
use crate::engine::mock::{MockDecoder, MockPlayback, MockSplitter};
use crate::engine::models::ModelManager;
use crate::engine::python_env::{EnvStatus, PythonEnv};
use crate::engine::queue::{JobState, SplitQueue, SplitRequest, StemLoadJob};
use crate::engine::{
    AudioDecoder, CpalPlayback, LoadJob, LyricAligner, NaiveAligner, PlaybackEngine, PythonSplitter, SplitModel,
    StemOutput, StemSplitter, SymphoniaDecoder,
};
use crate::model::{LyricLine, LyricStyle, Session, SongMixer, SourceAudio, StemKind, Track};
use crate::store::{ProjectRow, SongMeta, SongRow, Store};
use crate::ui;
use crate::workspace::{self, Importer, MoveEvent, Workspace};

/// Editor layout presets (header switcher).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorLayout {
    Edit,
    Lyrics,
    Perform,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Home,
    Project(i64),
    Song { project: i64, song: i64 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub born: Instant,
}

pub struct TimelineView {
    /// Zoom level.
    pub px_per_sec: f32,
    /// Leftmost visible time in seconds.
    pub scroll: f64,
    /// Keep the playhead in view while playing.
    pub follow: bool,
    pub track_height: f32,
    /// Vertical scroll of the track list in points.
    pub v_scroll: f32,
    /// Fit the whole song on the next frame.
    pub fit_request: bool,
}

/// What the song list shows for one song.
#[derive(Clone, Debug, PartialEq)]
pub enum SongStatus {
    Importing(f32),
    Missing,
    Queued,
    Splitting { progress: f32, stage: String },
    Split(usize),
    NotSplit,
}

/// The Lyrics panel's AI transcription drawer.
#[derive(Default)]
pub struct AiPanel {
    pub open: bool,
    pub mode: AiMode,
    pub job: Option<TranscribeJob>,
    /// Finished result waiting to be applied (song id, result).
    pub result: Option<(i64, AiResult)>,
    pub error: Option<String>,
    /// Send the isolated vocals instead of the full mix (when split).
    pub use_vocals: bool,
    /// Align mode: the lyrics to time (prefilled from the Lyrics panel).
    pub align_text: String,
    /// Align mode: use Whisper (true) or on-device vocal phrases (false).
    pub align_with_whisper: bool,
    pub method: AiMethod,
}

/// How audio is sent to Whisper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AiMethod {
    /// Short pieces cut at the singer's pauses (needs the vocals stem).
    #[default]
    Phrases,
    /// One request for the whole song.
    WholeSong,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AiMode {
    #[default]
    Transcribe,
    Align,
}

pub enum AiResult {
    Transcript(Transcript),
    Alignment(crate::align::Alignment),
}

/// One step of lyric edit history.
#[derive(Clone)]
pub struct LyricSnapshot {
    pub label: &'static str,
    lines: Vec<crate::model::LyricLine>,
    raw_text: String,
    sel: Option<usize>,
}

/// Undo / redo for the open song's lyrics (cleared when switching songs).
#[derive(Default)]
pub struct LyricHistory {
    undo: Vec<LyricSnapshot>,
    redo: Vec<LyricSnapshot>,
    /// (coalesce key, when) of the last checkpoint: rapid nudges/drags merge.
    last: Option<(String, Instant)>,
    /// Set while a compound action runs so nested calls don't add steps.
    suspended: bool,
}

impl LyricHistory {
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|s| s.label)
    }
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|s| s.label)
    }
}

const HISTORY_LIMIT: usize = 200;

/// Editing lyric timing from the Program monitor (never the pop-out window).
pub struct LyricEdit {
    pub on: bool,
    /// Selected line index.
    pub sel: Option<usize>,
    /// Pausing playback stamps the selected line's start.
    pub pause_marks_start: bool,
    /// Ask the UI to focus the text field next frame.
    pub focus_text: bool,
    /// Timeline: a lyric clip is selected (Delete / Enter act on it).
    pub clip_selected: bool,
    /// Inline text editor open on (line, draft text).
    pub editing: Option<(usize, String)>,
    /// The editor is in the Lyrics panel's Timing list (else on the timeline).
    pub edit_in_list: bool,
}

/// Modal dialogs on the library screens.
pub enum Dialog {
    NewProject { name: String },
    RenameProject { id: i64, name: String },
    DeleteProject { id: i64, delete_files: bool },
    ChangeWorkspace { to: PathBuf },
    MovingWorkspace { progress: f32, file: String },
    /// N pressed on an existing line: replace it with a new one at `t`?
    ReplaceLine { line: usize, t: f64 },
    /// Options before saving a project archive.
    ExportProject { id: i64, include_stems: bool },
}

/// Cue loops: Q marks a start, Q again closes the cue and loops it.
#[derive(Default)]
pub struct CueState {
    /// Start of a cue being marked (waiting for the second Q).
    pub recording: Option<f64>,
    /// Index into `session.cues` of the cue that is looping.
    pub active: Option<usize>,
    /// The song's own stem settings, restored when the cue ends.
    base: Option<Vec<crate::model::TrackMix>>,
}

/// Sections: S marks a start, S again closes it. Playback never stops; the
/// section's mix plays while the playhead is inside it.
#[derive(Default)]
pub struct SectionState {
    /// Start of a section being marked (waiting for the second S).
    pub recording: Option<f64>,
    /// Index into `session.sections` of the one under the playhead, whose mix
    /// the mixer strips are showing (and editing).
    pub current: Option<usize>,
    /// The song's own stem settings while the strips show a section's.
    base: Option<Vec<crate::model::TrackMix>>,
}

pub enum ArchiveDone {
    Exported(PathBuf),
    Imported(i64),
    /// Songs reused from other setlists (new song ids).
    SongsAdded(Vec<i64>),
}

/// The ⌘K search over every project and song.
#[derive(Default)]
pub struct GlobalSearch {
    pub open: bool,
    pub query: String,
    /// Highlighted result (keyboard).
    pub cursor: usize,
}

/// "Add from Library": songs from other setlists, ready to reuse.
#[derive(Default)]
pub struct LibraryPicker {
    pub query: String,
    pub selected: Vec<i64>,
}

/// A project .zip being written or read on a background thread.
pub struct ArchiveJob {
    pub title: &'static str,
    pub progress: Arc<std::sync::Mutex<(f32, String)>>,
    done: Arc<std::sync::Mutex<Option<Result<ArchiveDone, String>>>>,
}

pub struct StageWeaveApp {
    pub session: Session,

    pub decoder: Arc<dyn AudioDecoder>,
    pub aligner: Arc<dyn LyricAligner>,
    pub playback: Box<dyn PlaybackEngine>,

    /// `None` in demo mode (mock engine).
    pub env: Option<Arc<PythonEnv>>,
    pub python_splitter: Option<Arc<PythonSplitter>>,
    pub model_manager: Option<Arc<ModelManager>>,
    /// Set once the engine is ready and model status has been refreshed.
    models_refreshed: bool,
    pub show_models: bool,
    pub ffmpeg: Arc<crate::engine::ffmpeg::Ffmpeg>,
    pub backdrop: crate::backgrounds::BackdropPlayer,
    pub export: crate::export::ExportPanel,
    pub archive_job: Option<ArchiveJob>,
    pub cue: CueState,
    pub section: SectionState,
    pub search: GlobalSearch,
    pub library: Option<LibraryPicker>,
    /// Filters on the home grid and the setlist table.
    pub home_query: String,
    pub setlist_query: String,
    /// All songs of all projects (search), refreshed when stale.
    song_index: Option<(Instant, Arc<Vec<SongRow>>)>,

    // Library
    pub store: Arc<Store>,
    pub ws: Arc<Workspace>,
    pub importer: Importer,
    pub queue: SplitQueue,
    pub screen: Screen,
    pub projects: Vec<ProjectRow>,
    /// Songs of the project being viewed / played (setlist order).
    pub songs: Vec<SongRow>,
    pub selected_song: Option<i64>,
    pub dialog: Option<Dialog>,
    move_rx: Option<Receiver<MoveEvent>>,
    /// (song, model) → all stems on disk. Cleared when anything changes.
    split_cache: HashMap<(i64, String), Option<usize>>,

    // Open song
    pub load_job: Option<LoadJob>,
    stem_load: Option<StemLoadJob>,
    preload: Option<LoadJob>,
    preloaded: Option<(i64, SourceAudio)>,
    /// Saved mixer to apply once the song's stems are loaded.
    pending_mixer: Option<SongMixer>,
    last_saved_state: String,
    last_save_check: Instant,

    pub models: Vec<SplitModel>,

    pub layout: EditorLayout,
    /// 0 = Lyrics editor, 1 = Track inspector
    pub editor_tab: usize,
    /// 0 = Setlist, 1 = Song
    pub project_tab: usize,
    pub monitor_tab: usize,
    pub timeline_tab: usize,

    pub selected_track: Option<usize>,
    pub timeline: TimelineView,
    pub loop_enabled: bool,
    pub master_db: f32,
    pub master_meter: [f32; 2],

    pub lyric_window_open: bool,
    pub lyric_window_fullscreen: bool,
    /// Index of the next lyric line to stamp in tap-sync mode.
    pub tap_sync: Option<usize>,

    pub ai: AiSettings,
    pub ai_panel: AiPanel,
    pub lyric_edit: LyricEdit,
    pub history: LyricHistory,

    pub show_settings: bool,
    /// stderr tail shown by the "Details" button after a failed split.
    pub error_details: Option<Vec<String>>,
    pub last_split_error: Option<String>,

    pub toasts: Vec<Toast>,
    pub last_frame: Instant,
    pub dev: crate::dev::Dev,
}

impl StageWeaveApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_context(&cc.egui_ctx, crate::dev::Dev::from_env())
    }

    /// Build the app on an existing egui context (also used by UI tests).
    pub fn with_context(egui_ctx: &egui::Context, dev: crate::dev::Dev) -> Self {
        crate::theme::apply(egui_ctx);
        let ctx = egui_ctx.clone();
        let mut startup_error = None;

        let open_store = |path: Option<PathBuf>| -> Arc<Store> {
            let opened = path.map(|p| Store::open(&p)).unwrap_or_else(Store::in_memory);
            Arc::new(opened.unwrap_or_else(|_| Store::in_memory().expect("in-memory database")))
        };

        let (decoder, splitter, env, python_splitter, model_manager, store): (
            Arc<dyn AudioDecoder>,
            Arc<dyn StemSplitter>,
            _,
            _,
            _,
            Arc<Store>,
        ) = if dev.demo {
            let store = open_store(None);
            store.set_setting(workspace::SETTING_KEY, &std::env::temp_dir().join("stageweave-demo").to_string_lossy());
            (Arc::new(MockDecoder), Arc::new(MockSplitter), None, None, None, store)
        } else {
            let env = PythonEnv::new();
            env.check(ctx.clone());
            let store = match Store::open(&env.paths.database()) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    startup_error = Some(e);
                    open_store(None)
                }
            };
            let manager = ModelManager::new(env.clone(), store.clone());
            let ps = Arc::new(PythonSplitter::new(env.clone(), manager.clone()));
            (Arc::new(SymphoniaDecoder), ps.clone(), Some(env), Some(ps), Some(manager), store)
        };
        let playback: Box<dyn PlaybackEngine> = if dev.demo {
            Box::new(MockPlayback::new())
        } else {
            match CpalPlayback::new() {
                Ok(p) => Box::new(p),
                Err(e) => {
                    startup_error = Some(format!("{e}. Playback will be silent."));
                    Box::new(MockPlayback::new())
                }
            }
        };
        let models = splitter.models();
        let ws = Workspace::new(store.clone());
        let mut app = Self {
            session: Session::default(),
            decoder,
            aligner: Arc::new(NaiveAligner),
            playback,
            env,
            python_splitter,
            model_manager,
            models_refreshed: false,
            show_models: false,
            ffmpeg: crate::engine::ffmpeg::Ffmpeg::new(&crate::engine::python_env::EnginePaths::new()),
            backdrop: Default::default(),
            export: Default::default(),
            archive_job: None,
            cue: CueState::default(),
            section: SectionState::default(),
            search: GlobalSearch::default(),
            library: None,
            home_query: String::new(),
            setlist_query: String::new(),
            song_index: None,
            importer: Importer::new(store.clone(), ctx.clone()),
            queue: SplitQueue::new(splitter, ctx.clone()),
            store: store.clone(),
            ws,
            screen: Screen::Home,
            projects: Vec::new(),
            songs: Vec::new(),
            selected_song: None,
            dialog: None,
            move_rx: None,
            split_cache: HashMap::new(),
            load_job: None,
            stem_load: None,
            preload: None,
            preloaded: None,
            pending_mixer: None,
            last_saved_state: String::new(),
            last_save_check: Instant::now(),
            models,
            layout: EditorLayout::Edit,
            editor_tab: 0,
            project_tab: 0,
            monitor_tab: 0,
            timeline_tab: 0,
            selected_track: None,
            timeline: TimelineView {
                px_per_sec: 12.0,
                scroll: 0.0,
                follow: true,
                track_height: 74.0,
                v_scroll: 0.0,
                fit_request: true,
            },
            loop_enabled: false,
            master_db: 0.0,
            master_meter: [0.0; 2],
            lyric_window_open: false,
            lyric_window_fullscreen: false,
            tap_sync: None,
            ai: AiSettings::new(store.clone()),
            ai_panel: AiPanel { use_vocals: true, align_with_whisper: true, ..Default::default() },
            history: LyricHistory::default(),
            lyric_edit: LyricEdit { on: false, sel: None, pause_marks_start: true, focus_text: false, clip_selected: false, editing: None, edit_in_list: false },
            show_settings: false,
            error_details: None,
            last_split_error: None,
            toasts: Vec::new(),
            last_frame: Instant::now(),
            dev,
        };
        app.reload_projects();
        if let Some(e) = startup_error {
            app.toast(ToastKind::Error, e);
        }
        crate::dev::start(&mut app, &ctx);
        app
    }

    // ------------------------------------------------------------------ backgrounds

    /// The current song's background for the lyric renderer this frame.
    pub fn lyric_backdrop(&mut self, ctx: &egui::Context) -> crate::ui::lyrics_view::Backdrop {
        let bg = self.session.lyrics.style.background.clone();
        let ffmpeg = self.ffmpeg_for_video(&bg, ctx);
        let root = self.ws.root();
        self.backdrop.backdrop(ctx, &root, &bg, ffmpeg.as_deref())
    }

    /// ffmpeg path; starts the one-time download if a video needs it.
    pub fn ffmpeg_for_video(&self, bg: &crate::model::Background, ctx: &egui::Context) -> Option<PathBuf> {
        let path = self.ffmpeg.path();
        if path.is_none() && matches!(bg, crate::model::Background::Video { .. }) && self.ffmpeg.status() == crate::engine::ffmpeg::FfmpegStatus::Missing {
            self.ffmpeg.install(ctx);
        }
        path
    }

    /// Use a background for the current song.
    pub fn set_background(&mut self, bg: crate::model::Background) {
        self.session.lyrics.style.background = bg;
        self.save_song_state();
    }

    /// Pick an image or video from disk, add it to the library and use it.
    pub fn choose_background_file(&mut self) {
        let mut exts: Vec<&str> = crate::backgrounds::IMAGE_EXTENSIONS.to_vec();
        exts.extend(crate::backgrounds::VIDEO_EXTENSIONS);
        let Some(file) = rfd::FileDialog::new().set_title("Choose a background image or video loop").add_filter("Images & videos", &exts).pick_file() else {
            return;
        };
        match crate::backgrounds::add_file(&self.ws.root(), &file) {
            Ok(name) => self.set_background(crate::backgrounds::background_for(&name)),
            Err(e) => self.toast(ToastKind::Error, e),
        }
    }

    // ------------------------------------------------------------------ cues

    /// Q: first press marks a cue's start, second press closes it and loops it.
    pub fn cue_key(&mut self) {
        if !self.can_play() {
            return;
        }
        let t = self.position();
        match self.cue.recording.take() {
            Some(start) => {
                let (start, end) = if t < start { (t, start) } else { (start, t) };
                if end - start < 0.5 {
                    self.toast(ToastKind::Info, "Cue too short. Press Q to start a new one.");
                    return;
                }
                let n = self.session.cues.len() + 1;
                self.session.cues.push(crate::model::Cue { name: format!("Cue {n}"), start, end, mix: None });
                self.activate_cue(self.session.cues.len() - 1);
                self.save_song_state();
            }
            None => {
                self.exit_cue();
                self.cue.recording = Some(t);
            }
        }
    }

    /// Loop cue `i` and switch the mixer to its mix.
    pub fn activate_cue(&mut self, i: usize) {
        let Some(cue) = self.session.cues.get(i).cloned() else { return };
        if self.cue.active.is_some() {
            self.exit_cue();
        }
        // The cue's mix wins over any section while it loops.
        self.leave_section();
        self.cue.recording = None;
        let base = SongMixer::capture(&self.session.tracks, self.master_db).tracks;
        if let Some(mix) = &cue.mix {
            crate::model::apply_track_mix(mix, &mut self.session.tracks);
        }
        self.cue.base = Some(base);
        self.cue.active = Some(i);
        self.playback.set_loop(Some((cue.start, cue.end)));
        let pos = self.position();
        if pos < cue.start || pos >= cue.end {
            self.playback.seek(cue.start);
        }
        self.sync_mix();
    }

    /// Stop looping; the song's mix comes back and playback carries on.
    pub fn exit_cue(&mut self) {
        let Some(i) = self.cue.active.take() else { return };
        let base = self.cue.base.take().unwrap_or_default();
        if !self.session.tracks.is_empty() {
            let current = SongMixer::capture(&self.session.tracks, self.master_db).tracks;
            if let Some(c) = self.session.cues.get_mut(i) {
                c.mix = (current != base).then_some(current);
            }
            crate::model::apply_track_mix(&base, &mut self.session.tracks);
        }
        self.playback.set_loop(None);
        self.sync_mix();
        self.save_song_state();
    }

    pub fn delete_cue(&mut self, i: usize) {
        if self.cue.active == Some(i) {
            self.exit_cue();
        }
        if i < self.session.cues.len() {
            self.session.cues.remove(i);
            if let Some(a) = self.cue.active
                && a > i
            {
                self.cue.active = Some(a - 1);
            }
            self.save_song_state();
        }
    }

    /// Make the active cue use the song's mix again.
    pub fn reset_cue_mix(&mut self) {
        if let (Some(i), Some(base)) = (self.cue.active, self.cue.base.clone()) {
            crate::model::apply_track_mix(&base, &mut self.session.tracks);
            if let Some(c) = self.session.cues.get_mut(i) {
                c.mix = None;
            }
            self.sync_mix();
        }
    }

    /// Move a cue's start or end (keeps at least half a second).
    pub fn set_cue_bound(&mut self, i: usize, start: bool, t: f64) {
        let Some(c) = self.session.cues.get_mut(i) else { return };
        if start {
            c.start = t.min(c.end - 0.5).max(0.0);
        } else {
            c.end = t.max(c.start + 0.5);
        }
        let region = (c.start, c.end);
        if self.cue.active == Some(i) {
            self.playback.set_loop(Some(region));
        }
        self.save_song_state();
    }

    fn reset_cues(&mut self) {
        self.cue = CueState::default();
        self.section = SectionState::default();
        self.playback.set_loop(None);
    }

    // ------------------------------------------------------------------ sections

    /// S: first press marks a section's start, second press closes it.
    /// Playback carries on either way.
    pub fn section_key(&mut self) {
        if !self.can_play() {
            return;
        }
        let t = self.position();
        let Some(start) = self.section.recording.take() else {
            self.section.recording = Some(t);
            return;
        };
        let (start, end) = if t < start { (t, start) } else { (start, t) };
        if end - start < 0.5 {
            self.toast(ToastKind::Info, "Section too short. Press S to start a new one.");
            return;
        }
        let n = (1..).find(|n| !self.session.sections.iter().any(|s| s.name == format!("Section {n}"))).unwrap_or(1);
        let name = format!("Section {n}");
        self.leave_section();
        crate::model::insert_section(
            &mut self.session.sections,
            crate::model::Section { name: name.clone(), start, end, mix: None },
        );
        self.follow_sections();
        self.sync_mix();
        self.save_song_state();
        self.toast(ToastKind::Info, format!("{name} added. Mute or change volumes while it plays, or right-click it."));
    }

    /// The song's own stem settings (not a cue's or a section's).
    pub fn song_mix(&self) -> Vec<crate::model::TrackMix> {
        self.section
            .base
            .clone()
            .or_else(|| self.cue.base.clone())
            .unwrap_or_else(|| SongMixer::capture(&self.session.tracks, self.master_db).tracks)
    }

    /// Section `i`'s effective stem settings.
    pub fn section_mix(&self, i: usize) -> Vec<crate::model::TrackMix> {
        if self.section.current == Some(i) {
            return SongMixer::capture(&self.session.tracks, self.master_db).tracks;
        }
        match self.session.sections.get(i).and_then(|s| s.mix.clone()) {
            Some(m) => m,
            None => self.song_mix(),
        }
    }

    /// Show the mix of the section under the playhead on the mixer strips
    /// (the audio thread already switches on its own, sample-accurately).
    fn follow_sections(&mut self) {
        let want = if self.cue.active.is_none() && !self.session.tracks.is_empty() {
            crate::model::section_at(&self.session.sections, self.position())
        } else {
            None
        };
        if want == self.section.current {
            return;
        }
        self.leave_section();
        let Some(i) = want else { return };
        self.section.base = Some(SongMixer::capture(&self.session.tracks, self.master_db).tracks);
        if let Some(mix) = self.session.sections[i].mix.clone() {
            crate::model::apply_track_mix(&mix, &mut self.session.tracks);
        }
        self.section.current = Some(i);
        self.sync_mix();
    }

    /// Put the song's mix back on the strips, keeping edits made inside the
    /// section as the section's own mix.
    fn leave_section(&mut self) {
        let Some(i) = self.section.current.take() else { return };
        let base = self.section.base.take().unwrap_or_default();
        if !self.session.tracks.is_empty() {
            let current = SongMixer::capture(&self.session.tracks, self.master_db).tracks;
            if let Some(s) = self.session.sections.get_mut(i) {
                s.mix = (current != base).then_some(current);
            }
            crate::model::apply_track_mix(&base, &mut self.session.tracks);
        }
        self.sync_mix();
        self.save_song_state();
    }

    /// Change one stem inside section `i` (from its menu, wherever the playhead is).
    pub fn edit_section_stem(&mut self, i: usize, kind: StemKind, edit: impl FnOnce(&mut crate::model::TrackMix)) {
        if i >= self.session.sections.len() {
            return;
        }
        if self.section.current == Some(i) {
            if let Some(t) = self.session.tracks.iter_mut().find(|t| t.kind == kind) {
                let mut m = crate::model::TrackMix { kind, gain_db: t.gain_db, pan: t.pan, muted: t.muted, solo: t.solo };
                edit(&mut m);
                (t.gain_db, t.pan, t.muted, t.solo) = (m.gain_db, m.pan, m.muted, m.solo);
            }
        } else {
            let mut mix = self.section_mix(i);
            if let Some(m) = mix.iter_mut().find(|m| m.kind == kind) {
                edit(m);
            }
            let song = self.song_mix();
            self.session.sections[i].mix = (mix != song).then_some(mix);
        }
        self.sync_mix();
        self.save_song_state();
    }

    /// Make section `i` use the song's mix again.
    pub fn reset_section_mix(&mut self, i: usize) {
        if self.section.current == Some(i)
            && let Some(base) = self.section.base.clone()
        {
            crate::model::apply_track_mix(&base, &mut self.session.tracks);
        }
        if let Some(s) = self.session.sections.get_mut(i) {
            s.mix = None;
        }
        self.sync_mix();
        self.save_song_state();
    }

    pub fn delete_section(&mut self, i: usize) {
        if self.section.current == Some(i) {
            // Drop its mix rather than keeping the strips' edits.
            self.reset_section_mix(i);
        }
        self.leave_section();
        if i < self.session.sections.len() {
            self.session.sections.remove(i);
        }
        self.follow_sections();
        self.sync_mix();
        self.save_song_state();
    }

    /// Move a section's start or end (keeps half a second, never overlaps a neighbour).
    pub fn set_section_bound(&mut self, i: usize, start: bool, t: f64) {
        if i >= self.session.sections.len() {
            return;
        }
        self.leave_section();
        let lo = if i > 0 { self.session.sections[i - 1].end } else { 0.0 };
        let hi = self.session.sections.get(i + 1).map_or(self.session.duration().max(t), |n| n.start);
        let s = &mut self.session.sections[i];
        if start {
            s.start = t.clamp(lo, s.end - 0.5);
        } else {
            s.end = t.clamp(s.start + 0.5, hi.max(s.start + 0.5));
        }
        self.follow_sections();
        self.sync_mix();
        self.save_song_state();
    }

    /// Gains and pans for the playback engine from per-stem settings,
    /// with mute and solo applied.
    fn stem_gains(&self, mix: &[crate::model::TrackMix]) -> (Vec<f32>, Vec<f32>) {
        let tracks: Vec<crate::model::TrackMix> = self
            .session
            .tracks
            .iter()
            .map(|t| {
                let own = crate::model::TrackMix { kind: t.kind, gain_db: t.gain_db, pan: t.pan, muted: t.muted, solo: t.solo };
                mix.iter().find(|m| m.kind == t.kind).cloned().unwrap_or(own)
            })
            .collect();
        let any_solo = tracks.iter().any(|t| t.solo);
        let gains = tracks
            .iter()
            .map(|t| {
                let on = if any_solo { t.solo } else { !t.muted };
                if on && t.gain_db > crate::model::MIN_DB { 10f32.powf(t.gain_db / 20.0) } else { 0.0 }
            })
            .collect();
        (gains, tracks.iter().map(|t| t.pan).collect())
    }

    /// What the engine plays inside each section that has its own mix
    /// (nothing while a cue loops: its mix wins).
    pub fn section_schedule(&self) -> Vec<crate::engine::SectionMix> {
        if self.cue.active.is_some() || self.session.tracks.is_empty() {
            return Vec::new();
        }
        let song = self.song_mix();
        (0..self.session.sections.len())
            .filter_map(|i| {
                let s = &self.session.sections[i];
                let mix = if self.section.current == Some(i) { self.section_mix(i) } else { s.mix.clone()? };
                if mix == song {
                    return None;
                }
                let (gains, pans) = self.stem_gains(&mix);
                Some(crate::engine::SectionMix { start: s.start, end: s.end, gains, pans })
            })
            .collect()
    }

    // ------------------------------------------------------------------ export

    /// Ask where to save, then start rendering the lyric video.
    pub fn export_lyric_video(&mut self, ctx: &egui::Context) {
        use crate::export::{BgKind, ExportAudio, ExportSpec};
        let Some(ffmpeg) = self.ffmpeg.path() else {
            self.ffmpeg.install(ctx);
            return;
        };
        let Some(song) = self.current_song().cloned() else { return };
        let title = if self.session.lyrics.title.trim().is_empty() { song.title.clone() } else { self.session.lyrics.title.clone() };
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export lyric video")
            .add_filter("MP4 video", &["mp4"])
            .set_file_name(format!("{} - Lyric Video.mp4", workspace::sanitize_file_name(&title)));
        if let Some(p) = self.project_id().and_then(|id| self.store.project(id)) {
            dialog = dialog.set_directory(self.ws.project_dir(&p));
        }
        let Some(mut dest) = dialog.save_file() else { return };
        if dest.extension().is_none() {
            dest.set_extension("mp4");
        }
        let panel = &self.export;
        let mix = (panel.audio == ExportAudio::Mix && !self.session.tracks.is_empty()).then(|| {
            // The song's mix, switching to each section's mix inside it, as on playback.
            let lr = |gains: &[f32], pans: &[f32]| -> Vec<[f32; 2]> {
                gains.iter().zip(pans).map(|(&g, &p)| crate::engine::pan_gains(g, p)).collect()
            };
            let (gains, pans) = self.stem_gains(&self.song_mix());
            let tracks = self.session.tracks.iter().filter_map(|t| t.audio.clone()).zip(lr(&gains, &pans)).collect::<Vec<_>>();
            let sections = self.section_schedule().into_iter().map(|s| (s.start, s.end, lr(&s.gains, &s.pans))).collect();
            crate::export::Mixdown { tracks, master: 10f32.powf(self.master_db / 20.0), sections }
        });
        let audio = match &mix {
            Some(_) => std::env::temp_dir().join(format!("stageweave-mix-{}-{}.wav", std::process::id(), song.id)),
            None => self.ws.abs(&song.path),
        };
        let background = self.session.lyrics.style.background.file().map(|f| {
            let kind = if matches!(self.session.lyrics.style.background, crate::model::Background::Video { .. }) { BgKind::Video } else { BgKind::Image };
            (crate::backgrounds::dir(&self.ws.root()).join(f), kind)
        });
        if let Some((p, _)) = &background
            && !p.is_file()
        {
            self.toast(ToastKind::Error, format!("Background file missing: {}", p.display()));
            return;
        }
        let height = panel.height;
        let spec = ExportSpec {
            lyrics: self.session.lyrics.clone(),
            duration: self.session.duration(),
            width: height * 16 / 9,
            height,
            fps: panel.fps,
            audio,
            mix,
            background,
            ffmpeg,
            dest,
        };
        self.export.result = None;
        self.export.job = Some(crate::export::start(spec, ctx.clone()));
    }

    fn poll_export(&mut self) {
        let Some(job) = &self.export.job else { return };
        if let Some(r) = job.poll() {
            self.export.job = None;
            match &r {
                Ok(p) => self.toast(ToastKind::Success, format!("Exported {}", p.file_name().unwrap_or_default().to_string_lossy())),
                Err(e) if e == "Export cancelled" => {}
                Err(e) => self.toast(ToastKind::Error, e.clone()),
            }
            self.export.result = Some(r);
        }
    }

    // ------------------------------------------------------------------ project archives

    fn start_archive_job(&mut self, title: &'static str, ctx: &egui::Context, work: impl FnOnce(&dyn Fn(f32, &str)) -> Result<ArchiveDone, String> + Send + 'static) {
        let progress = Arc::new(std::sync::Mutex::new((0.0, String::new())));
        let done = Arc::new(std::sync::Mutex::new(None));
        let (p, d, c) = (progress.clone(), done.clone(), ctx.clone());
        std::thread::spawn(move || {
            let report = |f: f32, what: &str| {
                *p.lock().unwrap() = (f, what.to_owned());
                c.request_repaint();
            };
            let r = work(&report);
            *d.lock().unwrap() = Some(r);
            c.request_repaint();
        });
        self.archive_job = Some(ArchiveJob { title, progress, done });
    }

    /// Save a project as a .zip that can be imported on another computer.
    pub fn export_project_zip(&mut self, id: i64, include_stems: bool, ctx: &egui::Context) {
        let Some(p) = self.store.project(id) else { return };
        let Some(mut dest) = rfd::FileDialog::new()
            .set_title("Export project")
            .add_filter("StageWeave project", &["zip"])
            .set_file_name(format!("{}.zip", workspace::sanitize_file_name(&p.name)))
            .save_file()
        else {
            return;
        };
        if dest.extension().is_none() {
            dest.set_extension("zip");
        }
        self.save_song_state();
        let (store, ws) = (self.store.clone(), self.ws.clone());
        self.start_archive_job("Exporting Project", ctx, move |report| {
            crate::archive::export_project(&store, &ws, id, &dest, &crate::archive::ExportOptions { include_stems }, report)
                .map(|()| ArchiveDone::Exported(dest))
        });
    }

    /// Pick a project .zip and add it to the library.
    pub fn import_project_zip(&mut self, ctx: &egui::Context) {
        let Some(zip) = rfd::FileDialog::new().set_title("Import project").add_filter("StageWeave project", &["zip"]).pick_file() else {
            return;
        };
        self.import_project_from(zip, ctx);
    }

    pub fn import_project_from(&mut self, zip: PathBuf, ctx: &egui::Context) {
        let (store, ws) = (self.store.clone(), self.ws.clone());
        self.start_archive_job("Importing Project", ctx, move |report| {
            crate::archive::import_project(&store, &ws, &zip, report).map(ArchiveDone::Imported)
        });
    }

    fn poll_archive(&mut self) {
        let Some(job) = &self.archive_job else { return };
        let Some(r) = job.done.lock().unwrap().take() else { return };
        self.archive_job = None;
        match r {
            Ok(ArchiveDone::Exported(p)) => {
                self.toast(ToastKind::Success, format!("Saved {}", p.file_name().unwrap_or_default().to_string_lossy()));
            }
            Ok(ArchiveDone::SongsAdded(ids)) => {
                self.reload_projects();
                self.reload_songs();
                self.selected_song = ids.last().copied();
                let n = ids.len();
                self.toast(ToastKind::Success, format!("Added {n} song{} with stems and lyrics", if n == 1 { "" } else { "s" }));
            }
            Ok(ArchiveDone::Imported(id)) => {
                self.reload_projects();
                self.open_project(id);
                self.toast(ToastKind::Success, "Project imported");
            }
            Err(e) => self.toast(ToastKind::Error, e),
        }
    }

    // ------------------------------------------------------------------ library

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast { kind, text: text.into(), born: Instant::now() });
    }

    pub fn reload_projects(&mut self) {
        self.projects = self.store.list_projects();
    }

    pub fn reload_songs(&mut self) {
        self.songs = match self.project_id() {
            Some(p) => self.store.songs(p),
            None => Vec::new(),
        };
        self.split_cache.clear();
        self.song_index = None;
    }

    /// Every song in every project, for search. Cached for a few seconds.
    pub fn song_index(&mut self) -> Arc<Vec<SongRow>> {
        match &self.song_index {
            Some((at, songs)) if at.elapsed().as_secs() < 3 => songs.clone(),
            _ => {
                let songs = Arc::new(self.store.all_songs());
                self.song_index = Some((Instant::now(), songs.clone()));
                songs
            }
        }
    }

    /// Stems on disk for a song in any project (`None` if not split).
    pub fn song_split_stems(&mut self, song: &SongRow) -> Option<usize> {
        let project = self.projects.iter().find(|p| p.id == song.project_id)?.clone();
        let model = self.model_by_key(song.effective_model(&project)).cloned()?;
        let key = (song.id, model.key());
        if !self.split_cache.contains_key(&key) {
            let n = self.ws.song_stems(&project, song, &model.key(), &model.stems).map(|s| s.len());
            self.split_cache.insert(key.clone(), n);
        }
        self.split_cache[&key]
    }

    /// Go to a search result: its project, then the song itself if it can be opened.
    pub fn open_song_anywhere(&mut self, project_id: i64, song_id: i64, ctx: &egui::Context) {
        if self.project_id() != Some(project_id) {
            self.open_project(project_id);
        }
        self.selected_song = Some(song_id);
        if self.songs.iter().any(|s| s.id == song_id && s.imported) {
            self.open_song(song_id, ctx);
        }
    }

    /// Copy songs from other setlists into the current project, with their
    /// stems, lyrics, timing, cues and mix.
    pub fn add_from_library(&mut self, song_ids: Vec<i64>, ctx: &egui::Context) {
        let Some(project) = self.project_id() else { return };
        if song_ids.is_empty() || self.archive_job.is_some() {
            return;
        }
        let (store, ws) = (self.store.clone(), self.ws.clone());
        self.start_archive_job("Adding songs", ctx, move |report| {
            let mut added = Vec::new();
            let n = song_ids.len();
            for (i, id) in song_ids.into_iter().enumerate() {
                let title = store.song(id).map(|s| s.title).unwrap_or_default();
                report(i as f32 / n as f32, &title);
                added.push(crate::song_library::reuse_song(&store, &ws, id, project)?);
            }
            ws.write_manifest(project);
            Ok(ArchiveDone::SongsAdded(added))
        });
    }

    pub fn project_id(&self) -> Option<i64> {
        match self.screen {
            Screen::Home => None,
            Screen::Project(p) | Screen::Song { project: p, .. } => Some(p),
        }
    }

    pub fn current_project(&self) -> Option<&ProjectRow> {
        let id = self.project_id()?;
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn current_song_id(&self) -> Option<i64> {
        match self.screen {
            Screen::Song { song, .. } => Some(song),
            _ => None,
        }
    }

    pub fn current_song(&self) -> Option<&SongRow> {
        let id = self.current_song_id()?;
        self.songs.iter().find(|s| s.id == id)
    }

    pub fn model_by_key(&self, key: &str) -> Option<&SplitModel> {
        self.models.iter().find(|m| m.key() == key)
    }

    /// The model a song will be split with (override, else project default,
    /// else the first available model).
    pub fn effective_model(&self, song: &SongRow) -> SplitModel {
        let key = self.current_project().map(|p| song.effective_model(p).to_owned());
        key.and_then(|k| self.model_by_key(&k).cloned()).unwrap_or_else(|| self.models[0].clone())
    }

    pub fn create_project(&mut self, name: &str) -> i64 {
        let folder = self.ws.new_project_folder(name);
        let default = self.models[0].key();
        let id = match self.store.create_project(name, &folder, &default) {
            Ok(id) => id,
            Err(e) => {
                self.toast(ToastKind::Error, e);
                return -1;
            }
        };
        let _ = std::fs::create_dir_all(self.ws.abs(&folder).join("songs"));
        self.reload_projects();
        id
    }

    pub fn open_project(&mut self, id: i64) {
        if self.project_id() != Some(id) {
            self.setlist_query.clear();
        }
        self.leave_song();
        self.screen = Screen::Project(id);
        self.reload_projects();
        self.reload_songs();
        let songs = self.songs.clone();
        self.importer.resume(&self.ws, &songs);
        self.selected_song = self.songs.first().map(|s| s.id);
    }

    pub fn go_home(&mut self) {
        self.leave_song();
        self.screen = Screen::Home;
        self.reload_projects();
    }

    pub fn rename_project(&mut self, id: i64, name: &str) {
        if let Some(p) = self.projects.iter().find(|p| p.id == id).cloned() {
            self.ws.rename_project(&p, name.trim());
            self.ws.write_manifest(id);
            self.reload_projects();
            self.reload_songs();
        }
    }

    pub fn set_project_default_model(&mut self, key: &str) {
        if let Some(p) = self.current_project().cloned() {
            self.store.update_project(p.id, &p.name, &p.folder, key);
            self.reload_projects();
            self.split_cache.clear();
            self.ws.write_manifest(p.id);
        }
    }

    pub fn delete_project(&mut self, id: i64, delete_files: bool) {
        if let Some(p) = self.projects.iter().find(|p| p.id == id).cloned() {
            for s in self.store.songs(id) {
                self.queue.cancel(s.id);
            }
            if delete_files {
                let _ = std::fs::remove_dir_all(self.ws.project_dir(&p));
            }
            self.store.delete_project(id);
            if self.project_id() == Some(id) {
                self.go_home();
            }
            self.reload_projects();
            self.toast(ToastKind::Info, format!("Deleted {}", p.name));
        }
    }

    pub fn duplicate_project(&mut self, id: i64) {
        let Some(p) = self.projects.iter().find(|p| p.id == id).cloned() else { return };
        let new_id = self.create_project(&format!("{} copy", p.name));
        let Some(np) = self.store.project(new_id) else { return };
        self.store.update_project(new_id, &np.name, &np.folder, &p.default_model);
        let files: Vec<PathBuf> = self.store.songs(id).iter().map(|s| self.ws.abs(&s.path)).collect();
        // Songs inside the workspace are reused in place; copy them so the projects stay independent.
        let copies: Vec<PathBuf> = files.into_iter().filter(|f| f.is_file()).collect();
        let tmp: Vec<PathBuf> = copies
            .iter()
            .filter_map(|f| {
                let dest = workspace::unique_path(&self.ws.project_dir(&np).join("songs"), &f.file_name()?.to_string_lossy());
                std::fs::copy(f, &dest).ok().map(|_| dest)
            })
            .collect();
        self.importer.add_files(&self.ws, &np, &tmp);
        self.reload_projects();
    }

    /// Add audio files (or folders) to the current project.
    pub fn add_songs(&mut self, paths: &[PathBuf]) {
        let files = workspace::collect_audio_files(paths);
        if files.is_empty() {
            self.toast(ToastKind::Error, "No supported audio files found");
            return;
        }
        if !self.ws.exists() && std::fs::create_dir_all(self.ws.root()).is_err() {
            self.toast(ToastKind::Error, format!("Workspace folder unavailable: {}", self.ws.root().display()));
            return;
        }
        let Some(project) = self.current_project().cloned() else { return };
        let ids = self.importer.add_files(&self.ws, &project, &files);
        self.reload_projects();
        self.reload_songs();
        if self.selected_song.is_none() {
            self.selected_song = ids.first().copied();
        }
        self.toast(ToastKind::Info, format!("Adding {} song{}", ids.len(), if ids.len() == 1 { "" } else { "s" }));
    }

    pub fn add_songs_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Add songs")
            .add_filter("Audio", crate::engine::SUPPORTED_EXTENSIONS)
            .pick_files()
        {
            self.add_songs(&files);
        }
    }

    pub fn remove_song(&mut self, id: i64) {
        self.queue.cancel(id);
        if let (Some(song), Some(p)) = (self.songs.iter().find(|s| s.id == id).cloned(), self.current_project().cloned()) {
            // Only delete the file if it's the project's own copy.
            let file = self.ws.abs(&song.path);
            if file.starts_with(self.ws.project_dir(&p)) {
                let _ = std::fs::remove_file(file);
            }
            let stems = self.ws.stem_dir(&p, &song, "x:x");
            if let Some(dir) = stems.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        self.store.remove_song(id);
        if self.selected_song == Some(id) {
            self.selected_song = None;
        }
        self.reload_songs();
        self.reload_projects();
    }

    pub fn move_song(&mut self, id: i64, new_pos: usize) {
        self.store.move_song(id, new_pos as u32);
        self.reload_songs();
        if let Some(p) = self.project_id() {
            self.ws.write_manifest(p);
        }
    }

    pub fn update_song_meta(&mut self, id: i64, meta: &SongMeta) {
        self.store.update_song_meta(id, meta);
        self.reload_songs();
        if let Some(p) = self.project_id() {
            self.ws.write_manifest(p);
        }
    }

    pub fn song_status(&mut self, song: &SongRow) -> SongStatus {
        if let Some(f) = self.importer.progress.get(&song.id) {
            return SongStatus::Importing(*f);
        }
        match self.queue.state(song.id) {
            Some(JobState::Queued) => return SongStatus::Queued,
            Some(JobState::Running { progress, stage, .. }) => {
                return SongStatus::Splitting { progress: *progress, stage: stage.clone() };
            }
            None => {}
        }
        if !song.imported && !self.ws.abs(&song.path).is_file() {
            return SongStatus::Missing;
        }
        let model = self.effective_model(song);
        let key = (song.id, model.key());
        if !self.split_cache.contains_key(&key) {
            let n = self
                .current_project()
                .and_then(|p| self.ws.song_stems(p, song, &model.key(), &model.stems))
                .map(|s| s.len());
            self.split_cache.insert(key.clone(), n);
        }
        match self.split_cache[&key] {
            Some(n) => SongStatus::Split(n),
            None if self.ws.abs(&song.path).is_file() => SongStatus::NotSplit,
            None => SongStatus::Missing,
        }
    }

    pub fn split_song(&mut self, song_id: i64) {
        let Some(song) = self.songs.iter().find(|s| s.id == song_id).cloned() else { return };
        let Some(project) = self.current_project().cloned() else { return };
        if !song.imported {
            return;
        }
        let model = self.effective_model(&song);
        let req = SplitRequest {
            song_id,
            title: song.title.clone(),
            source: self.ws.abs(&song.path),
            out_dir: self.ws.stem_dir(&project, &song, &model.key()),
            model,
        };
        if Some(song_id) == self.current_song_id() {
            self.last_split_error = None;
        }
        self.queue.enqueue(req);
    }

    /// Queue every song that isn't split yet with its effective model.
    pub fn split_all(&mut self) -> usize {
        let pending: Vec<i64> = self
            .songs
            .clone()
            .iter()
            .filter(|s| matches!(self.song_status(s), SongStatus::NotSplit))
            .map(|s| s.id)
            .collect();
        for id in &pending {
            self.split_song(*id);
        }
        pending.len()
    }

    pub fn relocate_song(&mut self, id: i64) {
        if let Some(file) = rfd::FileDialog::new()
            .set_title("Locate song")
            .add_filter("Audio", crate::engine::SUPPORTED_EXTENSIONS)
            .pick_file()
        {
            self.store.remove_song(id);
            self.add_songs(&[file]);
        }
    }

    // ------------------------------------------------------------------ workspace

    pub fn choose_workspace(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Choose workspace folder").pick_folder() {
            if self.projects.is_empty() {
                self.ws.set_root(&dir);
                self.toast(ToastKind::Success, format!("Workspace: {}", dir.display()));
            } else {
                self.dialog = Some(Dialog::ChangeWorkspace { to: dir });
            }
        }
    }

    /// Point at an existing workspace folder (e.g. a reconnected drive).
    pub fn locate_workspace(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Locate workspace folder").pick_folder() {
            self.ws.set_root(&dir);
            self.reload_projects();
            self.reload_songs();
        }
    }

    pub fn start_workspace_move(&mut self, to: PathBuf, ctx: &egui::Context) {
        self.leave_song();
        if !self.queue.is_idle() {
            self.queue.cancel_all();
        }
        self.move_rx = Some(workspace::spawn_move(self.ws.clone(), to, ctx.clone()));
        self.dialog = Some(Dialog::MovingWorkspace { progress: 0.0, file: String::new() });
    }

    fn poll_workspace_move(&mut self) {
        let Some(rx) = &self.move_rx else { return };
        let mut finished = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                MoveEvent::Progress(f, file) => self.dialog = Some(Dialog::MovingWorkspace { progress: f, file }),
                MoveEvent::Done(r) => finished = Some(r),
            }
        }
        if let Some(r) = finished {
            self.move_rx = None;
            self.dialog = None;
            match r {
                Ok(()) => self.toast(ToastKind::Success, format!("Workspace moved to {}", self.ws.root().display())),
                Err(e) => self.toast(ToastKind::Error, format!("Move stopped: {e}. Nothing was deleted.")),
            }
            self.reload_projects();
            self.reload_songs();
        }
    }

    // ------------------------------------------------------------------ open song

    pub fn open_song(&mut self, song_id: i64, ctx: &egui::Context) {
        let Some(project) = self.project_id() else { return };
        if self.current_song_id() == Some(song_id) {
            return;
        }
        self.save_song_state();
        if self.screen == Screen::Project(project) || self.songs.is_empty() {
            self.reload_songs();
        }
        let Some(song) = self.songs.iter().find(|s| s.id == song_id).cloned() else { return };

        self.reset_cues();
        self.playback.pause();
        self.playback.seek(0.0);
        self.playback.set_tracks(Vec::new());
        self.screen = Screen::Song { project, song: song_id };
        self.selected_song = Some(song_id);
        self.stem_load = None;
        self.selected_track = None;
        self.tap_sync = None;
        self.lyric_edit.sel = None;
        self.lyric_edit.editing = None;
        self.lyric_edit.clip_selected = false;
        self.history = LyricHistory::default();
        // AI dialog text/results belong to the previous song.
        self.ai_panel.align_text.clear();
        self.ai_panel.error = None;
        if self.ai_panel.job.is_none() {
            self.ai_panel.result = None;
        }
        self.last_split_error = None;
        self.timeline.scroll = 0.0;
        self.timeline.fit_request = true;

        // Restore lyrics and mixer.
        let mut session = Session::default();
        session.lyrics.raw_text = song.lyrics_raw.clone();
        session.lyrics.title = song.title.clone();
        session.lyrics.artist = song.artist.clone();
        if let Some(lines) = song.lyric_lines.as_deref().and_then(|j| serde_json::from_str::<Vec<LyricLine>>(j).ok()) {
            session.lyrics.lines = lines;
        }
        if let Some(style) = song.lyric_style.as_deref().and_then(|j| serde_json::from_str::<LyricStyle>(j).ok()) {
            session.lyrics.style = style;
        }
        self.session = session;
        self.pending_mixer = song.mixer.as_deref().and_then(|j| serde_json::from_str(j).ok());
        self.master_db = self.pending_mixer.as_ref().map_or(0.0, |m| m.master_db);
        self.session.cues = self.pending_mixer.as_ref().map(|m| m.cues.clone()).unwrap_or_default();
        self.session.sections = self.pending_mixer.as_ref().map(|m| m.sections.clone()).unwrap_or_default();
        self.last_saved_state = self.state_json().unwrap_or_default();

        // Source audio: use the preloaded copy if we have it.
        if let Some((id, src)) = self.preloaded.take()
            && id == song_id
        {
            self.load_job = None;
            self.source_loaded(src, ctx);
        } else {
            let path = self.ws.abs(&song.path);
            if !path.is_file() {
                self.toast(ToastKind::Error, format!("{} is missing from the workspace", song.title));
                self.load_job = None;
                return;
            }
            self.load_job = Some(LoadJob::spawn(self.decoder.clone(), path, song_id, ctx.clone()));
        }
    }

    fn source_loaded(&mut self, source: SourceAudio, ctx: &egui::Context) {
        self.playback.set_tracks(source.audio.iter().cloned().collect());
        self.session.source = Some(source);
        self.sync_mix();
        self.load_stems_if_split(ctx);
        self.preload_next(ctx);
    }

    /// If the open song has stems on disk for its model, load them.
    fn load_stems_if_split(&mut self, ctx: &egui::Context) {
        let Some(song) = self.current_song().cloned() else { return };
        let Some(project) = self.current_project().cloned() else { return };
        let model = self.effective_model(&song);
        if let Some(stems) = self.ws.song_stems(&project, &song, &model.key(), &model.stems) {
            self.stem_load = Some(StemLoadJob::spawn(song.id, stems, ctx.clone()));
        }
    }

    fn preload_next(&mut self, ctx: &egui::Context) {
        let Some(next) = self.neighbour_song(1) else { return };
        if self.preloaded.as_ref().is_some_and(|(id, _)| *id == next.id) || !next.imported {
            return;
        }
        self.preloaded = None;
        self.preload = Some(LoadJob::spawn(self.decoder.clone(), self.ws.abs(&next.path), next.id, ctx.clone()));
    }

    pub fn neighbour_song(&self, step: isize) -> Option<SongRow> {
        let cur = self.current_song_id()?;
        let i = self.songs.iter().position(|s| s.id == cur)? as isize + step;
        (i >= 0).then(|| self.songs.get(i as usize).cloned()).flatten()
    }

    pub fn next_song(&mut self, ctx: &egui::Context) {
        if let Some(s) = self.neighbour_song(1) {
            self.open_song(s.id, ctx);
        }
    }

    pub fn prev_song(&mut self, ctx: &egui::Context) {
        if let Some(s) = self.neighbour_song(-1) {
            self.open_song(s.id, ctx);
        }
    }

    /// Leave the editor, saving the song's state.
    pub fn leave_song(&mut self) {
        self.save_song_state();
        self.reset_cues();
        if let Screen::Song { project, .. } = self.screen {
            self.screen = Screen::Project(project);
        }
        self.playback.pause();
        self.playback.seek(0.0);
        self.playback.set_tracks(Vec::new());
        self.session = Session::default();
        self.load_job = None;
        self.stem_load = None;
        self.pending_mixer = None;
        self.tap_sync = None;
        self.lyric_window_open = false;
        self.lyric_window_fullscreen = false;
    }

    fn state_json(&self) -> Option<String> {
        let mixer = if self.session.tracks.is_empty() {
            // Stems not loaded yet: keep the saved per-stem settings.
            let mut m = self.pending_mixer.clone().unwrap_or_default();
            m.master_db = self.master_db;
            m
        } else {
            SongMixer::capture(&self.session.tracks, self.master_db)
        };
        let mut mixer = mixer;
        mixer.cues = self.session.cues.clone();
        mixer.sections = self.session.sections.clone();
        if let (Some(i), Some(base)) = (self.section.current, &self.section.base) {
            // The strips show the section's mix; the song keeps its own.
            if !self.session.tracks.is_empty()
                && let Some(s) = mixer.sections.get_mut(i)
            {
                s.mix = (mixer.tracks != *base).then(|| mixer.tracks.clone());
            }
            mixer.tracks = base.clone();
        }
        if let (Some(i), Some(base)) = (self.cue.active, &self.cue.base) {
            // While a cue plays, the live mixer is the cue's mix; the song keeps its own.
            if !self.session.tracks.is_empty()
                && let Some(c) = mixer.cues.get_mut(i)
            {
                c.mix = (mixer.tracks != *base).then(|| mixer.tracks.clone());
            }
            mixer.tracks = base.clone();
        }
        let l = &self.session.lyrics;
        serde_json::to_string(&(
            &l.raw_text,
            serde_json::to_string(&l.lines).ok()?,
            serde_json::to_string(&l.style).ok()?,
            serde_json::to_string(&mixer).ok()?,
        ))
        .ok()
    }

    /// Persist lyrics + mixer for the open song if they changed.
    pub fn save_song_state(&mut self) {
        let Some(id) = self.current_song_id() else { return };
        let Some(json) = self.state_json() else { return };
        if json == self.last_saved_state {
            return;
        }
        if let Ok((raw, lines, style, mixer)) = serde_json::from_str::<(String, String, String, String)>(&json) {
            self.store.save_song_state(id, &raw, &lines, &style, &mixer);
            if let Some(s) = self.songs.iter_mut().find(|s| s.id == id) {
                s.lyrics_raw = raw;
                s.lyric_lines = Some(lines);
                s.lyric_style = Some(style);
                s.mixer = Some(mixer);
            }
        }
        self.last_saved_state = json;
    }

    /// Drop the loaded stems and load the ones for the song's current model
    /// (if it has been split with it).
    pub fn reload_stems(&mut self, ctx: &egui::Context) {
        self.leave_section();
        if self.cue.active.is_some() {
            self.exit_cue();
        }
        if !self.session.tracks.is_empty() {
            self.pending_mixer = Some(SongMixer::capture(&self.session.tracks, self.master_db));
        }
        self.session.tracks.clear();
        self.stem_load = None;
        self.selected_track = None;
        if let Some(src) = &self.session.source {
            let pos = self.position();
            self.playback.set_tracks(src.audio.iter().cloned().collect());
            self.playback.seek(pos);
        }
        self.sync_mix();
        self.split_cache.clear();
        self.load_stems_if_split(ctx);
    }

    /// Choose the model for the open song (stored as an override when it
    /// differs from the project default).
    pub fn set_song_model(&mut self, song_id: i64, key: &str) {
        let Some(song) = self.songs.iter().find(|s| s.id == song_id).cloned() else { return };
        let default = self.current_project().map(|p| p.default_model.clone()).unwrap_or_default();
        let meta = SongMeta {
            title: song.title,
            artist: song.artist,
            song_key: song.song_key,
            bpm: song.bpm,
            notes: song.notes,
            model_key: (key != default).then(|| key.to_owned()),
        };
        self.update_song_meta(song_id, &meta);
    }

    // ------------------------------------------------------------------ polling

    fn poll_load(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.load_job
            && let Ok(result) = job.rx.try_recv()
        {
            let tag = job.tag;
            self.load_job = None;
            if Some(tag) == self.current_song_id() {
                match result {
                    Ok(source) => self.source_loaded(source, ctx),
                    Err(e) => self.toast(ToastKind::Error, e),
                }
            }
        }
        if let Some(job) = &self.preload
            && let Ok(result) = job.rx.try_recv()
        {
            let tag = job.tag;
            self.preload = None;
            if let Ok(src) = result {
                self.preloaded = Some((tag, src));
            }
        }
        if let Some(job) = &self.stem_load
            && let Ok(result) = job.rx.try_recv()
        {
            let id = job.song_id;
            self.stem_load = None;
            if Some(id) == self.current_song_id() {
                match result {
                    Ok(stems) => self.install_stems(stems),
                    Err(e) => self.toast(ToastKind::Error, format!("Couldn't load stems: {e}")),
                }
            }
        }
    }

    fn install_stems(&mut self, stems: Vec<StemOutput>) {
        self.session.tracks = stems.into_iter().map(|s| Track::new(s.kind, s.peaks, s.path, s.audio)).collect();
        if let Some(m) = &self.pending_mixer {
            m.apply(&mut self.session.tracks);
        }
        let buffers: Vec<_> = self.session.tracks.iter().filter_map(|t| t.audio.clone()).collect();
        if buffers.len() == self.session.tracks.len() {
            let pos = self.position();
            self.playback.set_tracks(buffers);
            self.playback.seek(pos);
        }
        self.selected_track = Some(0);
        self.timeline.fit_request = true;
        self.sync_mix();
    }

    fn poll_queue(&mut self, ctx: &egui::Context) {
        for f in self.queue.poll() {
            self.split_cache.clear();
            let title = self.songs.iter().find(|s| s.id == f.song_id).map(|s| s.title.clone()).unwrap_or_default();
            match f.result {
                Ok(stems) => {
                    self.toast(ToastKind::Success, format!("Split {title} ({} stems, {:.0}s)", stems.len(), f.seconds));
                    if Some(f.song_id) == self.current_song_id() {
                        if stems.iter().all(|s| s.path.is_some()) {
                            self.load_stems_if_split(ctx);
                        } else {
                            // Mock splitter: peaks only.
                            self.install_stems(stems);
                        }
                    }
                }
                Err(e) if e == "Cancelled" => {}
                Err(e) => {
                    if Some(f.song_id) == self.current_song_id() {
                        self.last_split_error = Some(e.clone());
                    }
                    self.toast(ToastKind::Error, format!("{title}: {e}"));
                }
            }
        }
        for (id, result) in self.importer.poll() {
            if let Err(e) = result {
                let title = self.songs.iter().find(|s| s.id == id).map(|s| s.title.clone()).unwrap_or_default();
                self.toast(ToastKind::Error, format!("Couldn't import {title}: {e}"));
            }
            self.reload_songs();
            self.reload_projects();
            if let Some(p) = self.project_id() {
                self.ws.write_manifest(p);
            }
        }
    }

    // ------------------------------------------------------------------ playback

    pub fn has_tracks(&self) -> bool {
        !self.session.tracks.is_empty()
    }

    pub fn position(&self) -> f64 {
        self.playback.position()
    }

    pub fn is_playing(&self) -> bool {
        self.playback.is_playing()
    }

    /// The original mix can play before it is split.
    pub fn can_play(&self) -> bool {
        self.session.source.is_some() && self.load_job.is_none()
    }

    pub fn is_loading(&self) -> bool {
        self.load_job.is_some() || self.stem_load.is_some()
    }

    pub fn toggle_play(&mut self) {
        if !self.can_play() {
            return;
        }
        if self.playback.is_playing() {
            self.playback.pause();
            if self.lyric_edit.on && self.lyric_edit.pause_marks_start {
                self.lyric_mark_start();
                // Keep the keyboard free so Space resumes playback.
                self.lyric_edit.focus_text = false;
            }
        } else {
            if self.position() >= self.session.duration() - 0.05 {
                self.playback.seek(0.0);
            }
            self.playback.play();
        }
    }

    pub fn stop(&mut self) {
        self.playback.pause();
        self.playback.seek(0.0);
        self.timeline.scroll = 0.0;
    }

    pub fn seek(&mut self, t: f64) {
        let t = t.clamp(0.0, self.session.duration());
        self.playback.seek(t);
    }

    /// Push the current mixer state to the playback engine.
    pub fn sync_mix(&mut self) {
        if self.session.tracks.is_empty() {
            // Only the original mix is loaded.
            let master = 10f32.powf(self.master_db / 20.0);
            self.playback.set_mix(&[1.0], &[0.0], master);
            self.playback.set_sections(&[], master);
            return;
        }
        let gains: Vec<f32> = (0..self.session.tracks.len())
            .map(|i| if self.session.is_audible(i) { self.session.tracks[i].gain_linear() } else { 0.0 })
            .collect();
        let pans: Vec<f32> = self.session.tracks.iter().map(|t| t.pan).collect();
        let master = 10f32.powf(self.master_db / 20.0);
        if self.section.current.is_some() {
            // The strips show a section's mix: outside it the song's plays.
            let (gains, pans) = self.stem_gains(&self.song_mix());
            self.playback.set_mix(&gains, &pans, master);
        } else {
            self.playback.set_mix(&gains, &pans, master);
        }
        let schedule = self.section_schedule();
        self.playback.set_sections(&schedule, master);
    }

    pub fn generate_lyrics(&mut self) {
        self.lyric_checkpoint("Generate Lyrics", None);
        let lines: Vec<String> =
            self.session.lyrics.raw_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        if lines.is_empty() {
            self.toast(ToastKind::Error, "Paste some lyrics first");
            return;
        }
        let vocals = self.session.tracks.iter().find(|t| t.kind == StemKind::Vocals).map(|t| &t.peaks);
        let timed = self.aligner.align(&lines, self.session.duration(), vocals);
        let n = timed.len();
        self.session.lyrics.lines = timed;
        self.monitor_tab = 0;
        self.save_song_state();
        self.toast(ToastKind::Success, format!("Lyric video ready · {n} lines"));
    }

    /// Stamp the current playhead as the start of the next line (tap-sync).
    pub fn tap_sync_stamp(&mut self) {
        let Some(i) = self.tap_sync else { return };
        self.lyric_checkpoint("Tap Sync", None);
        let t = self.position();
        let dur = self.session.duration();
        let lines = &mut self.session.lyrics.lines;
        if i >= lines.len() {
            self.tap_sync = None;
            return;
        }
        lines[i].start = t;
        if i > 0 {
            lines[i - 1].end = t;
        }
        // Push the remaining lines so they stay after the stamp.
        if let Some(next) = lines.get(i + 1).map(|l| l.start)
            && next <= t
        {
            let remaining = lines.len() - i;
            let step = ((dur - t) / remaining as f64).max(0.5);
            for (k, l) in lines.iter_mut().enumerate().skip(i) {
                l.start = t + step * (k - i) as f64;
                l.end = l.start + step;
            }
        }
        if lines[i].end <= t {
            lines[i].end = (t + 3.0).min(dur);
        }
        self.tap_sync = if i + 1 < lines.len() { Some(i + 1) } else { None };
        if self.tap_sync.is_none() {
            self.toast(ToastKind::Success, "Tap sync complete");
        }
    }

    // ------------------------------------------------------------------ AI lyrics

    pub fn vocals_audio(&self) -> Option<Arc<crate::model::AudioBuffer>> {
        self.session.tracks.iter().find(|t| t.kind == StemKind::Vocals).and_then(|t| t.audio.clone())
    }

    /// Open the AI dialog, prefilling the align text from the Lyrics panel.
    pub fn open_ai_dialog(&mut self) {
        self.ai_panel.open = true;
        if self.ai_panel.align_text.trim().is_empty() {
            self.ai_panel.align_text = self.session.lyrics.raw_text.clone();
        }
    }

    /// Vocal pieces when phrase-by-phrase applies (method chosen and song split).
    pub fn ai_pieces(&self) -> Option<Vec<crate::chunking::Piece>> {
        if self.ai_panel.method != AiMethod::Phrases {
            return None;
        }
        let vocals = self.session.tracks.iter().find(|t| t.kind == StemKind::Vocals)?;
        vocals.audio.as_ref()?;
        Some(crate::chunking::build_pieces(&vocals.peaks, self.session.duration()))
    }

    fn ai_audio(&self) -> Option<Arc<crate::model::AudioBuffer>> {
        let vocals = self.vocals_audio().filter(|_| self.ai_panel.use_vocals);
        vocals.or_else(|| self.session.source.as_ref().and_then(|s| s.audio.clone()))
    }

    fn ai_service(&self, need_words: bool) -> Option<crate::ai::Service> {
        let provider = self.ai.provider()?;
        let mut model = self.ai.model(provider);
        if need_words && !model.timestamps {
            // Alignment needs word timing: fall back to the provider's Whisper model.
            model = provider.models().iter().find(|m| m.timestamps)?;
        }
        Some(crate::ai::Service { provider, key: self.ai.key(provider)?, model })
    }

    pub fn start_transcription(&mut self, ctx: &egui::Context) {
        let Some(song_id) = self.current_song_id() else { return };
        let Some(service) = self.ai_service(false) else {
            self.ai_panel.error = Some("Add an OpenAI or Groq API key in Settings → AI".into());
            return;
        };
        let Some(audio) = self.ai_audio() else {
            self.ai_panel.error = Some("The song's audio isn't loaded yet".into());
            return;
        };
        let l = &self.session.lyrics;
        let prompt = match (l.title.trim(), l.artist.trim()) {
            ("", _) => "Song lyrics.".to_owned(),
            (t, "") => format!("Song lyrics: \"{t}\"."),
            (t, a) => format!("Song lyrics: \"{t}\" by {a}."),
        };
        let pieces = self.ai_pieces();
        let language = self.ai.language();
        let (audio, prompt) = match (&pieces, self.vocals_audio()) {
            (Some(_), Some(vocals)) => {
                let lang = if language.is_empty() { String::new() } else { format!(" in {}", crate::ai::language_label(&language)) };
                let title = self.session.lyrics.title.trim();
                (vocals, if title.is_empty() { format!("Song lyrics{lang}.") } else { format!("{title}. Song lyrics{lang}.") })
            }
            _ => (audio, prompt),
        };
        let req = TranscribeRequest {
            mode: crate::ai::JobMode::Transcribe,
            service: Some(service),
            language,
            prompt,
            audio,
            vocals: None,
            pieces,
        };
        self.ai_panel.error = None;
        self.ai_panel.result = None;
        self.ai_panel.job = Some(TranscribeJob::spawn(song_id, req, ctx.clone()));
    }

    /// Time the lyrics in `ai_panel.align_text` against the song.
    pub fn start_alignment(&mut self, ctx: &egui::Context) {
        let Some(song_id) = self.current_song_id() else { return };
        let lyrics = self.ai_panel.align_text.clone();
        if lyrics.trim().is_empty() {
            self.ai_panel.error = Some("Paste the lyrics you want timed".into());
            return;
        }
        let vocals_peaks = self.session.tracks.iter().find(|t| t.kind == StemKind::Vocals).map(|t| t.peaks.clone());
        let service = if self.ai_panel.align_with_whisper {
            match self.ai_service(true) {
                Some(s) => Some(s),
                None => {
                    self.ai_panel.error = Some("Add an OpenAI or Groq API key in Settings → AI, or use On-device".into());
                    return;
                }
            }
        } else {
            if vocals_peaks.is_none() {
                self.ai_panel.error = Some("On-device timing listens to the vocals stem. Split the song first.".into());
                return;
            }
            None
        };
        let Some(audio) = self.ai_audio() else {
            self.ai_panel.error = Some("The song's audio isn't loaded yet".into());
            return;
        };
        let pieces = if service.is_some() { self.ai_pieces() } else { None };
        let audio = if pieces.is_some() { self.vocals_audio().unwrap_or(audio) } else { audio };
        let req = TranscribeRequest {
            mode: crate::ai::JobMode::Align { lyrics: lyrics.clone() },
            service,
            language: self.ai.language(),
            prompt: crate::ai::lyrics_prompt(&self.session.lyrics.title, &lyrics),
            audio,
            vocals: vocals_peaks,
            pieces,
        };
        self.ai_panel.error = None;
        self.ai_panel.result = None;
        self.ai_panel.job = Some(TranscribeJob::spawn(song_id, req, ctx.clone()));
    }

    pub fn cancel_transcription(&mut self) {
        if let Some(job) = self.ai_panel.job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn poll_ai(&mut self) {
        let Some(job) = &mut self.ai_panel.job else { return };
        let mut done = None;
        while let Ok(ev) = job.rx.try_recv() {
            match ev {
                JobEvent::Stage(s) => job.stage = s,
                JobEvent::Done(r) => done = Some(r),
            }
        }
        let Some(result) = done else { return };
        let song = job.song_id;
        self.ai_panel.job = None;
        match result {
            Ok(crate::ai::JobResult::Transcript(t)) if t.text.trim().is_empty() => {
                self.ai_panel.error = Some("No singing was detected. Try the full mix, or set the language.".into());
            }
            Ok(crate::ai::JobResult::Transcript(t)) => {
                let n = if t.lines.is_empty() { t.text.lines().count() } else { t.lines.len() };
                self.toast(ToastKind::Success, format!("Transcribed {n} lines"));
                self.ai_panel.result = Some((song, AiResult::Transcript(t)));
            }
            Ok(crate::ai::JobResult::Alignment(a)) => {
                self.toast(ToastKind::Success, format!("Timed {} lines · {} matched to the singing", a.total(), a.matched));
                self.ai_panel.result = Some((song, AiResult::Alignment(a)));
            }
            Err(e) if e == "Cancelled" => {}
            Err(e) => self.ai_panel.error = Some(e),
        }
    }

    /// Pick a lyrics file (LRC, SRT, ChordPro, OpenLyrics…) for this song.
    pub fn import_lyrics_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import lyrics")
            .add_filter("Lyrics", crate::lyric_import::EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        self.import_lyrics_file(&path);
    }

    /// Replace this song's lyrics with a file's. Synced files keep their
    /// timing; plain ones are timed like pasted lyrics. One undo step.
    pub fn import_lyrics_file(&mut self, path: &Path) {
        if self.current_song_id().is_none() {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return self.toast(ToastKind::Error, format!("Can't read {name}: {e}")),
        };
        let parsed = match crate::lyric_import::parse(&name, &crate::lyric_import::decode(&bytes), self.session.duration()) {
            Ok(p) => p,
            Err(e) => return self.toast(ToastKind::Error, format!("{name}: {e}")),
        };
        self.lyric_checkpoint("Import Lyrics", None);
        self.history.suspended = true;
        let l = &mut self.session.lyrics;
        if l.title.trim().is_empty()
            && let Some(t) = parsed.title
        {
            l.title = t;
        }
        if l.artist.trim().is_empty()
            && let Some(a) = parsed.artist
        {
            l.artist = a;
        }
        l.raw_text = parsed.text;
        match parsed.lines {
            Some(lines) => {
                let n = lines.len();
                self.session.lyrics.lines = lines;
                self.monitor_tab = 0;
                self.save_song_state();
                self.toast(ToastKind::Success, format!("Imported {n} timed lines ({})", parsed.format));
            }
            None => {
                self.generate_lyrics();
                self.toast(ToastKind::Info, format!("Imported from {}. Use AI → Align for timing that follows the singer.", parsed.format));
            }
        }
        self.history.suspended = false;
    }

    /// Use the finished result. For transcripts, `with_timing` = keep Whisper's
    /// timing (else spread evenly). Alignments keep the user's text exactly.
    pub fn apply_ai_result(&mut self, with_timing: bool) {
        let Some((song, result)) = self.ai_panel.result.take() else { return };
        if Some(song) != self.current_song_id() {
            return;
        }
        self.lyric_checkpoint("Apply AI Lyrics", None);
        self.history.suspended = true;
        self.apply_ai_result_inner(result, with_timing);
        self.history.suspended = false;
    }

    fn apply_ai_result_inner(&mut self, result: AiResult, with_timing: bool) {
        match result {
            AiResult::Alignment(a) => {
                self.session.lyrics.raw_text = self.ai_panel.align_text.trim().to_owned();
                let n = a.lines.len();
                self.session.lyrics.lines = a.lines.into_iter().map(|l| l.line).collect();
                self.monitor_tab = 0;
                self.save_song_state();
                self.toast(ToastKind::Success, format!("Lyric video ready · {n} lines timed"));
            }
            AiResult::Transcript(t) if t.lines.is_empty() => {
                // No timestamps: one sentence per line, then the usual aligner.
                let text: Vec<String> = t
                    .text
                    .split_inclusive(['.', '!', '?', '\n'])
                    .map(|s| s.trim().trim_end_matches('.').to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
                self.session.lyrics.raw_text = text.join("\n");
                self.generate_lyrics();
            }
            AiResult::Transcript(t) => {
                self.session.lyrics.raw_text = t.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
                if with_timing {
                    let n = t.lines.len();
                    self.session.lyrics.lines = t.lines;
                    self.monitor_tab = 0;
                    self.save_song_state();
                    self.toast(ToastKind::Success, format!("Lyric video ready · {n} lines timed by AI"));
                } else {
                    self.generate_lyrics();
                }
            }
        }
    }

    // ------------------------------------------------------------------ undo / redo

    fn lyric_snapshot(&self, label: &'static str) -> LyricSnapshot {
        LyricSnapshot {
            label,
            lines: self.session.lyrics.lines.clone(),
            raw_text: self.session.lyrics.raw_text.clone(),
            sel: self.lyric_edit.sel,
        }
    }

    /// Record the lyrics before a change. Calls with the same `coalesce` key
    /// within 1.5 s (a burst of nudges, one drag) become a single step.
    pub fn lyric_checkpoint(&mut self, label: &'static str, coalesce: Option<String>) {
        if self.history.suspended {
            return;
        }
        if let (Some(key), Some((last, at))) = (&coalesce, &mut self.history.last)
            && key == last
            && at.elapsed().as_secs_f32() < 1.5
        {
            *at = Instant::now();
            return;
        }
        let snap = self.lyric_snapshot(label);
        self.history.undo.push(snap);
        if self.history.undo.len() > HISTORY_LIMIT {
            self.history.undo.remove(0);
        }
        self.history.redo.clear();
        self.history.last = coalesce.map(|k| (k, Instant::now()));
    }

    fn restore_snapshot(&mut self, snap: LyricSnapshot) {
        self.session.lyrics.lines = snap.lines;
        self.session.lyrics.raw_text = snap.raw_text;
        let n = self.session.lyrics.lines.len();
        self.lyric_edit.sel = snap.sel.filter(|i| *i < n);
        self.lyric_edit.editing = None;
        self.history.last = None;
        self.save_song_state();
    }

    pub fn undo(&mut self) {
        let Some(snap) = self.history.undo.pop() else { return };
        let label = snap.label;
        let current = self.lyric_snapshot(label);
        self.history.redo.push(current);
        self.restore_snapshot(snap);
        self.toast(ToastKind::Info, format!("Undid: {label}"));
    }

    pub fn redo(&mut self) {
        let Some(snap) = self.history.redo.pop() else { return };
        let label = snap.label;
        let current = self.lyric_snapshot(label);
        self.history.undo.push(current);
        self.restore_snapshot(snap);
        self.toast(ToastKind::Info, format!("Redid: {label}"));
    }

    /// Drop the last step if it was `label` (e.g. an insert that was cancelled).
    fn forget_checkpoint(&mut self, label: &'static str) {
        if self.history.undo_label() == Some(label) {
            self.history.undo.pop();
        }
    }

    // ------------------------------------------------------------------ lyric editing

    /// Enter / leave edit mode on the monitor. Entering while playing pauses
    /// and marks the start of the line being sung.
    pub fn toggle_lyric_edit(&mut self) {
        self.lyric_edit.on = !self.lyric_edit.on;
        if !self.lyric_edit.on {
            return;
        }
        self.lyric_edit.sel = self.session.lyrics.line_for_edit(self.position());
        if self.playback.is_playing() {
            self.playback.pause();
            self.lyric_mark_start();
        }
    }

    /// Selected line, following the playhead while playing.
    pub fn lyric_sel(&mut self) -> Option<usize> {
        let n = self.session.lyrics.lines.len();
        if self.playback.is_playing() {
            self.lyric_edit.sel = self.session.lyrics.line_for_edit(self.position());
        }
        self.lyric_edit.sel = self.lyric_edit.sel.filter(|i| *i < n).or(if n > 0 { Some(0) } else { None });
        self.lyric_edit.sel
    }

    /// After any line edit: rewrite the Lyrics panel text and save the song.
    fn lyric_changed(&mut self) {
        self.session.lyrics.sync_raw_from_lines();
        self.save_song_state();
    }

    /// Timeline: select a lyric clip (and move the playhead to it).
    pub fn lyric_clip_select(&mut self, i: usize) {
        self.lyric_edit.clip_selected = true;
        self.lyric_select(i);
        if let Some(l) = self.session.lyrics.lines.get(i) {
            let t = l.start;
            self.seek(t);
        }
    }

    /// Where a new line would start if added now: the paused playhead,
    /// or `None` while playing (then the gap / split rules apply).
    pub fn lyric_insert_time(&self) -> Option<f64> {
        (!self.is_playing() && self.session.source.is_some()).then(|| self.position())
    }

    pub fn lyric_insert_above(&mut self, i: usize) {
        self.lyric_checkpoint("Insert Line", None);
        let k = match self.lyric_insert_time() {
            Some(t) => self.session.lyrics.insert_at(t),
            None => self.session.lyrics.insert_before(i),
        };
        self.lyric_after_insert(k);
    }

    pub fn lyric_insert_below(&mut self, i: usize) {
        self.lyric_checkpoint("Insert Line", None);
        let k = match self.lyric_insert_time() {
            Some(t) => self.session.lyrics.insert_at(t),
            None => {
                let end = self.session.duration();
                self.session.lyrics.insert_after(i, end)
            }
        };
        self.lyric_after_insert(k);
    }

    fn lyric_after_insert(&mut self, k: usize) {
        self.lyric_edit.edit_in_list = false;
        self.lyric_edit.sel = Some(k);
        self.lyric_edit.clip_selected = true;
        self.lyric_edit.editing = Some((k, String::new()));
        self.lyric_changed();
    }

    pub fn lyric_delete_at(&mut self, i: usize) {
        self.lyric_edit.sel = Some(i);
        self.lyric_edit.editing = None;
        self.lyric_delete();
    }

    /// N: add a line at the paused playhead and type into it. If a line is
    /// already there, ask before replacing it.
    pub fn lyric_new_at_playhead(&mut self) {
        if self.session.source.is_none() {
            return;
        }
        if self.is_playing() {
            self.playback.pause();
        }
        let t = self.position();
        match self.session.lyrics.line_at(t) {
            Some(line) => self.dialog = Some(Dialog::ReplaceLine { line, t }),
            None => self.lyric_add_at(t),
        }
    }

    /// Add a line at `t` (trimming any line it lands in) and type into it.
    pub fn lyric_add_at(&mut self, t: f64) {
        self.lyric_checkpoint("Insert Line", None);
        let k = self.session.lyrics.insert_at(t);
        self.lyric_after_insert(k);
    }

    /// E: stretch the line before the paused playhead so it ends right here.
    pub fn lyric_extend_to_playhead(&mut self) {
        if self.session.source.is_none() {
            return;
        }
        if self.is_playing() {
            self.playback.pause();
        }
        let t = self.position();
        let Some(i) = self.session.lyrics.lines.iter().rposition(|l| l.start < t) else { return };
        if (self.session.lyrics.lines[i].end - t).abs() < 1e-3 {
            return;
        }
        self.lyric_checkpoint("Extend Line", None);
        self.session.lyrics.set_end(i, t);
        self.lyric_edit.sel = Some(i);
        self.lyric_changed();
    }

    /// Confirmed: remove line `i` and start a new one at `t`, ready to type.
    pub fn lyric_replace_at(&mut self, i: usize, t: f64) {
        if i >= self.session.lyrics.lines.len() {
            return;
        }
        self.lyric_checkpoint("Replace Line", None);
        self.history.suspended = true;
        self.lyric_delete_at(i);
        self.history.suspended = false;
        let k = self.session.lyrics.insert_at(t);
        self.lyric_after_insert(k);
    }

    /// Remove a just-inserted line that was left empty, as if never inserted.
    /// (A cancelled replace brings the old line back.)
    fn abandon_empty_insert(&mut self, i: usize) {
        if self.history.undo_label() == Some("Replace Line")
            && let Some(snap) = self.history.undo.pop()
        {
            self.restore_snapshot(snap);
            return;
        }
        self.history.suspended = true;
        self.lyric_delete_at(i);
        self.history.suspended = false;
        self.forget_checkpoint("Insert Line");
    }

    pub fn lyric_begin_text_edit(&mut self, i: usize) {
        if let Some(l) = self.session.lyrics.lines.get(i) {
            self.lyric_edit.sel = Some(i);
            self.lyric_edit.clip_selected = true;
            self.lyric_edit.edit_in_list = false;
            self.lyric_edit.editing = Some((i, l.text.clone()));
        }
    }

    /// Save the inline editor's text. An empty new line is removed instead.
    pub fn lyric_commit_text(&mut self) {
        let Some((i, text)) = self.lyric_edit.editing.take() else { return };
        if i >= self.session.lyrics.lines.len() {
            return;
        }
        let text = text.trim().to_owned();
        if text.is_empty() && self.session.lyrics.lines[i].text.trim().is_empty() {
            self.abandon_empty_insert(i);
            return;
        }
        if text != self.session.lyrics.lines[i].text {
            // A new line's first text belongs to its "Insert Line" step.
            let fresh = matches!(self.history.undo_label(), Some("Insert Line" | "Replace Line"));
            if !self.session.lyrics.lines[i].text.trim().is_empty() || !fresh {
                self.lyric_checkpoint("Edit Text", None);
            }
            self.session.lyrics.lines[i].text = text;
            self.lyric_changed();
        }
    }

    pub fn lyric_cancel_text(&mut self) {
        if let Some((i, _)) = self.lyric_edit.editing.take()
            && self.session.lyrics.lines.get(i).is_some_and(|l| l.text.trim().is_empty())
        {
            // Cancelling a just-inserted empty line removes it.
            self.abandon_empty_insert(i);
        }
    }

    /// Stamp the selected line's start at the playhead (creates a line if none).
    pub fn lyric_mark_start(&mut self) {
        self.lyric_checkpoint("Mark Start", None);
        let t = self.position();
        let lyrics = &mut self.session.lyrics;
        let i = match self.lyric_edit.sel.filter(|i| *i < lyrics.lines.len()) {
            Some(i) => {
                lyrics.set_start(i, t);
                // Sorting may have moved it.
                lyrics.lines.iter().position(|l| (l.start - t).abs() < 1e-9).unwrap_or(i)
            }
            None => lyrics.insert_at(t),
        };
        self.lyric_edit.sel = Some(i);
        self.lyric_edit.focus_text = true;
        self.lyric_changed();
    }

    /// Stamp the selected line's end at the playhead and move to the next line.
    pub fn lyric_mark_end(&mut self) {
        let Some(i) = self.lyric_sel() else { return };
        self.lyric_checkpoint("Mark End", None);
        let t = self.position();
        self.session.lyrics.set_end(i, t);
        if i + 1 < self.session.lyrics.lines.len() {
            self.lyric_edit.sel = Some(i + 1);
        }
        self.lyric_changed();
    }

    pub fn lyric_select(&mut self, i: usize) {
        if let Some(line) = self.session.lyrics.lines.get(i) {
            self.lyric_edit.sel = Some(i);
            if !self.playback.is_playing() {
                let t = line.start;
                self.seek(t);
            }
        }
    }

    /// Set a line's start (Timing list drag), keeping neighbours consistent.
    pub fn lyric_set_start(&mut self, i: usize, t: f64) {
        self.lyric_checkpoint("Move Line", Some(format!("start-{i}")));
        self.session.lyrics.set_start(i, t);
        self.lyric_changed();
    }

    pub fn lyric_nudge(&mut self, start: bool, by: f64) {
        let Some(i) = self.lyric_sel() else { return };
        self.lyric_checkpoint("Nudge Timing", Some(format!("nudge-{i}-{start}")));
        let l = &self.session.lyrics.lines[i];
        if start {
            let t = l.start + by;
            self.session.lyrics.set_start(i, t);
        } else {
            let t = l.end + by;
            self.session.lyrics.set_end(i, t);
        }
        self.lyric_changed();
    }

    pub fn lyric_split(&mut self) {
        let Some(i) = self.lyric_sel() else { return };
        self.lyric_checkpoint("Split Line", None);
        if self.session.lyrics.split_at(i, self.position()) {
            self.lyric_edit.sel = Some(i + 1);
            self.lyric_changed();
        } else {
            self.forget_checkpoint("Split Line");
            self.toast(ToastKind::Info, "Move the playhead inside the line to split it");
        }
    }

    pub fn lyric_merge(&mut self) {
        let Some(i) = self.lyric_sel() else { return };
        if i + 1 >= self.session.lyrics.lines.len() {
            return;
        }
        self.lyric_checkpoint("Merge Lines", None);
        if self.session.lyrics.merge_with_next(i) {
            self.lyric_changed();
        }
    }

    pub fn lyric_delete(&mut self) {
        let Some(i) = self.lyric_sel() else { return };
        self.lyric_checkpoint("Delete Line", None);
        self.session.lyrics.delete(i);
        let n = self.session.lyrics.lines.len();
        self.lyric_edit.sel = if n == 0 { None } else { Some(i.min(n - 1)) };
        self.lyric_changed();
    }

    pub fn toggle_lyric_window(&mut self) {
        self.lyric_window_open = !self.lyric_window_open;
        if !self.lyric_window_open {
            self.lyric_window_fullscreen = false;
        }
    }

    // ------------------------------------------------------------------ per-frame

    fn advance(&mut self, dt: f32) {
        // Seeking out of a looping cue ends it.
        if let Some(c) = self.cue.active.and_then(|i| self.session.cues.get(i)) {
            let pos = self.position();
            if pos < c.start - 0.05 || pos > c.end + 0.6 {
                self.exit_cue();
            }
        }
        self.follow_sections();
        let dur = self.session.duration();
        if self.playback.is_playing() && self.position() >= dur {
            if self.loop_enabled {
                self.playback.seek(0.0);
            } else {
                self.playback.pause();
                self.playback.seek(dur);
            }
        }

        // Meters: fast attack, slow release.
        let playing = self.playback.is_playing();
        let pos = self.position();
        let master = 10f32.powf(self.master_db / 20.0);
        let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
        let release = (dt * 1.8).min(1.0);
        for i in 0..self.session.tracks.len() {
            let audible = self.session.is_audible(i);
            let t = &mut self.session.tracks[i];
            let target = if playing && audible { t.peaks.at(pos) * t.gain_linear() } else { 0.0 };
            t.meter = if target > t.meter { target } else { (t.meter - release).max(target) };
            let pl = (1.0 - t.pan).min(1.0);
            let pr = (1.0 + t.pan).min(1.0);
            sum_l += target * pl;
            sum_r += target * pr;
        }
        let n = (self.session.tracks.len() as f32).sqrt().max(1.0);
        let mut targets = [sum_l / n * master, sum_r / n * master];
        if self.session.tracks.is_empty()
            && playing
            && let Some(src) = &self.session.source
        {
            let v = src.peaks.at(pos) * master;
            targets = [v, v];
        }
        for (m, target) in self.master_meter.iter_mut().zip(targets) {
            *m = if target > *m { target } else { (*m - release).max(target) };
        }

        self.toasts.retain(|t| t.born.elapsed().as_secs_f32() < 4.0);

        // Autosave lyrics / mixer every couple of seconds.
        if self.last_save_check.elapsed().as_secs_f32() > 2.0 {
            self.last_save_check = Instant::now();
            self.save_song_state();
        }
    }

    /// Once the engine is ready, check which models are on disk and look up
    /// download sizes we don't know yet.
    fn refresh_models_when_ready(&mut self, ctx: &egui::Context) {
        if self.models_refreshed {
            return;
        }
        let (Some(env), Some(mm)) = (&self.env, &self.model_manager) else { return };
        if matches!(env.status(), EnvStatus::Ready(_)) {
            self.models_refreshed = true;
            mm.refresh(ctx, self.models.iter().map(|m| m.key()).collect(), true);
        }
    }

    pub fn model_keys(&self) -> Vec<String> {
        self.models.iter().map(|m| m.key()).collect()
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        // Dropped files / folders.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        let (zips, dropped): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")));
        // A lyrics file dropped on an open song replaces its lyrics.
        let is_lyrics = |p: &PathBuf| {
            p.extension().is_some_and(|e| crate::lyric_import::EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
        };
        let (lyric_files, dropped): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|p| matches!(self.screen, Screen::Song { .. }) && is_lyrics(p));
        if let Some(f) = lyric_files.first() {
            self.import_lyrics_file(f);
        }
        if let Some(zip) = zips.into_iter().next()
            && self.archive_job.is_none()
        {
            self.import_project_from(zip, ctx);
        }
        if !dropped.is_empty() && self.dialog.is_none() {
            if self.screen == Screen::Home {
                let id = self.create_project(&crate::ui::home::default_project_name());
                if id >= 0 {
                    self.open_project(id);
                }
            }
            self.add_songs(&dropped);
        }

        use egui::{Key, Modifiers};
        // ⌘K / ⌘F: search everything (works even while typing elsewhere).
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K) | i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.search.open = !self.search.open;
            self.search.cursor = 0;
        }
        if ctx.egui_wants_keyboard_input() || self.search.open || self.library.is_some() {
            return;
        }
        let in_song = matches!(self.screen, Screen::Song { .. });
        // Replace-line prompt: Enter replaces, Esc cancels, nothing else.
        if let Some(Dialog::ReplaceLine { line, t }) = self.dialog {
            let (enter, esc) = ctx.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::NONE, Key::Escape)));
            if enter {
                self.dialog = None;
                self.lyric_replace_at(line, t);
            } else if esc {
                self.dialog = None;
            }
            return;
        }
        let (space, home, end, left, right, l_key, enter, esc, open, pop, settings, models) = ctx.input_mut(|i| {
            (
                in_song && i.consume_key(Modifiers::NONE, Key::Space),
                in_song && i.consume_key(Modifiers::NONE, Key::Home),
                in_song && i.consume_key(Modifiers::NONE, Key::End),
                in_song && i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                in_song && i.consume_key(Modifiers::NONE, Key::ArrowRight),
                in_song && i.consume_key(Modifiers::NONE, Key::L),
                in_song && i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::P),
                i.consume_key(Modifiers::COMMAND, Key::Comma),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::M),
            )
        });
        // Undo / redo (⌘Z, ⇧⌘Z on macOS; Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z elsewhere).
        // Redo is checked first: egui would also match ⇧⌘Z as ⌘Z.
        if in_song {
            let (redo, undo) = ctx.input_mut(|i| {
                let redo = i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) | i.consume_key(Modifiers::COMMAND, Key::Y);
                (redo, i.consume_key(Modifiers::COMMAND, Key::Z))
            });
            if redo {
                self.redo();
            } else if undo {
                self.undo();
            }
        }
        // N: new line at the playhead (anywhere on the song screen).
        if in_song && self.tap_sync.is_none() && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::N)) {
            self.lyric_new_at_playhead();
        }
        // Q: cue start / cue end (then it loops).
        if in_song && self.tap_sync.is_none() && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Q)) {
            self.cue_key();
        }
        // S: section start / section end (playback carries on).
        if in_song && self.tap_sync.is_none() && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::S)) {
            self.section_key();
        }
        // E: previous line runs until the playhead.
        if in_song && self.tap_sync.is_none() && self.dialog.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::E)) {
            self.lyric_extend_to_playhead();
        }
        // Lyric edit mode: I / O marks, ↑ / ↓ select.
        if in_song && self.lyric_edit.on {
            let (i_key, o_key, up, down) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::I),
                    i.consume_key(Modifiers::NONE, Key::O),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                )
            });
            if i_key {
                self.lyric_mark_start();
            }
            if o_key {
                self.lyric_mark_end();
            }
            if (up || down)
                && let Some(sel) = self.lyric_sel()
            {
                let n = self.session.lyrics.lines.len();
                let to = if up { sel.saturating_sub(1) } else { (sel + 1).min(n.saturating_sub(1)) };
                self.lyric_select(to);
            }
        }
        // Song navigation: ⌘→/⌘← and PageDown/PageUp (foot pedals).
        let (next, prev) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::ArrowRight) | i.consume_key(Modifiers::NONE, Key::PageDown),
                i.consume_key(Modifiers::COMMAND, Key::ArrowLeft) | i.consume_key(Modifiers::NONE, Key::PageUp),
            )
        });
        if in_song && next {
            self.next_song(ctx);
        }
        if in_song && prev {
            self.prev_song(ctx);
        }
        if space {
            self.toggle_play();
        }
        if home {
            self.seek(0.0);
            self.timeline.scroll = 0.0;
        }
        if end {
            self.seek(self.session.duration());
        }
        if left {
            self.seek(self.position() - 5.0);
        }
        if right {
            self.seek(self.position() + 5.0);
        }
        if l_key {
            self.loop_enabled = !self.loop_enabled;
        }
        if enter && self.tap_sync.is_some() {
            self.tap_sync_stamp();
        } else if enter
            && self.lyric_edit.clip_selected
            && let Some(i) = self.lyric_edit.sel
        {
            self.lyric_begin_text_edit(i);
        }
        if in_song && self.lyric_edit.clip_selected {
            let del = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete) | i.consume_key(Modifiers::NONE, Key::Backspace));
            if del && let Some(i) = self.lyric_edit.sel {
                self.lyric_delete_at(i);
            }
        }
        if esc {
            // Closest thing first: modals, then modes, then selection.
            if self.ai_panel.open {
                self.ai_panel.open = false;
            } else if self.export.open && self.export.job.is_none() {
                self.export.open = false;
            } else if self.dialog.is_some() && !matches!(self.dialog, Some(Dialog::MovingWorkspace { .. })) {
                self.dialog = None;
            } else if self.tap_sync.is_some() {
                self.tap_sync = None;
            } else if self.section.recording.is_some() {
                self.section.recording = None;
            } else if self.cue.recording.is_some() {
                self.cue.recording = None;
            } else if self.cue.active.is_some() {
                self.exit_cue();
            } else if self.lyric_edit.clip_selected {
                self.lyric_edit.clip_selected = false;
            }
        }
        if open && self.project_id().is_some() {
            self.add_songs_dialog();
        }
        if settings {
            self.show_settings = true;
        }
        if models {
            self.show_models = true;
        }
        if pop && self.session.lyrics.is_generated() {
            self.toggle_lyric_window();
        }
    }
}

impl eframe::App for StageWeaveApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.handle_input(&ctx);
        self.poll_export();
        self.poll_archive();
        self.poll_load(&ctx);
        self.poll_queue(&ctx);
        self.poll_workspace_move();
        self.poll_ai();
        self.refresh_models_when_ready(&ctx);
        self.advance(dt);

        ui::draw(self, ui);
        crate::dev::tick(self, &ctx);

        let meters_active = self.master_meter.iter().any(|m| *m > 0.001);
        let busy = !self.queue.is_idle()
            || !self.importer.progress.is_empty()
            || self.is_loading()
            || self.ai_panel.job.is_some();
        if self.playback.is_playing() || busy || meters_active || !self.toasts.is_empty() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn on_exit(&mut self) {
        self.save_song_state();
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::model::LyricLine;
    use egui_kittest::Harness;

    fn app() -> StageWeaveApp {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.lyrics.lines = ["A", "B", "C"]
            .iter()
            .enumerate()
            .map(|(i, t)| LyricLine { text: (*t).into(), start: i as f64 * 10.0, end: i as f64 * 10.0 + 4.0 })
            .collect();
        app.session.lyrics.raw_text = "A\nB\nC".into();
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app
    }

    fn texts(app: &StageWeaveApp) -> Vec<String> {
        app.session.lyrics.lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn undo_restores_a_deleted_line_and_redo_deletes_it_again() {
        let mut a = app();
        a.lyric_delete_at(1);
        assert_eq!(texts(&a), ["A", "C"]);
        assert_eq!(a.history.undo_label(), Some("Delete Line"));
        a.undo();
        assert_eq!(texts(&a), ["A", "B", "C"]);
        assert_eq!(a.session.lyrics.raw_text, "A\nB\nC", "Lyrics panel text restored too");
        a.redo();
        assert_eq!(texts(&a), ["A", "C"]);
        a.undo();
        // A new edit after undo clears the redo stack.
        a.lyric_delete_at(0);
        assert!(a.history.redo_label().is_none());
    }

    #[test]
    fn nested_actions_are_one_step() {
        let mut a = app();
        // Delete by index goes through lyric_delete: still one step.
        a.lyric_delete_at(2);
        assert_eq!(a.history.undo.len(), 1);
        // Inserting then cancelling an empty line leaves no step behind.
        a.lyric_insert_below(0);
        a.lyric_cancel_text();
        assert_eq!(texts(&a), ["A", "B"]);
        assert_eq!(a.history.undo.len(), 1);
        // Inserting and typing text is a single "Insert Line" step.
        a.lyric_insert_below(0);
        let (k, _) = a.lyric_edit.editing.clone().unwrap();
        a.lyric_edit.editing = Some((k, "new".into()));
        a.lyric_commit_text();
        assert_eq!(a.history.undo_label(), Some("Insert Line"));
        assert_eq!(a.history.undo.len(), 2);
        a.undo();
        assert_eq!(texts(&a), ["A", "B"]);
    }

    #[test]
    fn rapid_nudges_coalesce_into_one_step() {
        let mut a = app();
        a.lyric_edit.sel = Some(1);
        for _ in 0..10 {
            a.lyric_nudge(true, 0.1);
        }
        assert_eq!(a.history.undo.len(), 1);
        a.undo();
        assert!((a.session.lyrics.lines[1].start - 10.0).abs() < 1e-9, "back to the original start");
    }

    #[test]
    fn keyboard_shortcuts_undo_and_redo() {
        let mut a = app();
        a.screen = Screen::Song { project: 0, song: 0 };
        a.lyric_delete_at(0);
        let mut h = Harness::new_ui_state(|ui, app: &mut StageWeaveApp| app.handle_input(ui.ctx()), a);
        h.run_steps(1);
        // ⌘Z on macOS, Ctrl+Z elsewhere (egui's COMMAND).
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        assert_eq!(texts(h.state()), ["A", "B", "C"]);
        // ⇧⌘Z / Ctrl+Shift+Z must redo, not undo again.
        h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
        h.run_steps(2);
        assert_eq!(texts(h.state()), ["B", "C"]);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        // Ctrl+Y (Windows / Linux redo).
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Y);
        h.run_steps(2);
        assert_eq!(texts(h.state()), ["B", "C"]);
    }

    fn n_harness(a: StageWeaveApp) -> Harness<'static, StageWeaveApp> {
        let mut a = a;
        a.screen = Screen::Song { project: 0, song: 0 };
        let mut h = Harness::new_ui_state(|ui, app: &mut StageWeaveApp| app.handle_input(ui.ctx()), a);
        h.run_steps(1);
        h
    }

    #[test]
    fn n_in_a_gap_adds_a_line_at_the_playhead_and_opens_its_editor() {
        let mut a = app();
        a.seek(6.0); // between A (0-4) and B (10-14)
        let mut h = n_harness(a);
        h.key_press(egui::Key::N);
        h.run_steps(2);
        let a = h.state();
        assert!(a.dialog.is_none());
        assert_eq!(a.session.lyrics.lines.len(), 4);
        assert!((a.session.lyrics.lines[1].start - 6.0).abs() < 1e-6);
        assert_eq!(a.lyric_edit.editing, Some((1, String::new())), "ready to type");
    }

    #[test]
    fn n_on_an_existing_line_asks_then_enter_replaces_it() {
        let mut a = app();
        a.seek(11.0); // inside B
        let mut h = n_harness(a);
        h.key_press(egui::Key::N);
        h.run_steps(2);
        assert!(matches!(h.state().dialog, Some(Dialog::ReplaceLine { line: 1, .. })));
        assert_eq!(texts(h.state()), ["A", "B", "C"], "nothing changed yet");
        h.key_press(egui::Key::Enter);
        h.run_steps(2);
        let a = h.state_mut();
        assert!(a.dialog.is_none());
        assert_eq!(texts(a), ["A", "", "C"]);
        assert!((a.session.lyrics.lines[1].start - 11.0).abs() < 1e-6);
        assert_eq!(a.lyric_edit.editing, Some((1, String::new())), "editor open on the new line");
        a.lyric_edit.editing = Some((1, "New".into()));
        a.lyric_commit_text();
        assert_eq!(texts(a), ["A", "New", "C"]);
        assert_eq!(a.history.undo.len(), 1, "replace + typing is one step");
        a.undo();
        assert_eq!(texts(a), ["A", "B", "C"]);
    }

    #[test]
    fn esc_cancels_the_replace_prompt_and_cancelling_the_new_text_restores_the_line() {
        let mut a = app();
        a.seek(11.0);
        let mut h = n_harness(a);
        h.key_press(egui::Key::N);
        h.run_steps(2);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().dialog.is_none());
        assert_eq!(texts(h.state()), ["A", "B", "C"]);
        assert!(h.state().history.undo.is_empty());

        let a = h.state_mut();
        a.lyric_replace_at(1, 11.0);
        a.lyric_cancel_text();
        assert_eq!(texts(a), ["A", "B", "C"], "old line back");
        assert!((a.session.lyrics.lines[1].start - 10.0).abs() < 1e-6);
        assert!(a.history.undo.is_empty() && a.history.redo.is_empty());
    }

    #[test]
    fn e_stretches_the_previous_line_to_the_playhead() {
        let mut a = app();
        a.seek(8.0); // gap after A (0-4), before B (10-14)
        let mut h = n_harness(a);
        h.key_press(egui::Key::E);
        h.run_steps(2);
        let a = h.state_mut();
        assert!((a.session.lyrics.lines[0].end - 8.0).abs() < 1e-6, "A now ends at the playhead");
        assert!((a.session.lyrics.lines[1].start - 10.0).abs() < 1e-6, "B untouched");
        assert_eq!(a.history.undo_label(), Some("Extend Line"));
        a.undo();
        assert!((a.session.lyrics.lines[0].end - 4.0).abs() < 1e-6);
    }

    fn app_with_stems() -> StageWeaveApp {
        let mut a = app();
        a.session.tracks = [StemKind::Vocals, StemKind::Drums]
            .into_iter()
            .map(|k| Track::new(k, crate::model::Peaks::default(), None, None))
            .collect();
        a
    }

    fn saved_mixer(a: &StageWeaveApp) -> SongMixer {
        let json = a.state_json().unwrap();
        let (_, _, _, mixer): (String, String, String, String) = serde_json::from_str(&json).unwrap();
        serde_json::from_str(&mixer).unwrap()
    }

    #[test]
    fn q_marks_a_cue_then_loops_it_with_its_own_mix() {
        let a = app_with_stems();
        let mut h = n_harness(a);
        h.state_mut().seek(10.0);
        h.key_press(egui::Key::Q);
        h.run_steps(1);
        assert_eq!(h.state().cue.recording, Some(10.0));
        h.state_mut().seek(20.0);
        h.key_press(egui::Key::Q);
        h.run_steps(1);
        let a = h.state_mut();
        assert_eq!(a.session.cues.len(), 1);
        assert_eq!((a.session.cues[0].start, a.session.cues[0].end), (10.0, 20.0));
        assert_eq!(a.cue.active, Some(0));
        assert!((a.position() - 10.0).abs() < 0.05, "jumps back to the cue start");

        // Mute the vocals inside the cue: the song's own mix is untouched.
        a.session.tracks[0].muted = true;
        let m = saved_mixer(a);
        assert!(!m.tracks[0].muted, "song mix unchanged");
        assert!(m.cues[0].mix.as_ref().unwrap()[0].muted, "cue keeps its mute");

        a.exit_cue();
        assert!(!a.session.tracks[0].muted, "song mix back after the cue");
        assert!(a.cue.active.is_none());
        a.activate_cue(0);
        assert!(a.session.tracks[0].muted, "cue mix applied again");
    }

    #[test]
    fn s_marks_a_section_that_plays_through_with_its_own_mix() {
        let a = app_with_stems();
        let mut h = n_harness(a);
        h.state_mut().seek(10.0);
        h.key_press(egui::Key::S);
        h.run_steps(1);
        assert_eq!(h.state().section.recording, Some(10.0));
        h.state_mut().seek(20.0);
        h.key_press(egui::Key::S);
        h.run_steps(1);
        let a = h.state_mut();
        assert_eq!(a.session.sections.len(), 1);
        assert_eq!((a.session.sections[0].start, a.session.sections[0].end), (10.0, 20.0));
        assert!((a.position() - 20.0).abs() < 0.05, "no jump, no loop");
        assert!(a.cue.active.is_none());

        // Inside the section, muting the drums belongs to the section.
        a.seek(15.0);
        a.follow_sections();
        assert_eq!(a.section.current, Some(0));
        a.session.tracks[1].muted = true;
        a.sync_mix();
        let m = saved_mixer(a);
        assert!(!m.tracks[1].muted, "song mix unchanged");
        assert!(m.sections[0].mix.as_ref().unwrap()[1].muted, "section keeps its mute");
        let sched = a.section_schedule();
        assert_eq!(sched.len(), 1);
        assert_eq!((sched[0].start, sched[0].end), (10.0, 20.0));
        assert_eq!(sched[0].gains[1], 0.0, "drums silent in the section");
        assert!(sched[0].gains[0] > 0.0);

        // Leaving it puts the song's mix back on the strips.
        a.seek(25.0);
        a.follow_sections();
        assert!(a.section.current.is_none());
        assert!(!a.session.tracks[1].muted);
        assert!(a.session.sections[0].mix.as_ref().unwrap()[1].muted);
        assert_eq!(a.section_schedule().len(), 1, "still scheduled outside it");

        // A looping cue's mix wins over sections.
        a.session.cues.push(crate::model::Cue { name: "Cue 1".into(), start: 12.0, end: 14.0, mix: None });
        a.activate_cue(0);
        a.follow_sections();
        assert!(a.section.current.is_none());
        assert!(a.section_schedule().is_empty());
        a.exit_cue();

        // Editing from the menu while elsewhere; back to the song mix drops it.
        a.edit_section_stem(0, StemKind::Vocals, |t| t.gain_db = -6.0);
        assert_eq!(a.session.sections[0].mix.as_ref().unwrap()[0].gain_db, -6.0);
        a.reset_section_mix(0);
        assert!(a.session.sections[0].mix.is_none());
        assert!(a.section_schedule().is_empty());
    }

    #[test]
    fn esc_cancels_marking_a_section() {
        let mut h = n_harness(app_with_stems());
        h.state_mut().seek(4.0);
        h.key_press(egui::Key::S);
        h.run_steps(1);
        assert!(h.state().section.recording.is_some());
        h.key_press(egui::Key::Escape);
        h.run_steps(1);
        assert!(h.state().section.recording.is_none());
        assert!(h.state().session.sections.is_empty());
    }

    #[test]
    fn esc_exits_a_cue_and_seeking_away_ends_it() {
        let mut a = app_with_stems();
        a.session.cues.push(crate::model::Cue { name: "Cue 1".into(), start: 5.0, end: 8.0, mix: None });
        a.activate_cue(0);
        let mut h = n_harness(a);
        h.key_press(egui::Key::Escape);
        h.run_steps(1);
        assert!(h.state().cue.active.is_none());

        let a = h.state_mut();
        a.activate_cue(0);
        a.seek(30.0);
        a.advance(0.016);
        assert!(a.cue.active.is_none(), "clicking elsewhere on the timeline leaves the cue");
    }

    #[test]
    fn mock_clock_wraps_inside_the_loop() {
        let mut p = crate::engine::mock::MockPlayback::new();
        p.seek(9.9);
        p.set_loop(Some((5.0, 10.0)));
        p.play();
        std::thread::sleep(std::time::Duration::from_millis(150));
        let t = p.position();
        assert!((5.0..5.3).contains(&t), "wrapped: {t}");
    }

    /// Two setlists in a private workspace; "Oceans" (with lyrics and stems) is in the first.
    fn library_app(tag: &str) -> (StageWeaveApp, i64, i64, i64, PathBuf) {
        let mut a = app();
        let dir = std::env::temp_dir().join(format!("sw-lib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        a.ws.set_root(&dir);
        let sunday = a.create_project("Sunday");
        let youth = a.create_project("Youth Night");
        let folder = a.store.project(sunday).unwrap().folder;
        std::fs::create_dir_all(dir.join(&folder).join("songs")).unwrap();
        std::fs::write(dir.join(&folder).join("songs/Oceans.mp3"), b"audio").unwrap();
        let song = a.store.add_song(sunday, &format!("{folder}/songs/Oceans.mp3"), "/m/Oceans.mp3", "Oceans").unwrap();
        a.store.finish_import(song, 240.0, 44_100, &[]);
        a.store.save_song_state(song, "You call me out upon the waters", "[]", "{}", "{}");
        let row = a.store.song(song).unwrap();
        let project = a.store.project(sunday).unwrap();
        let stems = a.ws.stem_dir(&project, &row, &project.default_model);
        std::fs::create_dir_all(&stems).unwrap();
        for k in &a.model_by_key(&project.default_model).unwrap().stems {
            std::fs::write(stems.join(format!("{}.wav", k.label().to_lowercase())), b"v").unwrap();
        }
        a.reload_projects();
        (a, sunday, youth, song, dir)
    }

    #[test]
    fn cmd_k_finds_a_song_by_its_lyrics_and_opens_its_setlist() {
        use egui_kittest::kittest::Queryable;
        let (mut a, sunday, youth, song, dir) = library_app("k");
        a.open_project(youth);
        let mut h = Harness::new_ui_state(
            |ui, app: &mut StageWeaveApp| {
                app.handle_input(ui.ctx());
                crate::ui::search::palette(app, ui.ctx());
            },
            a,
        );
        h.run_steps(1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        h.run_steps(1);
        assert!(h.state().search.open);
        h.state_mut().search.query = "upon the WATERS".into();
        h.run_steps(2);
        h.get_by_label("Song Oceans").click();
        h.run_steps(2);
        let a = h.state();
        assert!(!a.search.open, "closes after picking");
        assert_eq!((a.project_id(), a.selected_song), (Some(sunday), Some(song)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn add_from_library_reuses_a_split_song_with_its_lyrics() {
        use egui_kittest::kittest::Queryable;
        let (mut a, _, youth, _, dir) = library_app("add");
        a.open_project(youth);
        crate::ui::search::open_library(&mut a);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1280.0, 860.0))
            .build_ui_state(|ui, app: &mut StageWeaveApp| crate::ui::search::library_picker(app, ui.ctx()), a);
        h.run_steps(2);
        h.get_by_label("Song Oceans").click();
        h.run_steps(2);
        assert_eq!(h.state().library.as_ref().unwrap().selected.len(), 1);
        h.get_by_label("Add Song").click();
        h.run_steps(2);
        let a = h.state_mut();
        assert!(a.library.is_none());
        for _ in 0..300 {
            a.poll_archive();
            if a.archive_job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(a.songs.len(), 1);
        let added = a.songs[0].clone();
        assert_eq!((added.title.as_str(), added.lyrics_raw.as_str()), ("Oceans", "You call me out upon the waters"));
        assert!(matches!(a.song_status(&added), SongStatus::Split(n) if n > 0), "stems came along");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn importing_an_lrc_keeps_its_timing_and_undoes_in_one_step() {
        let mut a = app();
        a.screen = Screen::Song { project: 0, song: 0 };
        a.session.lyrics.artist.clear();
        let dir = std::env::temp_dir().join(format!("sw-lrc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("oceans.lrc");
        std::fs::write(&f, "[ar:Hillsong UNITED]\n[00:12.00]You call me out\n[00:16.50]Upon the waters\n").unwrap();
        a.import_lyrics_file(&f);
        let got: Vec<(f64, f64, String)> = a.session.lyrics.lines.iter().map(|l| (l.start, l.end, l.text.clone())).collect();
        assert_eq!(got, vec![(12.0, 16.5, "You call me out".into()), (16.5, 24.5, "Upon the waters".into())]);
        assert_eq!(a.session.lyrics.raw_text, "You call me out\nUpon the waters");
        assert_eq!(a.session.lyrics.artist, "Hillsong UNITED");
        a.undo();
        assert_eq!(texts(&a), vec!["A", "B", "C"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
