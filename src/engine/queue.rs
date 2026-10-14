//! Background separation queue: songs are split one at a time (one model on
//! the GPU at a time) while the UI keeps running. Also the stem loader that
//! decodes a finished song's stems for playback.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::model::StemKind;

use super::decoder::decode_file;
use super::{PEAKS_PER_SEC, SplitModel, StemOutput, StemSplitter};

#[derive(Clone)]
pub struct SplitRequest {
    pub song_id: i64,
    pub title: String,
    pub source: PathBuf,
    pub model: SplitModel,
    pub out_dir: PathBuf,
}

/// What the UI shows for a song in the queue.
#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Queued,
    /// Negative progress = indeterminate.
    Running { progress: f32, stage: String, started: Instant },
}

enum Event {
    Started(i64),
    Progress(i64, f32, String),
    Done(i64, Result<Vec<StemOutput>, String>, f32),
}

struct Shared {
    pending: VecDeque<SplitRequest>,
    active: Option<(i64, Arc<AtomicBool>)>,
    running: bool,
}

pub struct Finished {
    pub song_id: i64,
    pub result: Result<Vec<StemOutput>, String>,
    pub seconds: f32,
}

pub struct SplitQueue {
    splitter: Arc<dyn StemSplitter>,
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    pub jobs: HashMap<i64, JobState>,
    /// Titles for the status bar.
    titles: HashMap<i64, String>,
    repaint: egui::Context,
}

impl SplitQueue {
    pub fn new(splitter: Arc<dyn StemSplitter>, repaint: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            splitter,
            shared: Arc::new(Mutex::new(Shared { pending: VecDeque::new(), active: None, running: false })),
            tx,
            rx,
            jobs: HashMap::new(),
            titles: HashMap::new(),
            repaint,
        }
    }

    /// Queue a song (ignored if it's already queued or running).
    pub fn enqueue(&mut self, req: SplitRequest) {
        if self.jobs.contains_key(&req.song_id) {
            return;
        }
        self.jobs.insert(req.song_id, JobState::Queued);
        self.titles.insert(req.song_id, req.title.clone());
        let start = {
            let mut s = self.shared.lock().unwrap();
            s.pending.push_back(req);
            !std::mem::replace(&mut s.running, true)
        };
        if start {
            self.spawn_worker();
        }
        self.repaint.request_repaint();
    }

    fn spawn_worker(&self) {
        let (shared, tx, splitter, repaint) =
            (self.shared.clone(), self.tx.clone(), self.splitter.clone(), self.repaint.clone());
        std::thread::spawn(move || {
            loop {
                let (req, cancel) = {
                    let mut s = shared.lock().unwrap();
                    let Some(req) = s.pending.pop_front() else {
                        s.running = false;
                        s.active = None;
                        break;
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    s.active = Some((req.song_id, cancel.clone()));
                    (req, cancel)
                };
                let id = req.song_id;
                let _ = tx.send(Event::Started(id));
                repaint.request_repaint();
                let started = Instant::now();
                let progress = |f: f32, stage: &str| {
                    let _ = tx.send(Event::Progress(id, f, stage.to_owned()));
                    repaint.request_repaint();
                };
                let result = splitter.split(&req.source, &req.model, &req.out_dir, &progress, &cancel);
                let _ = tx.send(Event::Done(id, result, started.elapsed().as_secs_f32()));
                repaint.request_repaint();
            }
        });
    }

    pub fn cancel(&mut self, song_id: i64) {
        let mut s = self.shared.lock().unwrap();
        let before = s.pending.len();
        s.pending.retain(|r| r.song_id != song_id);
        if s.pending.len() != before {
            self.jobs.remove(&song_id);
        }
        if let Some((id, c)) = &s.active
            && *id == song_id
        {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_all(&mut self) {
        let mut s = self.shared.lock().unwrap();
        for r in s.pending.drain(..) {
            self.jobs.remove(&r.song_id);
        }
        if let Some((_, c)) = &s.active {
            c.store(true, Ordering::Relaxed);
        }
    }

    pub fn state(&self, song_id: i64) -> Option<&JobState> {
        self.jobs.get(&song_id)
    }

    pub fn is_idle(&self) -> bool {
        self.jobs.is_empty()
    }

    /// e.g. "Splitting 2 of 6 · Amazing Grace 45%"
    pub fn summary(&self) -> Option<String> {
        let (id, progress) = self.jobs.iter().find_map(|(id, j)| match j {
            JobState::Running { progress, .. } => Some((*id, *progress)),
            _ => None,
        })?;
        let title = self.titles.get(&id).cloned().unwrap_or_default();
        let pct = if progress >= 0.0 { format!(" {:.0}%", progress * 100.0) } else { String::new() };
        let n = self.jobs.len();
        Some(if n > 1 { format!("Splitting · {title}{pct} · {} more queued", n - 1) } else { format!("Splitting · {title}{pct}") })
    }

    pub fn poll(&mut self) -> Vec<Finished> {
        let mut done = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Started(id) => {
                    self.jobs.insert(id, JobState::Running { progress: -1.0, stage: "Starting…".into(), started: Instant::now() });
                }
                Event::Progress(id, p, stage) => {
                    if let Some(JobState::Running { progress, stage: s, .. }) = self.jobs.get_mut(&id) {
                        *progress = p;
                        *s = stage;
                    }
                }
                Event::Done(id, result, seconds) => {
                    self.jobs.remove(&id);
                    done.push(Finished { song_id: id, result, seconds });
                }
            }
        }
        done
    }
}

/// Decodes stems for playback off the UI thread.
pub struct StemLoadJob {
    pub song_id: i64,
    pub rx: Receiver<Result<Vec<StemOutput>, String>>,
}

impl StemLoadJob {
    pub fn spawn(song_id: i64, stems: Vec<(StemKind, PathBuf)>, repaint: egui::Context) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = stems
                .into_iter()
                .map(|(kind, path)| {
                    let (audio, _) = decode_file(&path)?;
                    Ok(StemOutput { kind, peaks: audio.peaks(PEAKS_PER_SEC), path: Some(path), audio: Some(Arc::new(audio)) })
                })
                .collect();
            let _ = tx.send(result);
            repaint.request_repaint();
        });
        Self { song_id, rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mock::MockSplitter;

    fn req(id: i64) -> SplitRequest {
        let model = MockSplitter.models()[0].clone();
        SplitRequest {
            song_id: id,
            title: format!("song {id}"),
            source: PathBuf::from(format!("/tmp/song{id}.wav")),
            model,
            out_dir: std::env::temp_dir(),
        }
    }

    fn drain(q: &mut SplitQueue, want: usize) -> Vec<Finished> {
        let mut out = Vec::new();
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        while out.len() < want && Instant::now() < deadline {
            out.extend(q.poll());
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        out
    }

    #[test]
    fn runs_in_order_and_dedupes() {
        let mut q = SplitQueue::new(Arc::new(MockSplitter), egui::Context::default());
        q.enqueue(req(1));
        q.enqueue(req(2));
        q.enqueue(req(2)); // duplicate ignored
        let done = drain(&mut q, 2);
        assert_eq!(done.iter().map(|f| f.song_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(done.iter().all(|f| f.result.is_ok()));
        assert!(q.is_idle());
    }

    #[test]
    fn cancel_removes_pending_and_stops_active() {
        let mut q = SplitQueue::new(Arc::new(MockSplitter), egui::Context::default());
        q.enqueue(req(1));
        q.enqueue(req(2));
        q.enqueue(req(3));
        q.cancel(2);
        std::thread::sleep(std::time::Duration::from_millis(100));
        q.cancel(1); // active
        let done = drain(&mut q, 2);
        let ids: Vec<i64> = done.iter().map(|f| f.song_id).collect();
        assert_eq!(ids, vec![1, 3]);
        assert!(done[0].result.is_err(), "cancelled job reports an error");
        assert!(done[1].result.is_ok());
    }
}
