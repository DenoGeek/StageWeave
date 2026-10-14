//! Runs Demucs / UVR models in the Python worker process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::model::StemKind;
use crate::store::{SeparationRecord, now};

use super::models::ModelManager;
use super::python_env::{EnvStatus, PythonEnv};
use super::worker::{self, WorkerMsg};
use super::{Engine, SplitModel, StemOutput, StemSplitter};

pub struct PythonSplitter {
    pub env: Arc<PythonEnv>,
    pub models: Arc<ModelManager>,
    /// stderr tail of the last failed run, for the "Details" view.
    pub last_error_log: Arc<Mutex<Vec<String>>>,
}

impl PythonSplitter {
    pub fn new(env: Arc<PythonEnv>, models: Arc<ModelManager>) -> Self {
        Self { env, models, last_error_log: Arc::default() }
    }
}

pub fn stem_file_name(kind: StemKind) -> String {
    format!("{}.wav", kind.label().to_lowercase())
}

/// The models offered in the UI.
pub fn curated_models() -> Vec<SplitModel> {
    use StemKind::*;
    let m = |engine, id: &str, name: &str, description: &str, tag: &str, stems: Vec<StemKind>| SplitModel {
        id: id.into(),
        engine,
        name: name.into(),
        description: description.into(),
        tag: tag.into(),
        stems,
    };
    vec![
        m(Engine::Demucs, "htdemucs", "4 Stems", "Vocals · Drums · Bass · Other", "Fast", vec![Vocals, Drums, Bass, Other]),
        m(Engine::Demucs, "htdemucs_ft", "4 Stems HQ", "Fine-tuned, about 4× slower", "Best", vec![Vocals, Drums, Bass, Other]),
        m(Engine::Demucs, "htdemucs_6s", "6 Stems", "Adds Guitar and Piano", "Fast", vec![Vocals, Drums, Bass, Guitar, Piano, Other]),
        m(
            Engine::Uvr,
            "model_bs_roformer_ep_317_sdr_12.9755.ckpt",
            "Karaoke · Roformer",
            "Cleanest vocals · slow (≈3× song length)",
            "Best",
            vec![Vocals, Instrumental],
        ),
        m(Engine::Uvr, "UVR-MDX-NET-Inst_HQ_3.onnx", "Karaoke · MDX-Net", "Vocals / instrumental", "Fast", vec![Vocals, Instrumental]),
        m(Engine::Uvr, "UVR_MDXNET_KARA_2.onnx", "Lead / Backing", "Lead vocal vs. backing + band", "Fast", vec![Vocals, Instrumental]),
    ]
}

impl StemSplitter for PythonSplitter {
    fn models(&self) -> Vec<SplitModel> {
        curated_models()
    }

    fn split(
        &self,
        source: &Path,
        model: &SplitModel,
        out_dir: &Path,
        progress: &dyn Fn(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<Vec<StemOutput>, String> {
        let info = match self.env.status() {
            EnvStatus::Ready(info) => info,
            _ => return Err("The separation engine isn't installed yet".into()),
        };
        let input = source;
        let expected: Vec<PathBuf> = model.stems.iter().map(|k| out_dir.join(stem_file_name(*k))).collect();

        if expected.iter().all(|p| p.exists()) {
            progress(1.0, "Using cached stems");
        } else {
            self.models.ensure_downloaded(&model.key(), progress, cancel)?;
            let started = Instant::now();
            self.run_separation(&info, model, input, out_dir, progress, cancel)?;
            self.models.store.record_separation(&SeparationRecord {
                source_path: input.display().to_string(),
                model_key: model.key(),
                out_dir: out_dir.display().to_string(),
                stems: model.stems.len() as u32,
                seconds: started.elapsed().as_secs_f64(),
                created_at: now(),
            });
        }
        self.models.store.touch_model(&model.key());

        // Stems we got, in model order (missing optional ones are skipped).
        let stems: Vec<StemOutput> = model
            .stems
            .iter()
            .map(|k| (*k, out_dir.join(stem_file_name(*k))))
            .filter(|(_, p)| p.exists())
            .map(|(kind, path)| StemOutput { kind, peaks: Default::default(), path: Some(path), audio: None })
            .collect();
        if stems.is_empty() {
            return Err("The model produced no stems".into());
        }
        Ok(stems)
    }
}

impl PythonSplitter {
    fn run_separation(
        &self,
        info: &super::python_env::EnvInfo,
        model: &SplitModel,
        input: &Path,
        out_dir: &Path,
        progress: &dyn Fn(f32, &str),
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let engine = match model.engine {
            Engine::Demucs => "demucs",
            Engine::Uvr => "uvr",
            Engine::Mock => return Err("Preview models can't run on the real engine".into()),
        };
        // Start from a clean folder so partial results never look cached.
        let _ = std::fs::remove_dir_all(out_dir);
        std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;

        let args: Vec<OsString> = vec![
            "separate".into(),
            "--engine".into(),
            engine.into(),
            "--model".into(),
            model.id.clone().into(),
            "--input".into(),
            input.into(),
            "--out".into(),
            out_dir.into(),
            "--device".into(),
            self.env.device().arg(Some(info)).into(),
            "--models-dir".into(),
            self.env.paths.models.join("uvr").into(),
        ];
        progress(-1.0, "Starting engine");
        let mut stage = "Starting engine".to_owned();
        let mut frac = -1.0f32;
        let result = worker::run(&self.env, &args, Some(cancel), |msg| match msg {
            WorkerMsg::Stage(s) => {
                stage = s;
                progress(frac, &stage);
            }
            WorkerMsg::Progress(v) => {
                frac = v;
                progress(frac, &stage);
            }
            WorkerMsg::Download { done, total, .. } => {
                let f = total.map_or(-1.0, |t| done as f32 / t.max(1) as f32);
                progress(f, &format!("Downloading model · {}", super::models::transfer_label(done, total, 0.0)));
            }
            _ => {}
        });
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(out_dir);
            if !e.cancelled {
                *self.last_error_log.lock().unwrap() = e.stderr_tail;
            }
            return Err(e.message);
        }
        Ok(())
    }
}
