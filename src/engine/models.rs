//! Model downloads: status, a sequential download queue with byte progress,
//! and deletion. State is persisted in SQLite (`Store`).

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::store::{ModelRecord, Store};

use super::python_env::PythonEnv;
use super::worker::{self, WorkerMsg};

#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    Idle,
    Queued,
    Downloading { done: u64, total: Option<u64>, bytes_per_sec: f64, file: String },
    Failed(String),
}

/// Everything the UI needs to show one model.
#[derive(Clone, Debug)]
pub struct ModelView {
    pub record: ModelRecord,
    pub transfer: Transfer,
}

impl ModelView {
    pub fn is_downloaded(&self) -> bool {
        self.record.downloaded
    }
}

struct Inner {
    transfers: HashMap<String, Transfer>,
    cancels: HashMap<String, Arc<AtomicBool>>,
    queue: VecDeque<String>,
    runner: bool,
    refreshing: bool,
}

pub struct ModelManager {
    env: Arc<PythonEnv>,
    pub store: Arc<Store>,
    inner: Mutex<Inner>,
    repaint: Mutex<Option<egui::Context>>,
}

impl ModelManager {
    pub fn new(env: Arc<PythonEnv>, store: Arc<Store>) -> Arc<Self> {
        Arc::new(Self {
            env,
            store,
            inner: Mutex::new(Inner {
                transfers: HashMap::new(),
                cancels: HashMap::new(),
                queue: VecDeque::new(),
                runner: false,
                refreshing: false,
            }),
            repaint: Mutex::new(None),
        })
    }

    fn models_dir_args(&self) -> [OsString; 2] {
        ["--models-dir".into(), self.env.paths.models.join("uvr").into_os_string()]
    }

    fn repaint(&self) {
        if let Some(ctx) = &*self.repaint.lock().unwrap() {
            ctx.request_repaint();
        }
    }

    pub fn view(&self, key: &str) -> ModelView {
        let record = self
            .store
            .models()
            .into_iter()
            .find(|m| m.key == key)
            .unwrap_or_else(|| ModelRecord { key: key.to_owned(), ..Default::default() });
        let transfer = self.inner.lock().unwrap().transfers.get(key).cloned().unwrap_or(Transfer::Idle);
        ModelView { record, transfer }
    }

    pub fn views(&self, keys: &[String]) -> HashMap<String, ModelView> {
        let records: HashMap<String, ModelRecord> = self.store.models().into_iter().map(|m| (m.key.clone(), m)).collect();
        let inner = self.inner.lock().unwrap();
        keys.iter()
            .map(|k| {
                let record = records.get(k).cloned().unwrap_or_else(|| ModelRecord { key: k.clone(), ..Default::default() });
                let transfer = inner.transfers.get(k).cloned().unwrap_or(Transfer::Idle);
                (k.clone(), ModelView { record, transfer })
            })
            .collect()
    }

    pub fn is_refreshing(&self) -> bool {
        self.inner.lock().unwrap().refreshing
    }

    pub fn any_active(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.runner || inner.refreshing
    }

    /// Re-check what's on disk; with `remote`, also look up download sizes
    /// for models whose size isn't known yet (slow, needs network).
    pub fn refresh(self: &Arc<Self>, ctx: &egui::Context, keys: Vec<String>, remote: bool) {
        *self.repaint.lock().unwrap() = Some(ctx.clone());
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.refreshing || keys.is_empty() {
                return;
            }
            inner.refreshing = true;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            this.status(&keys, false);
            if remote {
                let known: HashMap<String, Option<u64>> =
                    this.store.models().into_iter().map(|m| (m.key, m.bytes_total)).collect();
                let missing: Vec<String> =
                    keys.iter().filter(|k| known.get(*k).copied().flatten().is_none()).cloned().collect();
                if !missing.is_empty() {
                    this.status(&missing, true);
                }
            }
            this.inner.lock().unwrap().refreshing = false;
            this.repaint();
        });
    }

    fn status(&self, keys: &[String], remote: bool) {
        let mut args: Vec<OsString> = vec!["status".into()];
        args.extend(self.models_dir_args());
        if remote {
            args.push("--remote".into());
        }
        args.extend(keys.iter().map(OsString::from));
        let _ = worker::run(&self.env, &args, None, |msg| {
            if let WorkerMsg::Model { key, downloaded, on_disk, total } = msg {
                self.store.set_model_status(&key, downloaded, on_disk, total);
                self.repaint();
            }
        });
    }

    /// Queue a download (no-op if already downloaded or in flight).
    pub fn download(self: &Arc<Self>, ctx: &egui::Context, key: &str) {
        *self.repaint.lock().unwrap() = Some(ctx.clone());
        self.enqueue(key, false);
    }

    fn enqueue(self: &Arc<Self>, key: &str, front: bool) {
        let start_runner = {
            let mut inner = self.inner.lock().unwrap();
            let busy = matches!(inner.transfers.get(key), Some(Transfer::Queued | Transfer::Downloading { .. }));
            if !busy {
                inner.transfers.insert(key.to_owned(), Transfer::Queued);
                if front {
                    inner.queue.push_front(key.to_owned());
                } else {
                    inner.queue.push_back(key.to_owned());
                }
            } else if front && let Some(pos) = inner.queue.iter().position(|k| k == key) {
                // Jump the queue: a separation is waiting for this model.
                let k = inner.queue.remove(pos).unwrap();
                inner.queue.push_front(k);
            }
            let start = !inner.runner;
            inner.runner = true;
            start
        };
        if start_runner {
            let this = self.clone();
            std::thread::spawn(move || this.run_queue());
        }
        self.repaint();
    }

    pub fn cancel(&self, key: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.queue.retain(|k| k != key);
        if let Some(c) = inner.cancels.get(key) {
            c.store(true, Ordering::Relaxed);
        }
        if matches!(inner.transfers.get(key), Some(Transfer::Queued)) {
            inner.transfers.insert(key.to_owned(), Transfer::Idle);
        }
    }

    pub fn delete(self: &Arc<Self>, ctx: &egui::Context, key: &str) {
        *self.repaint.lock().unwrap() = Some(ctx.clone());
        self.cancel(key);
        let (this, key) = (self.clone(), key.to_owned());
        std::thread::spawn(move || {
            let mut args: Vec<OsString> = vec!["delete".into()];
            args.extend(this.models_dir_args());
            args.push(key.clone().into());
            if let Err(e) = worker::run(&this.env, &args, None, |_| {}) {
                this.store.set_model_error(&key, Some(&e.message));
            }
            this.status(std::slice::from_ref(&key), false);
            this.inner.lock().unwrap().transfers.insert(key, Transfer::Idle);
            this.repaint();
        });
    }

    fn run_queue(self: Arc<Self>) {
        loop {
            let (key, cancel) = {
                let mut inner = self.inner.lock().unwrap();
                let Some(key) = inner.queue.pop_front() else {
                    inner.runner = false;
                    break;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                inner.cancels.insert(key.clone(), cancel.clone());
                inner.transfers.insert(
                    key.clone(),
                    Transfer::Downloading { done: 0, total: None, bytes_per_sec: 0.0, file: String::new() },
                );
                (key, cancel)
            };
            self.repaint();
            let result = self.download_now(&key, &cancel);
            let outcome = match result {
                Ok(()) => Transfer::Idle,
                Err(e) if e.cancelled => Transfer::Idle,
                Err(e) => {
                    self.store.set_model_error(&key, Some(&e.message));
                    Transfer::Failed(e.message)
                }
            };
            // Re-measure what's on disk (also catches partial downloads).
            self.status(std::slice::from_ref(&key), false);
            let mut inner = self.inner.lock().unwrap();
            inner.cancels.remove(&key);
            inner.transfers.insert(key, outcome);
            drop(inner);
            self.repaint();
        }
        self.repaint();
    }

    fn download_now(&self, key: &str, cancel: &AtomicBool) -> Result<(), worker::WorkerError> {
        let mut args: Vec<OsString> = vec!["download".into()];
        args.extend(self.models_dir_args());
        args.push(key.into());
        self.store.set_model_error(key, None);
        let mut speed = SpeedMeter::default();
        worker::run(&self.env, &args, Some(cancel), |msg| {
            let update = match msg {
                WorkerMsg::Download { done, total, file } => {
                    let bps = speed.sample(done);
                    if let Some(t) = total {
                        self.store.set_model_status(key, false, done, Some(t));
                    }
                    Some(Transfer::Downloading { done, total, bytes_per_sec: bps, file })
                }
                WorkerMsg::Stage(s) => {
                    let mut inner = self.inner.lock().unwrap();
                    if let Some(Transfer::Downloading { file, .. }) = inner.transfers.get_mut(key) {
                        *file = s;
                    }
                    None
                }
                _ => None,
            };
            if let Some(t) = update {
                self.inner.lock().unwrap().transfers.insert(key.to_owned(), t);
                self.repaint();
            }
        })
    }

    /// Block until `key` is downloaded, reporting progress. Used by the
    /// splitter so a separation transparently waits for its model.
    pub fn ensure_downloaded(
        self: &Arc<Self>,
        key: &str,
        progress: &dyn Fn(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if self.view(key).is_downloaded() {
            return Ok(());
        }
        // The cached flag may be stale (files deleted by hand); re-check first.
        self.status(&[key.to_owned()], false);
        if self.view(key).is_downloaded() {
            return Ok(());
        }
        self.enqueue(key, true);
        loop {
            if cancel.load(Ordering::Relaxed) {
                self.cancel(key);
                return Err("Cancelled".into());
            }
            let v = self.view(key);
            match &v.transfer {
                Transfer::Queued => progress(-1.0, "Waiting for another download to finish"),
                Transfer::Downloading { done, total, bytes_per_sec, .. } => {
                    let frac = total.map_or(-1.0, |t| *done as f32 / t.max(1) as f32);
                    progress(frac, &format!("Downloading model · {}", transfer_label(*done, *total, *bytes_per_sec)));
                }
                Transfer::Failed(e) => return Err(format!("Model download failed: {e}")),
                Transfer::Idle => {
                    return if v.is_downloaded() {
                        Ok(())
                    } else {
                        Err("Model download was cancelled".into())
                    };
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

/// Smoothed transfer speed.
#[derive(Default)]
struct SpeedMeter {
    last: Option<(Instant, u64)>,
    bps: f64,
}

impl SpeedMeter {
    fn sample(&mut self, done: u64) -> f64 {
        let now = Instant::now();
        if let Some((t, b)) = self.last {
            let dt = now.duration_since(t).as_secs_f64();
            if dt >= 0.25 {
                let inst = done.saturating_sub(b) as f64 / dt;
                self.bps = if self.bps == 0.0 { inst } else { self.bps * 0.7 + inst * 0.3 };
                self.last = Some((now, done));
            }
        } else {
            self.last = Some((now, done));
        }
        self.bps
    }
}

pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

/// "120 / 640 MB · 8.4 MB/s · 1:02 left"
pub fn transfer_label(done: u64, total: Option<u64>, bps: f64) -> String {
    let mut s = match total {
        Some(t) => format!("{} / {}", human_bytes(done), human_bytes(t)),
        None => human_bytes(done),
    };
    if bps > 1.0 {
        s.push_str(&format!(" · {}/s", human_bytes(bps as u64)));
        if let Some(t) = total {
            let secs = (t.saturating_sub(done) as f64 / bps) as u64;
            s.push_str(&format!(" · {}:{:02} left", secs / 60, secs % 60));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_labels() {
        assert_eq!(human_bytes(639_333_486), "639 MB");
        assert_eq!(transfer_label(100_000_000, Some(200_000_000), 0.0), "100 MB / 200 MB");
        assert_eq!(
            transfer_label(100_000_000, Some(200_000_000), 10_000_000.0),
            "100 MB / 200 MB · 10 MB/s · 0:10 left"
        );
    }
}
