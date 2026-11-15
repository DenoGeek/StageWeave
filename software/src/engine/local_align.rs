//! Local lyrics alignment: the managed CTC models, and the job that times the
//! user's lyrics against the vocals stem on this computer.
//!
//! The Python worker runs the acoustic model (`emissions`); the alignment
//! itself is `ctc_align`. Nothing is uploaded and no network is used once
//! the model is downloaded (through `ModelManager`, like separation models).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;

use crate::ai::{JobEvent, JobResult, TranscribeJob};
use crate::ctc_align::{self, AlignmentResult, Emissions, LineStatus, Vocab};
use crate::store::Store;

use super::models::ModelManager;
use super::python_env::{EnvStatus, PythonEnv};
use super::worker::{self, EmissionsInfo, WorkerMsg};

/// A model for the lyrics-alignment task (never offered for separation).
#[derive(Clone, Debug, PartialEq)]
pub struct AlignModel {
    /// `align:id`, as used by the worker, the model manager and the database.
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub language: &'static str,
    /// Download size in bytes (pinned files, verified on download).
    pub bytes: u64,
    pub recommended: bool,
}

pub const ALIGN_MODELS: &[AlignModel] = &[AlignModel {
    key: "align:wav2vec2-base-960h-int8",
    name: "Lyrics Alignment — English (CPU)",
    description: "wav2vec2-base-960h (Meta AI, Apache-2.0). Word timing from your own lyrics, offline. Trained on speech: check sung lines.",
    language: "en",
    bytes: 121_954_282 + 291,
    recommended: true,
}];

/// Settings key for the chosen alignment model.
const SETTING: &str = "align_model";

/// The alignment model to use: the saved choice if it's still offered,
/// else the recommended one. Separate from the separation model setting.
pub fn model(store: &Store) -> &'static AlignModel {
    let saved = store.setting(SETTING);
    ALIGN_MODELS
        .iter()
        .find(|m| saved.as_deref() == Some(m.key))
        .or_else(|| ALIGN_MODELS.iter().find(|m| m.recommended))
        .unwrap_or(&ALIGN_MODELS[0])
}

/// Everything a job needs, captured on the UI thread.
pub struct AlignRequest {
    pub vocals: PathBuf,
    /// The lyrics as non-empty lines, as they'll appear in the editor.
    pub lines: Vec<String>,
    pub duration: f64,
    pub model: &'static AlignModel,
    /// Download the model first if needed (the user agreed to it).
    pub allow_download: bool,
}

pub fn spawn(song_id: i64, req: AlignRequest, env: Arc<PythonEnv>, mm: Arc<ModelManager>, repaint: egui::Context) -> TranscribeJob {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        let send = |e: JobEvent| {
            let _ = tx.send(e);
            repaint.request_repaint();
        };
        let result = run(&req, &env, &mm, &c, &send);
        if c.load(Ordering::Relaxed) {
            return send(JobEvent::Done(Err("Cancelled".into())));
        }
        send(JobEvent::Done(result.map(|r| JobResult::Alignment(to_alignment(&r)))));
    });
    TranscribeJob { song_id, rx, cancel, stage: "Starting…".into(), progress: None, started: std::time::Instant::now() }
}

fn run(
    req: &AlignRequest,
    env: &Arc<PythonEnv>,
    mm: &Arc<ModelManager>,
    cancel: &AtomicBool,
    send: &dyn Fn(JobEvent),
) -> Result<AlignmentResult, String> {
    if !matches!(env.status(), EnvStatus::Ready(_)) {
        return Err("The audio engine isn't installed yet. Install it from the Song panel.".into());
    }
    if !mm.view(req.model.key).is_downloaded() && !req.allow_download {
        return Err("The alignment model isn't downloaded. Download it in Window → Models.".into());
    }
    mm.ensure_downloaded(
        req.model.key,
        &|frac, stage| {
            send(JobEvent::Stage(stage.to_owned()));
            send(JobEvent::Progress(frac));
        },
        cancel,
    )?;

    let out = std::env::temp_dir().join(format!("stageweave-emissions-{}-{}.f32", std::process::id(), unique()));
    let _cleanup = RemoveOnDrop(out.clone());
    let args: Vec<OsString> = vec![
        "emissions".into(),
        "--model".into(),
        req.model.key.into(),
        "--input".into(),
        req.vocals.clone().into_os_string(),
        "--out".into(),
        out.clone().into_os_string(),
    ];
    let mut info = None;
    worker::run(env, &args, Some(cancel), |msg| match msg {
        WorkerMsg::Stage(s) => send(JobEvent::Stage(format!("{s}…"))),
        WorkerMsg::Progress(p) => send(JobEvent::Progress(p)),
        WorkerMsg::Emissions(e) => info = Some(e),
        _ => {}
    })
    .map_err(|e| e.message)?;
    let info = info.ok_or("The alignment engine stopped without a result")?;
    env.log(format!(
        "align: model load {:.2} s · inference {:.1} s · peak memory {}",
        info.load_s,
        info.infer_s,
        info.peak_rss_mb.map_or("n/a".into(), |m| format!("{m:.0} MB"))
    ));
    send(JobEvent::Stage("Matching your lyrics to the singing…".into()));
    let em = read_emissions(&info)?;
    let vocab = Vocab::from_map(&info.vocab)?;
    let id = req.model.key.trim_start_matches("align:");
    ctc_align::align(&req.lines, &em, &vocab, req.duration, id)
}

fn unique() -> u64 {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Load the worker's raw little-endian f32 `[frames × classes]` file.
pub fn read_emissions(info: &EmissionsInfo) -> Result<Emissions, String> {
    read_emissions_file(&info.path, info.frames, info.classes, info.stride)
}

fn read_emissions_file(path: &Path, frames: usize, classes: usize, stride: f64) -> Result<Emissions, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read the alignment output: {e}"))?;
    if bytes.len() != frames * classes * 4 {
        return Err("The alignment output is incomplete".into());
    }
    let logp = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    Ok(Emissions { frames, classes, stride, logp })
}

/// The review screen's view of a result.
pub fn to_alignment(r: &AlignmentResult) -> crate::align::Alignment {
    use crate::align::{AlignSource, AlignedLine, Alignment};
    let lines: Vec<AlignedLine> = r
        .lyric_lines()
        .into_iter()
        .zip(&r.lines)
        .map(|(line, l)| AlignedLine { line, confidence: l.confidence, found: l.status == LineStatus::Aligned })
        .collect();
    let matched = r.lines.iter().filter(|l| l.status == LineStatus::Aligned && l.confidence >= ctc_align::CHECK_BELOW).count();
    let not_found = r.lines.iter().filter(|l| l.status != LineStatus::Aligned).count();
    let note = (not_found > 0).then(|| {
        format!("{not_found} line{} not found in the singing (grey): placed between their neighbours.", if not_found == 1 { "" } else { "s" })
    });
    Alignment { note, lines, matched, source: AlignSource::Local }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_raw_f32_emissions() {
        let path = std::env::temp_dir().join(format!("mk-em-{}.f32", std::process::id()));
        let values: Vec<f32> = (0..6).map(|i| -(i as f32)).collect();
        std::fs::write(&path, values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        let em = read_emissions_file(&path, 2, 3, 0.02).unwrap();
        assert_eq!(em.logp, values);
        assert!(read_emissions_file(&path, 3, 3, 0.02).is_err(), "size mismatch is an error");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_recommended_model_is_the_default() {
        let store = Store::in_memory().unwrap();
        assert_eq!(model(&store).key, "align:wav2vec2-base-960h-int8");
        store.set_setting(SETTING, "align:gone");
        assert!(model(&store).recommended, "an unknown saved model falls back");
        assert!(ALIGN_MODELS.iter().all(|m| m.key.starts_with("align:")), "never a separation model key");
    }

    /// Real model on a real stem: `cargo test live_local_align -- --ignored --nocapture`
    /// Needs the app's engine and a downloaded model; prints timings and memory.
    #[test]
    #[ignore]
    fn live_local_align() {
        let env = PythonEnv::new();
        env.check(egui::Context::default());
        while matches!(env.status(), EnvStatus::Checking) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let store = Arc::new(Store::open(&env.paths.database()).unwrap());
        let mm = ModelManager::new(env.clone(), store.clone());
        let vocals = std::env::var("STAGEWEAVE_ALIGN_VOCALS").expect("set STAGEWEAVE_ALIGN_VOCALS to a vocals.wav");
        let lyrics = std::fs::read_to_string(std::env::var("STAGEWEAVE_ALIGN_LYRICS").expect("set STAGEWEAVE_ALIGN_LYRICS")).unwrap();
        let lines: Vec<String> = lyrics.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect();
        let duration = crate::engine::decode_file_for_tests(Path::new(&vocals)).0.duration();
        let allow_download = std::env::var("STAGEWEAVE_ALIGN_DOWNLOAD").is_ok();
        let req = AlignRequest { vocals: vocals.into(), lines, duration, model: model(&store), allow_download };
        let cancel = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        let r = run(&req, &env, &mm, &cancel, &|e| {
            if let JobEvent::Stage(s) = e {
                println!("{s}");
            }
        })
        .unwrap();
        println!("total {:.1} s", t0.elapsed().as_secs_f64());
        for l in &r.lines {
            println!("{:7.2}-{:7.2}  {:.2}  {:?}  {}", l.start, l.end, l.confidence, l.status, l.text);
        }
    }
}
