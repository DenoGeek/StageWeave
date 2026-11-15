//! Streaming HTTP downloads with progress, written to `<dest>.part` and renamed
//! into place only when complete.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Progress of a download running on a background thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DlState {
    #[default]
    Idle,
    Running { done: u64, total: Option<u64> },
    Done,
    Failed(String),
}

impl DlState {
    pub fn fraction(&self) -> Option<f32> {
        match self {
            Self::Running { done, total: Some(t) } if *t > 0 => Some(*done as f32 / *t as f32),
            _ => None,
        }
    }
}

pub type Shared = Arc<Mutex<DlState>>;

/// Some stock sites refuse requests without a browser-like agent.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Download `url` to `dest`, reporting `(bytes done, total)`.
pub fn fetch(url: &str, dest: &Path, cancel: &AtomicBool, progress: &dyn Fn(u64, Option<u64>)) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    }
    let resp = ureq::get(url).header("User-Agent", USER_AGENT).call().map_err(|e| format!("Download failed: {e}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let mut reader = resp.into_body().into_reader();
    let part = part_path(dest);
    let mut out = std::fs::File::create(&part).map_err(|e| format!("Can't write {}: {e}", part.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let result = (|| {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".to_owned());
            }
            let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| format!("Can't write file: {e}"))?;
            done += n as u64;
            progress(done, total);
        }
        if total.is_some_and(|t| t != done) {
            return Err("Download ended early".to_owned());
        }
        out.flush().map_err(|e| e.to_string())
    })();
    drop(out);
    match result {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Run `fetch` on a thread, publishing progress into the returned state.
pub fn spawn(url: String, dest: PathBuf, ctx: egui::Context, after: impl FnOnce(&Path) -> Result<(), String> + Send + 'static) -> Shared {
    let state: Shared = Arc::new(Mutex::new(DlState::Running { done: 0, total: None }));
    let s = state.clone();
    std::thread::spawn(move || {
        let cancel = AtomicBool::new(false);
        let last = std::cell::Cell::new(std::time::Instant::now());
        let r = fetch(&url, &dest, &cancel, &|done, total| {
            if last.get().elapsed().as_millis() > 100 {
                last.set(std::time::Instant::now());
                *s.lock().unwrap() = DlState::Running { done, total };
                ctx.request_repaint();
            }
        })
        .and_then(|()| after(&dest));
        *s.lock().unwrap() = match r {
            Ok(()) => DlState::Done,
            Err(e) => DlState::Failed(e),
        };
        ctx.request_repaint();
    });
    state
}
