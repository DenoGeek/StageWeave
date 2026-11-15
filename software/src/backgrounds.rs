//! Lyric-video backgrounds: the library folder, the starter loops, and the
//! live player that turns an image / looping video into a texture.
//!
//! Files live in `<workspace>/Backgrounds/` and songs refer to them by file
//! name, so a project archive can carry them along.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};

use crate::engine::python_env::background_command;
use crate::model::Background;
use crate::ui::lyrics_view::Backdrop;

pub const DIR_NAME: &str = "Backgrounds";

/// Free stock loops bundled with the app (Mixkit free license, see
/// `assets/backgrounds/CREDITS.md`). Written into the library on first use.
pub struct Starter {
    pub title: &'static str,
    pub file: &'static str,
    pub data: &'static [u8],
    /// Small poster frame for the picker (JPEG).
    pub poster: &'static [u8],
}

pub const STARTERS: [Starter; 3] = [
    Starter {
        title: "Clouds",
        file: "Clouds.mp4",
        data: include_bytes!("../assets/backgrounds/Clouds.mp4"),
        poster: include_bytes!("../assets/backgrounds/Clouds.jpg"),
    },
    Starter {
        title: "Flowers",
        file: "Flowers.mp4",
        data: include_bytes!("../assets/backgrounds/Flowers.mp4"),
        poster: include_bytes!("../assets/backgrounds/Flowers.jpg"),
    },
    Starter {
        title: "Drapes",
        file: "Drapes.mp4",
        data: include_bytes!("../assets/backgrounds/Drapes.mp4"),
        poster: include_bytes!("../assets/backgrounds/Drapes.jpg"),
    },
];

/// Make sure a starter loop is in the library (restores it if deleted or damaged).
pub fn ensure_starter(workspace_root: &Path, s: &Starter) -> Result<(), String> {
    let dest = dir(workspace_root).join(s.file);
    if std::fs::metadata(&dest).is_ok_and(|m| m.len() == s.data.len() as u64) {
        return Ok(());
    }
    std::fs::create_dir_all(dir(workspace_root)).map_err(|e| e.to_string())?;
    let tmp = crate::download::part_path(&dest);
    std::fs::write(&tmp, s.data).map_err(|e| format!("Can't write {}: {e}", dest.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())
}

pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "webm", "mkv"];

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_video(path: &Path) -> bool {
    VIDEO_EXTENSIONS.contains(&ext(path).as_str())
}

pub fn is_image(path: &Path) -> bool {
    IMAGE_EXTENSIONS.contains(&ext(path).as_str())
}

pub fn dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(DIR_NAME)
}

/// The background for a file in the library.
pub fn background_for(file: &str) -> Background {
    if is_video(Path::new(file)) { Background::Video { file: file.to_owned() } } else { Background::Image { file: file.to_owned() } }
}

/// Copy a user's image / video into the library. Returns its file name.
/// The same file picked twice is reused rather than copied again.
pub fn add_file(workspace_root: &Path, src: &Path) -> Result<String, String> {
    if !is_image(src) && !is_video(src) {
        return Err(format!(
            "Choose an image ({}) or a video loop ({})",
            IMAGE_EXTENSIONS.join(", "),
            VIDEO_EXTENSIONS.join(", ")
        ));
    }
    let dir = dir(workspace_root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = crate::workspace::sanitize_file_name(&src.file_name().unwrap_or_default().to_string_lossy());
    let same = dir.join(&name);
    let src_len = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
    if same.is_file() && std::fs::metadata(&same).is_ok_and(|m| m.len() == src_len) {
        return Ok(name);
    }
    let dest = crate::workspace::unique_path(&dir, &name);
    crate::workspace::copy_with_progress(src, &dest, &|_| {})?;
    Ok(dest.file_name().unwrap_or_default().to_string_lossy().into_owned())
}

/// Files in the library, newest first.
pub fn list(workspace_root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir(workspace_root)) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, String)> = rd
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_file() && (is_image(&p) || is_video(&p))
        })
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.file_name().to_string_lossy().into_owned()))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().map(|(_, f)| f).collect()
}

// --------------------------------------------------------------------------- decoding

/// Decode an image file, scaled down to at most 1920 px wide.
pub fn load_image(path: &Path) -> Result<ColorImage, String> {
    let img = image::open(path).map_err(|e| format!("Can't open image: {e}"))?;
    let img = if img.width() > 1920 { img.resize(1920, 1080 * 4, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// One frame of a video (for thumbnails), `width` px wide.
pub fn video_frame(ffmpeg: &Path, path: &Path, at: f64, width: u32) -> Result<ColorImage, String> {
    let height = width * 9 / 16;
    let out = background_command(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{at:.2}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &cover_filter(width, height), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Can't run ffmpeg: {e}"))?;
    let need = (width * height * 4) as usize;
    if out.stdout.len() < need {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &out.stdout[..need]))
}

/// ffmpeg filter: scale to fill `w`×`h` and crop the overflow.
pub fn cover_filter(w: u32, h: u32) -> String {
    format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1")
}

/// A video decoded by ffmpeg in real time and looped forever.
pub struct VideoLoop {
    child: Child,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    pub size: [usize; 2],
}

impl VideoLoop {
    pub const W: u32 = 1280;
    pub const H: u32 = 720;
    pub const FPS: u32 = 25;

    pub fn start(ffmpeg: &Path, path: &Path, ctx: egui::Context) -> Result<Self, String> {
        let (w, h) = (Self::W, Self::H);
        let mut child = background_command(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-re", "-stream_loop", "-1", "-i"])
            .arg(path)
            .args(["-an", "-vf", &format!("{},fps={}", cover_filter(w, h), Self::FPS), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Can't run ffmpeg: {e}"))?;
        let mut out = child.stdout.take().ok_or("no ffmpeg output")?;
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (l, s) = (latest.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut frame = vec![0u8; (w * h * 4) as usize];
            while !s.load(Ordering::Relaxed) && out.read_exact(&mut frame).is_ok() {
                *l.lock().unwrap() = Some(frame.clone());
                ctx.request_repaint();
            }
        });
        Ok(Self { child, latest, stop, size: [w as usize, h as usize] })
    }

    /// The newest frame since the last call.
    pub fn take_frame(&self) -> Option<Vec<u8>> {
        self.latest.lock().unwrap().take()
    }
}

impl Drop for VideoLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A video that follows a clock (the song's playhead) instead of playing
/// on its own: ffmpeg decodes ahead from the current time, and each frame
/// is handed out when the playhead reaches it. Pausing holds the frame;
/// seeking restarts ffmpeg at the new time.
pub struct SyncedVideo {
    ffmpeg: PathBuf,
    path: PathBuf,
    size: [usize; 2],
    ctx: egui::Context,
    decoder: Option<SyncedDecoder>,
    /// Time of the frame last handed out.
    shown: Option<f64>,
}

struct SyncedDecoder {
    child: Child,
    start: f64,
    state: Arc<Mutex<SyncedQueue>>,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
struct SyncedQueue {
    frames: std::collections::VecDeque<(f64, Vec<u8>)>,
    /// Time of the newest decoded frame.
    newest: f64,
    eof: bool,
}

impl SyncedVideo {
    /// Frames decoded ahead of the playhead.
    const AHEAD: usize = 12;

    pub fn new(ffmpeg: &Path, path: &Path, w: u32, h: u32, ctx: egui::Context) -> Self {
        Self { ffmpeg: ffmpeg.to_owned(), path: path.to_owned(), size: [w as usize, h as usize], ctx, decoder: None, shown: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> [usize; 2] {
        self.size
    }

    fn spawn(&mut self, at: f64) {
        self.decoder = None;
        let (w, h) = (self.size[0] as u32, self.size[1] as u32);
        let fps = VideoLoop::FPS as f64;
        let Ok(mut child) = background_command(&self.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", at.max(0.0)), "-i"])
            .arg(&self.path)
            .args(["-an", "-vf", &format!("{},fps={}", cover_filter(w, h), VideoLoop::FPS), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        let Some(mut out) = child.stdout.take() else { return };
        let state = Arc::new(Mutex::new(SyncedQueue { newest: at, ..Default::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp, ctx) = (state.clone(), stop.clone(), self.ctx.clone());
        let start = at.max(0.0);
        std::thread::spawn(move || {
            let mut k = 0u64;
            let mut frame = vec![0u8; (w * h * 4) as usize];
            loop {
                // Stay a few frames ahead of the playhead, no more.
                while !sp.load(Ordering::Relaxed) && st.lock().unwrap().frames.len() >= Self::AHEAD {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if sp.load(Ordering::Relaxed) || out.read_exact(&mut frame).is_err() {
                    break;
                }
                let t = start + k as f64 / fps;
                k += 1;
                let mut q = st.lock().unwrap();
                q.frames.push_back((t, frame.clone()));
                q.newest = t;
                drop(q);
                ctx.request_repaint();
            }
            st.lock().unwrap().eof = true;
            ctx.request_repaint();
        });
        self.decoder = Some(SyncedDecoder { child, start, state, stop });
    }

    /// The frame to show at song time `t`, if it changed since last call.
    pub fn frame_at(&mut self, t: f64) -> Option<Vec<u8>> {
        let restart = match &self.decoder {
            None => true,
            Some(d) => {
                let q = d.state.lock().unwrap();
                let behind = self.shown.is_some_and(|s| t < s - 0.25) || t < d.start - 0.05;
                // Fell far behind (a jump forward, or decoding too slowly).
                let ahead = !q.eof && t > q.newest + 1.0;
                behind || ahead
            }
        };
        if restart {
            self.shown = None;
            self.spawn(t);
        }
        let d = self.decoder.as_ref()?;
        let mut q = d.state.lock().unwrap();
        let mut pick = None;
        while q.frames.front().is_some_and(|(ft, _)| *ft <= t + 0.02) {
            pick = q.frames.pop_front();
        }
        drop(q);
        // Repaint until the next frame is due.
        self.ctx.request_repaint_after(std::time::Duration::from_millis(20));
        let (ft, frame) = pick?;
        self.shown = Some(ft);
        Some(frame)
    }
}

impl Drop for SyncedDecoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// --------------------------------------------------------------------------- live player

/// Keeps the texture for the current song's background up to date.
#[derive(Default)]
pub struct BackdropPlayer {
    showing: Option<PathBuf>,
    texture: Option<TextureHandle>,
    video: Option<VideoLoop>,
    /// A YouTube item's video, following the playhead.
    synced: Option<SyncedVideo>,
    pub error: Option<String>,
    /// Thumbnails for the picker, by file name (`None` = failed).
    thumbs: HashMap<String, Option<TextureHandle>>,
}

impl BackdropPlayer {
    /// What to draw for `bg` this frame. Falls back to the theme while a
    /// file is missing or loading.
    pub fn backdrop(&mut self, ctx: &egui::Context, root: &Path, bg: &Background, ffmpeg: Option<&Path>) -> Backdrop {
        if self.synced.is_some() {
            self.stop();
        }
        let Some(file) = bg.file() else {
            self.stop();
            return Backdrop::Theme;
        };
        let path = dir(root).join(file);
        if self.showing.as_deref() != Some(path.as_path()) {
            self.stop();
            self.showing = Some(path.clone());
            // A starter that was deleted (or never written on this computer) comes back.
            if let Some(st) = STARTERS.iter().find(|st| st.file == file) {
                let _ = ensure_starter(root, st);
            }
            if !path.is_file() {
                self.error = Some(format!("{file} is missing from the Backgrounds folder"));
            } else if matches!(bg, Background::Video { .. }) {
                match ffmpeg {
                    Some(ff) => match VideoLoop::start(ff, &path, ctx.clone()) {
                        Ok(v) => self.video = Some(v),
                        Err(e) => self.error = Some(e),
                    },
                    // Try again once ffmpeg is available.
                    None => self.showing = None,
                }
            } else {
                match load_image(&path) {
                    Ok(img) => self.texture = Some(ctx.load_texture("lyric_bg", img, TextureOptions::LINEAR)),
                    Err(e) => self.error = Some(e),
                }
            }
        }
        if let Some(v) = &self.video
            && let Some(frame) = v.take_frame()
        {
            let img = ColorImage::from_rgba_premultiplied(v.size, &frame);
            match &mut self.texture {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("lyric_bg", img, TextureOptions::LINEAR)),
            }
        }
        match &self.texture {
            Some(t) => Backdrop::Texture { id: t.id(), size: t.size_vec2() },
            None => Backdrop::Theme,
        }
    }

    /// A video that follows the playhead (`t`), e.g. a YouTube item's.
    pub fn synced(&mut self, ctx: &egui::Context, ffmpeg: &Path, path: &Path, t: f64) -> Backdrop {
        if self.synced.as_ref().is_none_or(|v| v.path() != path) {
            self.stop();
            self.synced = Some(SyncedVideo::new(ffmpeg, path, VideoLoop::W, VideoLoop::H, ctx.clone()));
        }
        let v = self.synced.as_mut().expect("just set");
        if let Some(frame) = v.frame_at(t) {
            let img = ColorImage::from_rgba_premultiplied(v.size(), &frame);
            match &mut self.texture {
                Some(tex) => tex.set(img, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("lyric_bg", img, TextureOptions::LINEAR)),
            }
        }
        match &self.texture {
            Some(t) => Backdrop::Texture { id: t.id(), size: t.size_vec2() },
            None => Backdrop::Theme,
        }
    }

    fn stop(&mut self) {
        self.synced = None;
        self.video = None;
        self.texture = None;
        self.showing = None;
        self.error = None;
    }

    /// A small preview for the picker (loaded on first request).
    pub fn thumb(&mut self, ctx: &egui::Context, root: &Path, file: &str, ffmpeg: Option<&Path>) -> Option<(egui::TextureId, Vec2)> {
        if !self.thumbs.contains_key(file)
            && let Some(s) = STARTERS.iter().find(|s| s.file == file)
        {
            let img = image::load_from_memory(s.poster).ok().map(|i| {
                let rgba = i.to_rgba8();
                ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
            });
            let tex = img.map(|i| ctx.load_texture(format!("bg_thumb_{file}"), i, TextureOptions::LINEAR));
            self.thumbs.insert(file.to_owned(), tex);
        }
        if !self.thumbs.contains_key(file) {
            let path = dir(root).join(file);
            if !path.is_file() {
                return None;
            }
            let img = if is_video(&path) {
                match ffmpeg {
                    Some(ff) => video_frame(ff, &path, 1.0, 192).ok(),
                    None => return None,
                }
            } else {
                load_image(&path).ok().map(|i| shrink(&i, 192))
            };
            let tex = img.map(|i| ctx.load_texture(format!("bg_thumb_{file}"), i, TextureOptions::LINEAR));
            self.thumbs.insert(file.to_owned(), tex);
        }
        self.thumbs.get(file)?.as_ref().map(|t| (t.id(), t.size_vec2()))
    }

}

/// Nearest-neighbour downscale to `width` (for thumbnails only).
fn shrink(img: &ColorImage, width: usize) -> ColorImage {
    let [w, h] = img.size;
    if w <= width {
        return img.clone();
    }
    let nh = (h * width / w).max(1);
    let mut out = ColorImage::filled([width, nh], egui::Color32::BLACK);
    for y in 0..nh {
        for x in 0..width {
            out.pixels[y * width + x] = img.pixels[(y * h / nh) * w + x * w / width];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    /// Needs ffmpeg: `cargo test synced_video -- --ignored`
    #[test]
    #[ignore]
    fn synced_video_follows_the_clock() {
        let ff = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().map(PathBuf::from).find(|p| p.is_file()).expect("ffmpeg");
        let dir = std::env::temp_dir().join(format!("sw-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rb.mp4");
        // 2 s red, then 2 s blue.
        let ok = background_command(&ff)
            .args(["-y", "-loglevel", "error", "-filter_complex", "color=red:s=64x36:d=2:r=25[a];color=blue:s=64x36:d=2:r=25[b];[a][b]concat=n=2:v=1", "-pix_fmt", "yuv420p"])
            .arg(&file)
            .status()
            .unwrap();
        assert!(ok.success());
        let mut v = SyncedVideo::new(&ff, &file, 64, 36, egui::Context::default());
        let wait = |v: &mut SyncedVideo, t: f64| -> Vec<u8> {
            for _ in 0..200 {
                if let Some(f) = v.frame_at(t) {
                    return f;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("no frame at {t}");
        };
        let red = |f: &[u8]| f[0] > 200 && f[2] < 60;
        let blue = |f: &[u8]| f[2] > 200 && f[0] < 60;
        assert!(red(&wait(&mut v, 0.5)));
        // Paused: same time, no new frame.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(v.frame_at(0.5).is_none());
        // Jump ahead, then back.
        assert!(blue(&wait(&mut v, 3.0)));
        assert!(red(&wait(&mut v, 1.0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_are_classified_and_copied_once() {
        let root = std::env::temp_dir().join(format!("mk-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("Sky.MP4");
        std::fs::write(&src, b"fake video").unwrap();
        let name = add_file(&root, &src).unwrap();
        assert_eq!(name, "Sky.MP4");
        assert_eq!(add_file(&root, &src).unwrap(), name, "same file reused");
        assert_eq!(background_for(&name), Background::Video { file: name.clone() });
        assert_eq!(background_for("a.jpg"), Background::Image { file: "a.jpg".into() });
        assert!(add_file(&root, &root.join("x.txt")).is_err());
        assert_eq!(list(&root), vec![name]);
        ensure_starter(&root, &STARTERS[0]).unwrap();
        assert_eq!(std::fs::read(dir(&root).join("Clouds.mp4")).unwrap().len(), STARTERS[0].data.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cover_uv_crops_the_long_side() {
        let r = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(160.0, 90.0));
        let wide = crate::ui::lyrics_view::cover_uv(vec2(400.0, 100.0), r);
        assert!((wide.width() - (16.0 / 9.0) / 4.0).abs() < 1e-4 && wide.height() == 1.0);
        let tall = crate::ui::lyrics_view::cover_uv(vec2(100.0, 100.0), r);
        assert!(tall.width() == 1.0 && (tall.height() - 9.0 / 16.0).abs() < 1e-4);
    }
}
