//! ffmpeg: plays video backgrounds and encodes lyric-video exports.
//!
//! Uses `ffmpeg` from PATH when present, otherwise a static build downloaded
//! once into `<app data>/engine/ffmpeg/`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::download::{self, DlState};

use super::python_env::{EnginePaths, background_command};

#[derive(Clone, Debug, PartialEq)]
pub enum FfmpegStatus {
    Unknown,
    Missing,
    Downloading(DlState),
    Ready(PathBuf),
    Failed(String),
}

pub struct Ffmpeg {
    status: Mutex<FfmpegStatus>,
    dir: PathBuf,
}

enum Archive {
    Zip,
    TarXz,
}

/// Where to get a static build for this platform: (url, archive, binary name inside).
fn build_source() -> Option<(&'static str, Archive)> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip", Archive::Zip),
        ("linux", "x86_64") => ("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz", Archive::TarXz),
        ("linux", "aarch64") => ("https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz", Archive::TarXz),
        ("macos", "aarch64") => ("https://ffmpeg.martin-riedl.de/redirect/latest/macos/arm64/release/ffmpeg.zip", Archive::Zip),
        ("macos", "x86_64") => ("https://ffmpeg.martin-riedl.de/redirect/latest/macos/amd64/release/ffmpeg.zip", Archive::Zip),
        _ => return None,
    })
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }
}

impl Ffmpeg {
    pub fn new(paths: &EnginePaths) -> Arc<Self> {
        Arc::new(Self { status: Mutex::new(FfmpegStatus::Unknown), dir: paths.root.join("ffmpeg") })
    }

    pub fn status(&self) -> FfmpegStatus {
        let mut s = self.status.lock().unwrap();
        if *s == FfmpegStatus::Unknown {
            *s = match self.locate() {
                Some(p) => FfmpegStatus::Ready(p),
                None => FfmpegStatus::Missing,
            };
        }
        s.clone()
    }

    /// The ffmpeg binary, if available now.
    pub fn path(&self) -> Option<PathBuf> {
        match self.status() {
            FfmpegStatus::Ready(p) => Some(p),
            _ => None,
        }
    }

    fn local(&self) -> PathBuf {
        self.dir.join(exe_name())
    }

    fn locate(&self) -> Option<PathBuf> {
        let local = self.local();
        if local.is_file() {
            return Some(local);
        }
        let works = |p: &Path| background_command(p).arg("-version").output().is_ok_and(|o| o.status.success());
        if works(Path::new("ffmpeg")) {
            return Some(PathBuf::from("ffmpeg"));
        }
        // GUI apps on macOS don't see the shell's PATH; check the usual places.
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().map(PathBuf::from).find(|p| p.is_file() && works(p))
    }

    /// Download a static build (once). Progress shows in `status()`.
    pub fn install(self: &Arc<Self>, ctx: &egui::Context) {
        if matches!(self.status(), FfmpegStatus::Ready(_) | FfmpegStatus::Downloading(_)) {
            return;
        }
        let Some((url, archive)) = build_source() else {
            *self.status.lock().unwrap() = FfmpegStatus::Failed("No ffmpeg download for this platform. Install ffmpeg and restart.".into());
            return;
        };
        let file = if matches!(archive, Archive::Zip) { "ffmpeg-download.zip" } else { "ffmpeg-download.tar.xz" };
        let dest = self.dir.join(file);
        let me = self.clone();
        *self.status.lock().unwrap() = FfmpegStatus::Downloading(DlState::Running { done: 0, total: None });
        let exe = self.local();
        let state = download::spawn(url.to_owned(), dest, ctx.clone(), move |archive_path| {
            let r = extract(archive_path, &archive, &exe);
            let _ = std::fs::remove_file(archive_path);
            r
        });
        // Mirror the download's progress into our status.
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            loop {
                let s = state.lock().unwrap().clone();
                let next = match &s {
                    DlState::Done => match me.local().is_file() {
                        true => FfmpegStatus::Ready(me.local()),
                        false => FfmpegStatus::Failed("ffmpeg was not found in the download".into()),
                    },
                    DlState::Failed(e) => FfmpegStatus::Failed(e.clone()),
                    other => FfmpegStatus::Downloading(other.clone()),
                };
                let finished = !matches!(next, FfmpegStatus::Downloading(_));
                *me.status.lock().unwrap() = next;
                ctx.request_repaint();
                if finished {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        });
    }
}

/// Pull the ffmpeg binary out of a downloaded archive.
fn extract(archive_path: &Path, kind: &Archive, dest: &Path) -> Result<(), String> {
    let name = exe_name();
    let is_exe = |p: &str| Path::new(p).file_name().is_some_and(|n| n == name);
    let mut bytes = None;
    match kind {
        Archive::Zip => {
            let f = std::fs::File::open(archive_path).map_err(|e| e.to_string())?;
            let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
            for i in 0..zip.len() {
                let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
                if f.is_file() && is_exe(&f.name().map(|n| n.into_owned()).unwrap_or_default()) {
                    let mut buf = Vec::new();
                    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                    bytes = Some(buf);
                    break;
                }
            }
        }
        Archive::TarXz => {
            let mut f = std::io::BufReader::new(std::fs::File::open(archive_path).map_err(|e| e.to_string())?);
            // Unpack to a temp .tar on disk (the archive is a few hundred MB unpacked).
            let tar_path = archive_path.with_extension("unpacked");
            {
                let mut out = std::io::BufWriter::new(std::fs::File::create(&tar_path).map_err(|e| e.to_string())?);
                lzma_rs::xz_decompress(&mut f, &mut out).map_err(|e| format!("Bad archive: {e}"))?;
            }
            let _cleanup = RemoveOnDrop(tar_path.clone());
            let mut tar = tar::Archive::new(std::fs::File::open(&tar_path).map_err(|e| e.to_string())?);
            for entry in tar.entries().map_err(|e| e.to_string())? {
                let mut e = entry.map_err(|e| e.to_string())?;
                let path = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                if e.header().entry_type().is_file() && path.contains("/bin/") && is_exe(&path) {
                    let mut buf = Vec::new();
                    e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                    bytes = Some(buf);
                    break;
                }
            }
        }
    }
    let bytes = bytes.ok_or("ffmpeg not found in the download")?;
    let tmp = download::part_path(dest);
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "macos")]
    {
        // Downloaded by us, not a browser, but clear quarantine just in case.
        let _ = background_command("xattr").args(["-d", "com.apple.quarantine"]).arg(&tmp).output();
    }
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Downloads the real static build: `cargo test ffmpeg_download -- --ignored`
    #[test]
    #[ignore]
    fn ffmpeg_download_and_extract() {
        let (url, archive) = build_source().expect("platform supported");
        let dir = std::env::temp_dir().join(format!("mk-ffmpeg-{}", std::process::id()));
        let file = dir.join("dl");
        crate::download::fetch(url, &file, &std::sync::atomic::AtomicBool::new(false), &|_, _| {}).unwrap();
        let exe = dir.join(exe_name());
        extract(&file, &archive, &exe).unwrap();
        let out = background_command(&exe).arg("-encoders").output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("libx264") && text.contains(" aac "), "encoders available");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
