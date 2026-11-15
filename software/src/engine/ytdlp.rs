//! yt-dlp: downloads YouTube videos added to a setlist.
//!
//! Uses the standalone build from yt-dlp's GitHub releases, fetched once into
//! `<app data>/engine/yt-dlp/` (YouTube changes often, so it can be updated
//! from Window → Models). A `yt-dlp` on PATH is only a fallback when that
//! download fails: old copies (e.g. from pip) often can't download any more.
//! Downloads run on the importer's background thread.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use super::python_env::{EnginePaths, background_command};

pub struct YtDlp {
    dir: PathBuf,
    /// Serialises installs/updates (the importer and the Models window).
    lock: Mutex<()>,
    /// What Window → Models shows.
    state: Mutex<ToolState>,
}

/// yt-dlp as shown in Window → Models.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ToolState {
    #[default]
    Unknown,
    Checking,
    /// Installed version (`None` = not installed yet).
    Ready(Option<String>),
    Updating(f32),
    Failed(String),
}

/// Release asset for this platform.
fn asset() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", _) => "yt-dlp_macos",
        ("windows", _) => "yt-dlp.exe",
        ("linux", "x86_64") => "yt-dlp_linux",
        ("linux", "aarch64") => "yt-dlp_linux_aarch64",
        _ => return None,
    })
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" }
}

/// What yt-dlp reported about a video.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub title: String,
    pub artist: String,
    pub duration: f64,
}

impl YtDlp {
    pub fn new(paths: &EnginePaths) -> Self {
        Self { dir: paths.root.join("yt-dlp"), lock: Mutex::new(()), state: Mutex::new(ToolState::Unknown) }
    }

    /// Status for the Models window; the first call checks the version in
    /// the background.
    pub fn state(self: &Arc<Self>, ctx: &egui::Context) -> ToolState {
        let mut s = self.state.lock().unwrap();
        if *s == ToolState::Unknown {
            *s = ToolState::Checking;
            let (me, ctx) = (self.clone(), ctx.clone());
            std::thread::spawn(move || {
                let v = me.version();
                *me.state.lock().unwrap() = ToolState::Ready(v);
                ctx.request_repaint();
            });
        }
        s.clone()
    }

    /// Download the latest yt-dlp in the background (YouTube changes often,
    /// and old versions stop working).
    pub fn update_async(self: &Arc<Self>, ctx: &egui::Context) {
        let (me, ctx) = (self.clone(), ctx.clone());
        *self.state.lock().unwrap() = ToolState::Updating(0.0);
        std::thread::spawn(move || {
            let r = me.download(&|f| {
                *me.state.lock().unwrap() = ToolState::Updating(f);
                ctx.request_repaint();
            });
            *me.state.lock().unwrap() = match r {
                Ok(()) => ToolState::Ready(me.version()),
                Err(e) => ToolState::Failed(e),
            };
            ctx.request_repaint();
        });
    }

    fn local(&self) -> PathBuf {
        self.dir.join(exe_name())
    }

    /// The yt-dlp to run, if there is one.
    pub fn locate(&self) -> Option<PathBuf> {
        let local = self.local();
        if local.is_file() {
            return Some(local);
        }
        let works = |p: &Path| background_command(p).arg("--version").output().is_ok_and(|o| o.status.success());
        if works(Path::new("yt-dlp")) {
            return Some(PathBuf::from("yt-dlp"));
        }
        ["/opt/homebrew/bin/yt-dlp", "/usr/local/bin/yt-dlp", "/usr/bin/yt-dlp"].into_iter().map(PathBuf::from).find(|p| p.is_file() && works(p))
    }

    /// Version string of the yt-dlp that would be used.
    pub fn version(&self) -> Option<String> {
        let exe = self.locate()?;
        let out = background_command(exe).arg("--version").output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// The yt-dlp to run, downloading our own copy first if needed (blocking).
    pub fn ensure(&self, progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
        let local = self.local();
        if local.is_file() {
            return Ok(local);
        }
        match self.download(progress) {
            Ok(()) => Ok(local),
            Err(e) => self.locate().ok_or(e),
        }
    }

    /// Download (or replace) the local copy with the latest release (blocking).
    pub fn download(&self, progress: &dyn Fn(f32)) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap();
        let name = asset().ok_or("No yt-dlp download for this platform. Install yt-dlp and restart.")?;
        let url = format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{name}");
        let dest = self.local();
        let tmp = dest.with_extension("download");
        crate::download::fetch(&url, &tmp, &AtomicBool::new(false), &|done, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                progress(done as f32 / t as f32);
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        #[cfg(target_os = "macos")]
        {
            let _ = background_command("xattr").args(["-d", "com.apple.quarantine"]).arg(&tmp).output();
        }
        std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())
    }

    /// Title, channel and length of a video.
    pub fn info(exe: &Path, url: &str) -> Result<VideoInfo, String> {
        let out = background_command(exe)
            .args(["-J", "--no-playlist", "--no-warnings", "--"])
            .arg(url)
            .output()
            .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
        if !out.status.success() {
            return Err(last_error(&String::from_utf8_lossy(&out.stderr)));
        }
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("Unexpected yt-dlp output: {e}"))?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_owned();
        let artist = [s("artist"), s("creator"), s("uploader"), s("channel")].into_iter().find(|a| !a.is_empty()).unwrap_or_default();
        let title = [s("track"), s("title")].into_iter().find(|t| !t.is_empty()).unwrap_or_default();
        Ok(VideoInfo { title, artist, duration: v.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0) })
    }

    /// Download one format of `url` to `<dir>/<base>.<ext>` and return the
    /// file. `extra` adds format and post-processing options.
    pub fn fetch(exe: &Path, url: &str, dir: &Path, base: &str, extra: &[&str], progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let template = dir.join(format!("{base}.%(ext)s"));
        let mut child = background_command(exe)
            .args(["--no-playlist", "--no-warnings", "--no-part", "--no-mtime", "--force-overwrites", "--newline", "--progress"])
            .args(["--progress-template", "download:SW%(progress._percent_str)s"])
            .args(["--print", "after_move:filepath"])
            .args(extra)
            .arg("-o")
            .arg(&template)
            .arg("--")
            .arg(url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
        let stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut e) = stderr {
                let _ = std::io::Read::read_to_string(&mut e, &mut s);
            }
            s
        });
        let mut file = None;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let line = line.trim();
                if let Some(pct) = line.strip_prefix("SW") {
                    if let Ok(p) = pct.trim().trim_end_matches('%').trim().parse::<f32>() {
                        progress((p / 100.0).clamp(0.0, 1.0));
                    }
                } else if !line.is_empty() && Path::new(line).is_file() {
                    file = Some(PathBuf::from(line));
                }
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        let stderr = err_thread.join().unwrap_or_default();
        if !status.success() {
            return Err(last_error(&stderr));
        }
        file.ok_or_else(|| "yt-dlp didn't say where it saved the file".to_owned())
    }
}

/// The useful part of yt-dlp's stderr.
fn last_error(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| l.contains("ERROR")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("yt-dlp failed");
    let msg = line.trim().trim_start_matches("ERROR:").trim();
    // "[youtube] abc: Video unavailable" → "Video unavailable"
    let msg = if msg.starts_with('[') { msg.split_once(": ").map_or(msg, |(_, m)| m) } else { msg };
    msg.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_shortened() {
        let e = "WARNING: x\nERROR: [youtube] dQw4w9WgXcQ: Video unavailable. This video is private\n";
        assert_eq!(last_error(e), "Video unavailable. This video is private");
        assert_eq!(last_error(""), "yt-dlp failed");
    }

    /// Downloads yt-dlp and a short video: `cargo test ytdlp_download -- --ignored`
    #[test]
    #[ignore]
    fn ytdlp_download_audio() {
        let dir = std::env::temp_dir().join(format!("sw-ytdlp-{}", std::process::id()));
        let y = YtDlp { dir: dir.join("bin"), lock: Mutex::new(()), state: Mutex::new(ToolState::Unknown) };
        let exe = y.ensure(&|_| {}).unwrap();
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        let info = YtDlp::info(&exe, url).unwrap();
        assert!(info.duration > 10.0);
        assert!(exe.starts_with(&dir), "uses its own copy, not one on PATH");
        let f = YtDlp::fetch(&exe, url, &dir, "a", &["-f", "ba[ext=m4a]/ba[ext=mp4]/ba/b[ext=mp4]/b"], &|_| {}).unwrap();
        // The app decodes what was downloaded.
        let (buf, _) = crate::engine::decode_file_for_tests(&f);
        assert!(buf.duration() > 10.0, "decoded {} s", buf.duration());
        assert!(f.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
