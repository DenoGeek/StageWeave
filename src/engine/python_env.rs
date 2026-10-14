//! Manages the private Python environment that runs the separation worker.
//!
//! Layout (under the OS app-data dir, e.g. `~/Library/Application Support/MusicKit`):
//! ```text
//! engine/uv/            uv binary, downloaded if not on PATH
//! engine/python/        standalone CPython installed by uv
//! engine/venv/          torch + demucs + audio-separator
//! engine/models/        model weights (HF / torch hub / UVR)
//! engine/musickit_worker.py
//! engine/engine.json    installed versions
//! ```
//! Separated stems are cached under the OS cache dir in `stems/`.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub const WORKER_SOURCE: &str = include_str!("../../assets/musickit_worker.py");

/// Versions installed into the private environment.
pub const PYTHON_VERSION: &str = "3.11";
pub const DEMUCS_REQ: &str = "demucs==4.1.0";
/// Package name and version are separate so the `[cpu]` / `[gpu]` extra can go in between.
pub const AUDIO_SEPARATOR: (&str, &str) = ("audio-separator", "0.47.0");

/// e.g. `audio-separator[cpu]==0.47.0`
pub fn audio_separator_req(extra: &str) -> String {
    format!("{}[{extra}]=={}", AUDIO_SEPARATOR.0, AUDIO_SEPARATOR.1)
}

#[derive(Clone, Debug)]
pub struct EnginePaths {
    pub root: PathBuf,
    pub uv_dir: PathBuf,
    pub python_dir: PathBuf,
    pub venv: PathBuf,
    pub models: PathBuf,
    pub worker: PathBuf,
    pub manifest: PathBuf,
    pub stem_cache: PathBuf,
}

impl EnginePaths {
    pub fn new() -> Self {
        let dirs = directories::ProjectDirs::from("com", "CodeFamiliar", "MusicKit");
        let (data, cache) = match &dirs {
            Some(d) => (d.data_dir().to_path_buf(), d.cache_dir().to_path_buf()),
            None => (std::env::temp_dir().join("musickit"), std::env::temp_dir().join("musickit-cache")),
        };
        let root = data.join("engine");
        Self {
            uv_dir: root.join("uv"),
            python_dir: root.join("python"),
            venv: root.join("venv"),
            models: root.join("models"),
            worker: root.join("musickit_worker.py"),
            manifest: root.join("engine.json"),
            stem_cache: cache.join("stems"),
            root,
        }
    }

    pub fn database(&self) -> PathBuf {
        self.root.parent().unwrap_or(&self.root).join("musickit.db")
    }

    pub fn python(&self) -> PathBuf {
        if cfg!(windows) {
            self.venv.join("Scripts").join("python.exe")
        } else {
            self.venv.join("bin").join("python")
        }
    }

    fn local_uv(&self) -> PathBuf {
        self.uv_dir.join(if cfg!(windows) { "uv.exe" } else { "uv" })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EnvInfo {
    pub python: Option<String>,
    pub torch: Option<String>,
    pub demucs: Option<String>,
    pub audio_separator: Option<String>,
    pub device: Option<String>,
}

impl EnvInfo {
    pub fn summary(&self) -> String {
        let dev = self.device.as_deref().unwrap_or("cpu").to_uppercase();
        format!(
            "Demucs {} · UVR {} · {dev}",
            self.demucs.as_deref().unwrap_or("?"),
            self.audio_separator.as_deref().unwrap_or("?")
        )
    }
}

#[derive(Clone, Debug)]
pub enum EnvStatus {
    Checking,
    NotInstalled,
    Installing { step: usize, of: usize, text: String },
    Ready(EnvInfo),
    Broken(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevicePref {
    Auto,
    Cpu,
    Gpu,
}

impl DevicePref {
    pub fn arg(self, info: Option<&EnvInfo>) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Cpu => "cpu".into(),
            Self::Gpu => info
                .and_then(|i| i.device.clone())
                .filter(|d| d != "cpu")
                .unwrap_or_else(|| "auto".into()),
        }
    }
}

struct State {
    status: EnvStatus,
    log: Vec<String>,
    device: DevicePref,
}

/// Thread-safe handle shared by the UI and the splitter.
pub struct PythonEnv {
    pub paths: EnginePaths,
    state: Mutex<State>,
}

const LOG_LINES: usize = 400;

impl PythonEnv {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            paths: EnginePaths::new(),
            state: Mutex::new(State { status: EnvStatus::Checking, log: Vec::new(), device: DevicePref::Auto }),
        })
    }

    pub fn status(&self) -> EnvStatus {
        self.state.lock().unwrap().status.clone()
    }

    pub fn info(&self) -> Option<EnvInfo> {
        match self.status() {
            EnvStatus::Ready(i) => Some(i),
            _ => None,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.status(), EnvStatus::Checking | EnvStatus::Installing { .. })
    }

    pub fn device(&self) -> DevicePref {
        self.state.lock().unwrap().device
    }

    pub fn set_device(&self, d: DevicePref) {
        self.state.lock().unwrap().device = d;
    }

    pub fn log_tail(&self, n: usize) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.log[s.log.len().saturating_sub(n)..].to_vec()
    }

    pub fn log(&self, line: impl Into<String>) {
        let mut s = self.state.lock().unwrap();
        s.log.push(line.into());
        let excess = s.log.len().saturating_sub(LOG_LINES);
        s.log.drain(..excess);
    }

    /// The most useful line of recent output: uv/pip print `error: …` first.
    fn last_error_line(&self) -> String {
        let tail = self.log_tail(40);
        tail.iter()
            .find(|l| l.trim_start().to_ascii_lowercase().starts_with("error"))
            .or_else(|| tail.iter().rev().find(|l| l.chars().any(char::is_alphanumeric)))
            .map(|l| l.trim().trim_start_matches("error:").trim_start_matches("Error:").trim().to_owned())
            .unwrap_or_else(|| "see the log for details".into())
    }

    /// Persist the log so failed installs can be diagnosed after a restart.
    fn save_log(&self) {
        let _ = std::fs::create_dir_all(&self.paths.root);
        let _ = std::fs::write(self.paths.root.join("install.log"), self.log_tail(LOG_LINES).join("\n"));
    }

    fn set_status(&self, status: EnvStatus) {
        self.state.lock().unwrap().status = status;
    }

    /// Write the embedded worker script if it is missing or outdated.
    pub fn ensure_worker(&self) -> Result<PathBuf, String> {
        let p = &self.paths.worker;
        if std::fs::read_to_string(p).ok().as_deref() != Some(WORKER_SOURCE) {
            std::fs::create_dir_all(&self.paths.root).map_err(|e| e.to_string())?;
            std::fs::write(p, WORKER_SOURCE).map_err(|e| format!("Can't write worker: {e}"))?;
        }
        Ok(p.clone())
    }

    /// Command for running the worker with model caches redirected into app data.
    pub fn worker_command(&self) -> Result<Command, String> {
        let worker = self.ensure_worker()?;
        let mut cmd = background_command(self.paths.python());
        cmd.arg("-u")
            .arg(worker)
            .env("HF_HOME", self.paths.models.join("hf"))
            .env("TORCH_HOME", self.paths.models.join("torch"))
            .env("HF_HUB_DISABLE_XET", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1");
        Ok(cmd)
    }

    /// Probe the environment in the background.
    pub fn check(self: &Arc<Self>, ctx: egui::Context) {
        let this = self.clone();
        self.set_status(EnvStatus::Checking);
        std::thread::spawn(move || {
            let status = this.probe();
            this.set_status(status);
            ctx.request_repaint();
        });
    }

    fn probe(&self) -> EnvStatus {
        if !self.paths.python().exists() {
            return EnvStatus::NotInstalled;
        }
        let mut cmd = match self.worker_command() {
            Ok(c) => c,
            Err(e) => return EnvStatus::Broken(e),
        };
        let out = match cmd.arg("env-info").stdin(Stdio::null()).output() {
            Ok(o) => o,
            Err(e) => return EnvStatus::Broken(format!("Can't start Python: {e}")),
        };
        for line in String::from_utf8_lossy(&out.stderr).lines() {
            self.log(line);
        }
        let env = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["t"] == "env");
        match env {
            Some(v) => {
                let info: EnvInfo = serde_json::from_value(v).unwrap_or_default();
                if info.demucs.is_none() || info.audio_separator.is_none() || info.torch.is_none() {
                    EnvStatus::Broken("Some packages are missing. Reinstall the engine.".into())
                } else {
                    let _ = std::fs::write(&self.paths.manifest, serde_json::to_string_pretty(&info).unwrap_or_default());
                    EnvStatus::Ready(info)
                }
            }
            None => EnvStatus::Broken("The engine didn't respond. Reinstall it.".into()),
        }
    }

    /// Build (or rebuild) the environment in the background.
    pub fn install(self: &Arc<Self>, ctx: egui::Context) {
        if self.is_busy() {
            return;
        }
        let this = self.clone();
        self.set_status(EnvStatus::Installing { step: 1, of: 4, text: "Preparing".into() });
        std::thread::spawn(move || {
            let result = this.run_install(&ctx);
            match result {
                Ok(()) => {
                    this.set_status(EnvStatus::Installing { step: 4, of: 4, text: "Verifying".into() });
                    ctx.request_repaint();
                    let status = this.probe();
                    this.set_status(status);
                }
                Err(e) => {
                    this.log(format!("ERROR: {e}"));
                    this.set_status(EnvStatus::Broken(e));
                }
            }
            this.save_log();
            ctx.request_repaint();
        });
    }

    fn step(&self, ctx: &egui::Context, step: usize, text: &str) {
        self.log(format!("== {text}"));
        self.set_status(EnvStatus::Installing { step, of: 4, text: text.into() });
        ctx.request_repaint();
    }

    fn run_install(&self, ctx: &egui::Context) -> Result<(), String> {
        let p = &self.paths;
        std::fs::create_dir_all(&p.root).map_err(|e| e.to_string())?;

        self.step(ctx, 1, "Getting uv");
        let uv = self.find_or_download_uv()?;

        self.step(ctx, 2, &format!("Installing Python {PYTHON_VERSION}"));
        if p.venv.exists() {
            std::fs::remove_dir_all(&p.venv).map_err(|e| format!("Can't remove old environment: {e}"))?;
        }
        let mut cmd = background_command(&uv);
        cmd.args(["venv", "--python", PYTHON_VERSION]).arg(&p.venv);
        cmd.env("UV_PYTHON_INSTALL_DIR", &p.python_dir);
        self.run_logged(cmd, ctx)?;

        let extra = if has_nvidia_gpu() { "gpu" } else { "cpu" };
        self.step(ctx, 3, &format!("Downloading PyTorch, Demucs and UVR ({extra}) — this can take a while"));
        let mut cmd = background_command(&uv);
        cmd.args(["pip", "install", "--python"])
            .arg(p.python())
            .args(["--torch-backend", "auto", DEMUCS_REQ])
            .arg(audio_separator_req(extra));
        cmd.env("UV_PYTHON_INSTALL_DIR", &p.python_dir);
        self.run_logged(cmd, ctx)?;

        self.ensure_worker()?;
        Ok(())
    }

    /// Run a command, streaming its stdout+stderr into the log.
    fn run_logged(&self, mut cmd: Command, ctx: &egui::Context) -> Result<(), String> {
        self.log(format!("$ {cmd:?}"));
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Couldn't run {:?}: {e}", cmd.get_program()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        std::thread::scope(|s| {
            for stream in [stdout.map(|o| Box::new(o) as Box<dyn Read + Send>), stderr.map(|e| Box::new(e) as _)]
                .into_iter()
                .flatten()
            {
                s.spawn(move || {
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        self.log(line);
                        ctx.request_repaint();
                    }
                });
            }
        });
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            let tool = Path::new(cmd.get_program()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            Err(format!("{tool} failed ({status}): {}", self.last_error_line()))
        }
    }

    fn find_or_download_uv(&self) -> Result<PathBuf, String> {
        let local = self.paths.local_uv();
        if local.exists() {
            return Ok(local);
        }
        if let Ok(o) = background_command("uv").arg("--version").output()
            && o.status.success()
        {
            self.log(format!("Using uv from PATH ({})", String::from_utf8_lossy(&o.stdout).trim()));
            return Ok(PathBuf::from("uv"));
        }
        download_uv(&self.paths.uv_dir, &local, |l| self.log(l))?;
        Ok(local)
    }

    /// Delete the environment (models and stem cache are kept).
    pub fn uninstall(&self) -> Result<(), String> {
        for d in [&self.paths.venv, &self.paths.python_dir] {
            if d.exists() {
                std::fs::remove_dir_all(d).map_err(|e| e.to_string())?;
            }
        }
        self.set_status(EnvStatus::NotInstalled);
        Ok(())
    }
}

/// A command that never flashes a console window on Windows.
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn has_nvidia_gpu() -> bool {
    !cfg!(target_os = "macos")
        && background_command("nvidia-smi")
            .arg("-L")
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

fn uv_target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => return None,
    })
}

fn download_uv(dir: &Path, dest: &Path, log: impl Fn(String)) -> Result<(), String> {
    let target = uv_target().ok_or("No uv build for this platform; install uv manually")?;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let url = format!("https://github.com/astral-sh/uv/releases/latest/download/uv-{target}.{ext}");
    log(format!("Downloading {url}"));
    let mut body = Vec::new();
    ureq::get(&url)
        .call()
        .map_err(|e| format!("Download failed: {e}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("Download failed: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let exe_name = if cfg!(windows) { "uv.exe" } else { "uv" };

    let bytes = if ext == "zip" {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(body)).map_err(|e| e.to_string())?;
        let mut found = None;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = f.name().map(|n| n.into_owned()).unwrap_or_default();
            if Path::new(&name).file_name().is_some_and(|n| n == exe_name) {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                found = Some(buf);
                break;
            }
        }
        found
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(body)));
        let mut found = None;
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut e = entry.map_err(|e| e.to_string())?;
            let is_uv = e.path().ok().and_then(|p| p.file_name().map(|n| n == exe_name)).unwrap_or(false);
            if is_uv {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                found = Some(buf);
                break;
            }
        }
        found
    };
    let bytes = bytes.ok_or("uv binary not found in archive")?;
    std::fs::write(dest, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    log(format!("uv installed to {}", dest.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_goes_before_version() {
        assert_eq!(audio_separator_req("cpu"), "audio-separator[cpu]==0.47.0");
    }
}
