//! The user-chosen workspace folder: copied songs and generated stems.
//!
//! ```text
//! <workspace>/
//!   Sunday 12 Oct/
//!     songs/Amazing Grace.mp3
//!     stems/amazing-grace-17/htdemucs/vocals.wav …   (file-name slug + song id)
//!     project.json
//! ```
//! All paths in the database are relative to the workspace root (with `/`
//! separators), so the workspace can be moved without breaking anything.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use crate::engine::ffmpeg::Ffmpeg;
use crate::engine::ytdlp::YtDlp;
use crate::engine::{AudioDecoder, SymphoniaDecoder};
use crate::model::{ItemKind, ItemSource, StemKind};
use crate::store::{ProjectRow, SongRow, Store};

pub const SETTING_KEY: &str = "workspace_dir";
const THUMB_PER_SEC: f32 = 10.0;

pub struct Workspace {
    root: Mutex<PathBuf>,
    pub store: Arc<Store>,
}

impl Workspace {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        let root = store.setting(SETTING_KEY).map(PathBuf::from).unwrap_or_else(default_dir);
        Arc::new(Self { root: Mutex::new(root), store })
    }

    pub fn root(&self) -> PathBuf {
        self.root.lock().unwrap().clone()
    }

    pub fn exists(&self) -> bool {
        self.root().is_dir()
    }

    pub fn set_root(&self, dir: &Path) {
        *self.root.lock().unwrap() = dir.to_path_buf();
        self.store.set_setting(SETTING_KEY, &dir.to_string_lossy());
    }

    pub fn abs(&self, rel: &str) -> PathBuf {
        join_rel(&self.root(), rel)
    }

    pub fn project_dir(&self, p: &ProjectRow) -> PathBuf {
        self.abs(&p.folder)
    }

    /// A folder name for a new project that doesn't collide on disk.
    pub fn new_project_folder(&self, name: &str) -> String {
        let base = sanitize_file_name(name);
        let root = self.root();
        let mut candidate = base.clone();
        let mut n = 2;
        while root.join(&candidate).exists() || self.store.list_projects().iter().any(|p| p.folder == candidate) {
            candidate = format!("{base} ({n})");
            n += 1;
        }
        candidate
    }

    /// `<project>/stems/<song-slug>-<id>/<model>/`
    pub fn stem_dir(&self, project: &ProjectRow, song: &SongRow, model_key: &str) -> PathBuf {
        let model_id = model_key.split_once(':').map_or(model_key, |(_, id)| id);
        self.project_dir(project)
            .join("stems")
            // File name + id: stable when the song's title is edited.
            .join(format!("{}-{}", slug(&title_from_path(Path::new(&song.path))), song.id))
            .join(sanitize_model(model_id))
    }

    /// All expected stems if this song has already been split with `model_key`.
    pub fn song_stems(
        &self,
        project: &ProjectRow,
        song: &SongRow,
        model_key: &str,
        expected: &[StemKind],
    ) -> Option<Vec<(StemKind, PathBuf)>> {
        let dir = self.stem_dir(project, song, model_key);
        let stems: Vec<(StemKind, PathBuf)> =
            expected.iter().map(|k| (*k, dir.join(format!("{}.wav", k.label().to_lowercase())))).collect();
        stems.iter().all(|(_, p)| p.is_file()).then_some(stems)
    }

    /// Rename a project's folder on disk and keep song paths in sync.
    pub fn rename_project(&self, project: &ProjectRow, new_name: &str) {
        let mut folder = project.folder.clone();
        let wanted = sanitize_file_name(new_name);
        if wanted != project.folder {
            let target = self.new_project_folder(new_name);
            let from = self.abs(&project.folder);
            if !from.exists() || std::fs::rename(&from, self.abs(&target)).is_ok() {
                self.store.rename_project_folder(project.id, &project.folder, &target);
                folder = target;
            }
        }
        self.store.update_project(project.id, new_name, &folder, &project.default_model);
    }

    /// Write `project.json` so the folder is understandable outside the app.
    pub fn write_manifest(&self, project_id: i64) {
        let Some(p) = self.store.project(project_id) else { return };
        let songs = self.store.songs(project_id);
        let manifest = serde_json::json!({
            "name": p.name,
            "default_model": p.default_model,
            "songs": songs.iter().map(|s| serde_json::json!({
                "title": s.title,
                "artist": s.artist,
                "file": s.path.strip_prefix(&format!("{}/", p.folder)).unwrap_or(&s.path),
                "key": s.song_key,
                "bpm": s.bpm,
                "notes": s.notes,
                "model": s.effective_model(&p),
                "kind": s.kind.as_str(),
            })).collect::<Vec<_>>(),
        });
        let dir = self.project_dir(&p);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join("project.json"), serde_json::to_string_pretty(&manifest).unwrap_or_default());
        }
    }
}

pub fn default_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.audio_dir().map(Path::to_path_buf).or_else(|| Some(u.home_dir().join("Music"))))
        .unwrap_or_else(std::env::temp_dir)
        .join("StageWeave")
}

pub fn join_rel(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|c| !c.is_empty()).fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Path relative to `root` with `/` separators, if `path` is inside it.
pub fn to_rel(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
}

/// Keep names readable but legal on every OS.
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    if cleaned.is_empty() { "Untitled".into() } else { cleaned.chars().take(80).collect() }
}

pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').chars().take(40).collect::<String>();
    if out.is_empty() { "song".into() } else { out }
}

fn sanitize_model(id: &str) -> String {
    id.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// `dir/name.ext`, or `dir/name (2).ext` … if taken.
pub fn unique_path(dir: &Path, file_name: &str) -> PathBuf {
    let p = Path::new(file_name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut candidate = dir.join(format!("{stem}{ext}"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    candidate
}

/// Nice default title from a file name: "05 - Amazing_Grace" → "Amazing Grace".
pub fn title_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let spaced = stem.replace(['_', '.'], " ");
    let trimmed = spaced.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ').trim();
    if trimmed.is_empty() { stem } else { trimmed.split_whitespace().collect::<Vec<_>>().join(" ") }
}

/// Expand dropped folders into the supported audio files they contain.
pub fn collect_audio_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> =
                std::fs::read_dir(p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            entries.sort();
            out.extend(collect_audio_files(&entries));
        } else if crate::engine::is_supported(p) {
            out.push(p.clone());
        }
    }
    out
}

// --------------------------------------------------------------------------- importing

pub enum ImportEvent {
    Progress(i64, f32),
    Done(i64, Result<(), String>),
}

enum ImportTask {
    /// Copy an audio file into the project and analyse it.
    File { song_id: i64, src: PathBuf, dest: PathBuf },
    /// Download a YouTube video's audio (and video) into `dir`.
    YouTube { song_id: i64, url: String, video_id: String, dir: PathBuf, root: PathBuf },
    /// Turn a presentation, PDF or images into a slide deck in `out`.
    Deck { song_id: i64, sources: Vec<PathBuf>, out: PathBuf, root: PathBuf },
}

impl ImportTask {
    fn song_id(&self) -> i64 {
        match self {
            Self::File { song_id, .. } | Self::YouTube { song_id, .. } | Self::Deck { song_id, .. } => *song_id,
        }
    }
}

/// Copies added songs into the workspace, downloads YouTube videos, and
/// analyses them (duration, thumbnail) one at a time in the background.
pub struct Importer {
    tx: Sender<ImportTask>,
    pub events: Receiver<ImportEvent>,
    pub progress: std::collections::HashMap<i64, f32>,
    pub ytdlp: Arc<YtDlp>,
}

impl Importer {
    pub fn new(store: Arc<Store>, repaint: egui::Context, ytdlp: Arc<YtDlp>, ffmpeg: Arc<Ffmpeg>, pdfium: Arc<crate::slides::PdfiumLib>) -> Self {
        let (tx, rx) = channel::<ImportTask>();
        let (etx, events) = channel();
        let yt = ytdlp.clone();
        let pdf = pdfium;
        std::thread::spawn(move || {
            let mut pending: VecDeque<ImportTask> = VecDeque::new();
            while let Some(task) = pending.pop_front().or_else(|| rx.recv().ok()) {
                pending.extend(rx.try_iter());
                let id = task.song_id();
                let report = |f: f32| {
                    let _ = etx.send(ImportEvent::Progress(id, f));
                    repaint.request_repaint();
                };
                let result = match &task {
                    ImportTask::File { src, dest, .. } => import_one(&store, id, src, dest, &report),
                    ImportTask::YouTube { url, video_id, dir, root, .. } => {
                        import_youtube(&store, &yt, &ffmpeg, id, url, video_id, dir, root, &report)
                    }
                    ImportTask::Deck { sources, out, root, .. } => {
                        // Only PowerPoint files carry media that needs ffmpeg.
                        let pptx = sources.first().is_some_and(|p| matches!(crate::slides::extension(p).as_str(), "pptx" | "ppsx"));
                        let ff = if pptx { ffmpeg_now(&ffmpeg, &repaint) } else { None };
                        import_deck(&store, &pdf, ff, id, sources, out, root, &report)
                    }
                };
                if result.is_err() && !matches!(task, ImportTask::File { .. }) {
                    // A failed download or conversion leaves nothing worth keeping.
                    if let ImportTask::Deck { out, .. } = &task {
                        let _ = std::fs::remove_dir_all(out);
                    }
                    store.remove_song(id);
                }
                let _ = etx.send(ImportEvent::Done(id, result));
                repaint.request_repaint();
            }
        });
        Self { tx, events, progress: Default::default(), ytdlp }
    }

    /// Add a slide deck made from `sources` (one presentation or PDF, or
    /// several images) to a project, and queue its conversion.
    pub fn add_deck(&mut self, ws: &Workspace, project: &ProjectRow, sources: &[PathBuf]) -> Result<i64, String> {
        let first = sources.first().ok_or("Nothing to add")?;
        let title = if sources.len() > 1 {
            first.parent().map(title_from_path).filter(|t| !t.is_empty()).unwrap_or_else(|| "Slides".into())
        } else {
            title_from_path(first)
        };
        let decks = ws.project_dir(project).join("decks");
        let _ = std::fs::create_dir_all(&decks);
        let out = unique_path(&decks, &sanitize_file_name(&title));
        let _ = std::fs::create_dir_all(&out);
        let root = ws.root();
        let rel = to_rel(&root, &out).ok_or("Deck is outside the workspace")?;
        let source = ItemSource::Deck { dir: rel.clone(), slides: 0 };
        let id = ws.store.add_item(project.id, ItemKind::Deck, &format!("{rel}/{}", crate::slides::DECK_FILE), &first.to_string_lossy(), &title, &source)?;
        self.progress.insert(id, 0.0);
        let _ = self.tx.send(ImportTask::Deck { song_id: id, sources: sources.to_vec(), out, root });
        Ok(id)
    }

    /// Register files with a project and queue their import. Returns new song ids.
    pub fn add_files(&mut self, ws: &Workspace, project: &ProjectRow, files: &[PathBuf]) -> Vec<i64> {
        let songs_dir = ws.project_dir(project).join("songs");
        let _ = std::fs::create_dir_all(&songs_dir);
        let root = ws.root();
        let mut ids = Vec::new();
        for src in files {
            // Files already in the workspace are used in place.
            let (dest, needs_copy) = match to_rel(&root, src) {
                Some(_) => (src.clone(), false),
                None => {
                    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
                    (unique_path(&songs_dir, &name), true)
                }
            };
            // Reserve the name so a second file with the same name gets " (2)".
            if needs_copy {
                let _ = std::fs::File::create(&dest);
            }
            let rel = to_rel(&root, &dest).unwrap_or_else(|| dest.to_string_lossy().into_owned());
            let title = title_from_path(src);
            if let Ok(id) = ws.store.add_song(project.id, &rel, &src.to_string_lossy(), &title) {
                self.progress.insert(id, 0.0);
                let _ = self.tx.send(ImportTask::File { song_id: id, src: src.clone(), dest });
                ids.push(id);
            }
        }
        ids
    }

    /// Add a YouTube link to a project and queue its download.
    pub fn add_youtube(&mut self, ws: &Workspace, project: &ProjectRow, url: &str) -> Result<i64, String> {
        let video_id = crate::model::youtube_id(url).ok_or("That isn't a YouTube video link")?;
        let url = format!("https://www.youtube.com/watch?v={video_id}");
        let dir = ws.project_dir(project).join("songs");
        let root = ws.root();
        // Placeholder until the download says what it's called.
        let rel = to_rel(&root, &dir.join(format!("{video_id}.m4a"))).unwrap_or_default();
        let source = ItemSource::YouTube { url: url.clone(), video_id: video_id.clone(), video: None, show_video: true };
        let id = ws.store.add_item(project.id, ItemKind::YouTube, &rel, &url, "YouTube video", &source)?;
        self.progress.insert(id, 0.0);
        let _ = self.tx.send(ImportTask::YouTube { song_id: id, url, video_id, dir, root });
        Ok(id)
    }

    /// Re-queue songs whose import never finished (e.g. the app quit mid-copy).
    pub fn resume(&mut self, ws: &Workspace, songs: &[SongRow]) {
        for s in songs {
            if s.imported || self.progress.contains_key(&s.id) {
                continue;
            }
            match (&s.kind, &s.source) {
                (ItemKind::YouTube, Some(ItemSource::YouTube { url, video_id, .. })) => {
                    let dir = ws.abs(&s.path).parent().map(Path::to_path_buf).unwrap_or_default();
                    self.progress.insert(s.id, 0.0);
                    let _ = self.tx.send(ImportTask::YouTube { song_id: s.id, url: url.clone(), video_id: video_id.clone(), dir, root: ws.root() });
                }
                (ItemKind::Deck, Some(ItemSource::Deck { dir, .. })) => {
                    // A presentation or PDF can be converted again; images can't be found.
                    let src = PathBuf::from(&s.original_path);
                    if src.is_file() && crate::slides::is_deck_file(&src) {
                        self.progress.insert(s.id, 0.0);
                        let out = ws.abs(dir);
                        let _ = std::fs::remove_dir_all(&out);
                        let _ = self.tx.send(ImportTask::Deck { song_id: s.id, sources: vec![src], out, root: ws.root() });
                    } else {
                        ws.store.remove_song(s.id);
                    }
                }
                (ItemKind::Song, _) => {
                    let src = PathBuf::from(&s.original_path);
                    if src.is_file() {
                        self.progress.insert(s.id, 0.0);
                        let _ = self.tx.send(ImportTask::File { song_id: s.id, src, dest: ws.abs(&s.path) });
                    }
                }
                _ => {}
            }
        }
    }

    pub fn poll(&mut self) -> Vec<(i64, Result<(), String>)> {
        let mut done = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                ImportEvent::Progress(id, f) => {
                    self.progress.insert(id, f);
                }
                ImportEvent::Done(id, r) => {
                    self.progress.remove(&id);
                    done.push((id, r));
                }
            }
        }
        done
    }
}

/// Duration, sample rate and waveform thumbnail, then mark imported.
fn analyse(store: &Store, song_id: i64, file: &Path) -> Result<(), String> {
    let source = SymphoniaDecoder.load(file)?;
    let audio = source.audio.as_ref().ok_or("No audio")?;
    let thumb: Vec<u8> = audio.peaks(THUMB_PER_SEC).data.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8).collect();
    store.finish_import(song_id, source.info.duration, source.info.sample_rate, &thumb);
    Ok(())
}

/// ffmpeg, downloading it first if needed (blocking; the importer's thread).
fn ffmpeg_now(ffmpeg: &Arc<Ffmpeg>, ctx: &egui::Context) -> Option<PathBuf> {
    use crate::engine::ffmpeg::FfmpegStatus;
    if let Some(p) = ffmpeg.path() {
        return Some(p);
    }
    ffmpeg.install(ctx);
    loop {
        match ffmpeg.status() {
            FfmpegStatus::Ready(p) => return Some(p),
            FfmpegStatus::Downloading(_) => std::thread::sleep(std::time::Duration::from_millis(250)),
            _ => return None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn import_deck(
    store: &Store,
    pdfium: &crate::slides::PdfiumLib,
    ffmpeg: Option<PathBuf>,
    song_id: i64,
    sources: &[PathBuf],
    out: &Path,
    root: &Path,
    report: &dyn Fn(f32),
) -> Result<(), String> {
    let tools = crate::slides::DeckTools { pdfium, ffmpeg };
    let deck = crate::slides::import(sources, out, &tools, &|f, _| report(f))?;
    let dir = to_rel(root, out).ok_or("Deck is outside the workspace")?;
    store.set_item_source(song_id, &ItemSource::Deck { dir, slides: deck.slides.len() });
    store.finish_import(song_id, 0.0, 0, &[]);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn import_youtube(
    store: &Store,
    yt: &YtDlp,
    ffmpeg: &Ffmpeg,
    song_id: i64,
    url: &str,
    video_id: &str,
    dir: &Path,
    root: &Path,
    report: &dyn Fn(f32),
) -> Result<(), String> {
    let exe = yt.ensure(&|f| report(f * 0.05))?;
    report(0.05);
    let info = YtDlp::info(&exe, url)?;
    report(0.08);
    let base = format!("{} [{video_id}]", sanitize_file_name(&info.title).chars().take(60).collect::<String>());
    let ff = ffmpeg.path();
    // m4a (AAC) decodes natively. Some videos only offer combined audio+video;
    // fall back to that. No yt-dlp post-processing: it needs ffprobe next to
    // ffmpeg, which a PATH or static ffmpeg often doesn't have.
    let audio_args = ["-f", "ba[ext=m4a]/ba[ext=mp4]/ba/b[ext=mp4]/b"];
    let mut audio = YtDlp::fetch(&exe, url, dir, &base, &audio_args, &|f| report(0.08 + f * 0.42))?;
    report(0.5);
    // Anything else (e.g. WebM/Opus) is converted to m4a with ffmpeg.
    let native = ["m4a", "mp4", "mp3", "aac", "wav", "flac"];
    if let Some(ff) = &ff
        && !native.contains(&crate::slides::extension(&audio).as_str())
    {
        let out = dir.join(format!("{base}.m4a"));
        let ok = crate::engine::python_env::background_command(ff)
            .args(["-y", "-v", "error", "-i"])
            .arg(&audio)
            .args(["-vn", "-c:a", "aac", "-b:a", "192k"])
            .arg(&out)
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            let _ = std::fs::remove_file(&audio);
            audio = out;
        }
    }
    report(0.55);
    // Video only (the audio above plays it): H.264 up to 720p, decoded by ffmpeg.
    let video_fmt = ["-f", "bv*[height<=720][vcodec^=avc1][ext=mp4]/bv*[height<=720][ext=mp4]/bv*[height<=720]/b[height<=720]/b"];
    let video = YtDlp::fetch(&exe, url, dir, &format!("{base}.video"), &video_fmt, &|f| report(0.55 + f * 0.35)).ok();
    report(0.9);
    let rel = |p: &Path| to_rel(root, p).unwrap_or_else(|| p.to_string_lossy().into_owned());
    // The placeholder name reserved when the link was added.
    let placeholder = dir.join(format!("{video_id}.m4a"));
    if placeholder != audio {
        let _ = std::fs::remove_file(&placeholder);
    }
    store.set_item_file(song_id, &rel(&audio), &info.title, &info.artist);
    let show_video = match store.song(song_id).and_then(|s| s.source) {
        Some(ItemSource::YouTube { show_video, .. }) => show_video,
        _ => true,
    };
    store.set_item_source(
        song_id,
        &ItemSource::YouTube { url: url.to_owned(), video_id: video_id.to_owned(), video: video.as_deref().map(rel), show_video },
    );
    analyse(store, song_id, &audio).map_err(|e| {
        if ff.is_none() { format!("{e} (installing ffmpeg lets StageWeave convert this video's audio)") } else { e }
    })?;
    report(1.0);
    Ok(())
}

fn import_one(store: &Store, song_id: i64, src: &Path, dest: &Path, report: &dyn Fn(f32)) -> Result<(), String> {
    if src != dest {
        copy_with_progress(src, dest, &|f| report(f * 0.8))?;
    }
    report(0.85);
    analyse(store, song_id, dest)?;
    report(1.0);
    Ok(())
}

/// Copy via a `.part` file so an interrupted copy never looks complete.
pub fn copy_with_progress(src: &Path, dest: &Path, report: &dyn Fn(f32)) -> Result<(), String> {
    let total = std::fs::metadata(src).map_err(|e| format!("Can't read {}: {e}", src.display()))?.len().max(1);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let part = dest.with_file_name(format!("{}.part", dest.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    let mut input = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut output = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = input.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n]).map_err(|e| format!("Copy failed: {e}"))?;
        done += n as u64;
        report(done as f32 / total as f32);
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

// --------------------------------------------------------------------------- moving the workspace

pub enum MoveEvent {
    Progress(f32, String),
    Done(Result<(), String>),
}

/// Copy every project folder to `to`, verify, then delete the originals.
pub fn spawn_move(ws: Arc<Workspace>, to: PathBuf, repaint: egui::Context) -> Receiver<MoveEvent> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = move_projects(&ws, &to, &|f, s| {
            let _ = tx.send(MoveEvent::Progress(f, s.to_owned()));
            repaint.request_repaint();
        });
        let _ = tx.send(MoveEvent::Done(result));
        repaint.request_repaint();
    });
    rx
}

fn move_projects(ws: &Workspace, to: &Path, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    let from = ws.root();
    if from == to {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(|e| format!("Can't create {}: {e}", to.display()))?;
    let projects = ws.store.list_projects();
    let files: Vec<(PathBuf, PathBuf)> = projects
        .iter()
        .flat_map(|p| {
            let src_dir = join_rel(&from, &p.folder);
            walk(&src_dir).into_iter().map(move |f| {
                let rel = f.strip_prefix(&src_dir).map(Path::to_path_buf).unwrap_or_default();
                (f, (p.folder.clone(), rel))
            })
        })
        .map(|(f, (folder, rel))| (f, join_rel(to, &folder).join(rel)))
        .collect();
    let total: u64 = files.iter().map(|(f, _)| std::fs::metadata(f).map_or(0, |m| m.len())).sum::<u64>().max(1);
    let mut done = 0u64;
    for (src, dest) in &files {
        let size = std::fs::metadata(src).map_or(0, |m| m.len());
        let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        copy_with_progress(src, dest, &|f| report((done as f32 + f * size as f32) / total as f32, &name))?;
        if std::fs::metadata(dest).map_or(0, |m| m.len()) != size {
            return Err(format!("Verification failed for {}", dest.display()));
        }
        done += size;
    }
    ws.set_root(to);
    for p in &projects {
        let _ = std::fs::remove_dir_all(join_rel(&from, &p.folder));
    }
    report(1.0, "Done");
    Ok(())
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .flat_map(|e| {
            let p = e.path();
            if p.is_dir() { walk(&p) } else { vec![p] }
        })
        .filter(|p| !p.to_string_lossy().ends_with(".part"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stageweave-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn relative_paths_round_trip() {
        let root = Path::new("/ws");
        let p = join_rel(root, "Sunday/songs/a b.mp3");
        assert_eq!(p, Path::new("/ws/Sunday/songs/a b.mp3"));
        assert_eq!(to_rel(root, &p).as_deref(), Some("Sunday/songs/a b.mp3"));
        assert_eq!(to_rel(root, Path::new("/elsewhere/x.mp3")), None);
    }

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(sanitize_file_name("  Sun/day: 12?  "), "Sun-day- 12-");
        assert_eq!(sanitize_file_name("..."), "Untitled");
        assert_eq!(slug("Amazing Grace (Live)!"), "amazing-grace-live");
        assert_eq!(title_from_path(Path::new("/x/05 - Amazing_Grace.mp3")), "Amazing Grace");
        let d = temp_dir("unique");
        std::fs::write(d.join("a.mp3"), b"x").unwrap();
        std::fs::write(d.join("a (2).mp3"), b"x").unwrap();
        assert_eq!(unique_path(&d, "a.mp3"), d.join("a (3).mp3"));
        assert_eq!(unique_path(&d, "b.mp3"), d.join("b.mp3"));
        let _ = std::fs::remove_dir_all(d);
    }

    /// 1 s of 440 Hz stereo 16-bit WAV at 44.1 kHz.
    fn write_wav(path: &Path) {
        let (rate, frames) = (44_100u32, 44_100u32);
        let data_len = frames * 4;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&2u16.to_le_bytes()); // stereo
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 4).to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let v = ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 16_000.0) as i16;
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn importing_copies_and_analyses_songs() {
        let (src_dir, ws_dir) = (temp_dir("import-src"), temp_dir("import-ws"));
        let src = src_dir.join("01 - Test_Song.wav");
        write_wav(&src);
        let store = Arc::new(Store::in_memory().unwrap());
        store.set_setting(SETTING_KEY, &ws_dir.to_string_lossy());
        let ws = Workspace::new(store.clone());
        let pid = store.create_project("Sunday", &ws.new_project_folder("Sunday"), "demucs:htdemucs").unwrap();
        let project = store.project(pid).unwrap();

        let paths = crate::engine::python_env::EnginePaths::new();
        let mut importer = Importer::new(
            store.clone(),
            egui::Context::default(),
            Arc::new(YtDlp::new(&paths)),
            Ffmpeg::new(&paths),
            Arc::new(crate::slides::PdfiumLib::new(&paths)),
        );
        let ids = importer.add_files(&ws, &project, &[src.clone(), src.clone()]);
        assert_eq!(ids.len(), 2);
        let mut done = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while done.len() < 2 && std::time::Instant::now() < deadline {
            done.extend(importer.poll());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:?}");

        let songs = store.songs(pid);
        assert_eq!(songs[0].title, "Test Song");
        assert!(songs.iter().all(|s| s.imported && (s.duration - 1.0).abs() < 0.01 && !s.thumb.is_empty()));
        assert_eq!(songs[0].path, "Sunday/songs/01 - Test_Song.wav");
        assert_eq!(songs[1].path, "Sunday/songs/01 - Test_Song (2).wav", "second copy gets a unique name");
        assert!(ws.abs(&songs[1].path).is_file());
        assert!(src.is_file(), "original left untouched");
        let _ = (std::fs::remove_dir_all(src_dir), std::fs::remove_dir_all(ws_dir));
    }

    #[test]
    fn moving_a_workspace_keeps_projects_working() {
        let (a, b) = (temp_dir("move-a"), temp_dir("move-b"));
        let store = Arc::new(Store::in_memory().unwrap());
        store.set_setting(SETTING_KEY, &a.to_string_lossy());
        let ws = Workspace::new(store.clone());
        let folder = ws.new_project_folder("Sunday");
        let pid = store.create_project("Sunday", &folder, "demucs:htdemucs").unwrap();
        let song_file = a.join("Sunday/songs/x.mp3");
        std::fs::create_dir_all(song_file.parent().unwrap()).unwrap();
        std::fs::write(&song_file, vec![7u8; 5000]).unwrap();
        store.add_song(pid, "Sunday/songs/x.mp3", "/orig/x.mp3", "x").unwrap();

        move_projects(&ws, &b, &|_, _| {}).unwrap();
        assert_eq!(ws.root(), b);
        let song = &store.songs(pid)[0];
        assert_eq!(std::fs::read(ws.abs(&song.path)).unwrap().len(), 5000);
        assert!(!song_file.exists(), "old copy removed");
        let _ = (std::fs::remove_dir_all(a), std::fs::remove_dir_all(b));
    }
}
