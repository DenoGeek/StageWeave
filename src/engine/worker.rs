//! Running the Python worker and parsing its JSON-lines protocol.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::model::StemKind;

use super::python_env::PythonEnv;

#[derive(Debug, PartialEq)]
pub enum WorkerMsg {
    Stage(String),
    Progress(f32),
    Download { done: u64, total: Option<u64>, file: String },
    Model { key: String, downloaded: bool, on_disk: u64, total: Option<u64> },
    Stem(StemKind, PathBuf),
    Done,
    Error(String),
}

pub fn stem_kind(name: &str) -> Option<StemKind> {
    use StemKind::*;
    Some(match name {
        "vocals" => Vocals,
        "drums" => Drums,
        "bass" => Bass,
        "guitar" => Guitar,
        "piano" => Piano,
        "other" => Other,
        "instrumental" => Instrumental,
        _ => return None,
    })
}

/// Parse one protocol line. Non-protocol lines return `None`.
pub fn parse_line(line: &str) -> Option<WorkerMsg> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    Some(match v.get("t")?.as_str()? {
        "stage" => WorkerMsg::Stage(v["text"].as_str().unwrap_or("").to_owned()),
        "progress" => WorkerMsg::Progress(v["v"].as_f64()? as f32),
        "download" => WorkerMsg::Download {
            done: v["done"].as_u64()?,
            total: v["total"].as_u64().filter(|t| *t > 0),
            file: v["file"].as_str().unwrap_or("").to_owned(),
        },
        "model" => WorkerMsg::Model {
            key: v["key"].as_str()?.to_owned(),
            downloaded: v["downloaded"].as_bool()?,
            on_disk: v["on_disk"].as_u64().unwrap_or(0),
            total: v["total"].as_u64().filter(|t| *t > 0),
        },
        "stem" => WorkerMsg::Stem(stem_kind(v["kind"].as_str()?)?, PathBuf::from(v["path"].as_str()?)),
        "done" => WorkerMsg::Done,
        "error" => WorkerMsg::Error(v["msg"].as_str().unwrap_or("Unknown error").to_owned()),
        _ => return None,
    })
}

#[derive(Debug)]
pub struct WorkerError {
    pub message: String,
    pub stderr_tail: Vec<String>,
    pub cancelled: bool,
}

/// Run `worker.py <args>`, calling `on_msg` for each protocol message.
/// Killing the child is how cancellation works.
pub fn run(
    env: &Arc<PythonEnv>,
    args: &[OsString],
    cancel: Option<&AtomicBool>,
    mut on_msg: impl FnMut(WorkerMsg),
) -> Result<(), WorkerError> {
    let err = |message: String| WorkerError { message, stderr_tail: Vec::new(), cancelled: false };
    let mut cmd = env.worker_command().map_err(err)?;
    // The worker exits when this pipe closes, so it never outlives the app.
    cmd.args(args)
        .env("MUSICKIT_WATCH_PARENT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.log(format!("$ worker {}", args.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")));
    let mut child = cmd.spawn().map_err(|e| err(format!("Couldn't start the engine: {e}")))?;

    let (tx, rx) = channel();
    let stdout = child.stdout.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let tail: Arc<Mutex<Vec<String>>> = Arc::default();
    let stderr = child.stderr.take().expect("piped");
    let (tail_w, env_log) = (tail.clone(), env.clone());
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            env_log.log(line.clone());
            let mut t = tail_w.lock().unwrap();
            t.push(line);
            let excess = t.len().saturating_sub(60);
            t.drain(..excess);
        }
    });

    let mut error = None;
    let mut done = false;
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerError { message: "Cancelled".into(), stderr_tail: Vec::new(), cancelled: true });
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => match parse_line(&line) {
                Some(WorkerMsg::Done) => done = true,
                Some(WorkerMsg::Error(e)) => error = Some(e),
                Some(msg) => on_msg(msg),
                None => env.log(line),
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| err(e.to_string()))?;
    // `done` is only sent after the work is complete, so a crash during
    // Python's shutdown must not turn a finished job into a failure.
    if done && error.is_none() {
        return Ok(());
    }
    let stderr_tail = tail.lock().unwrap().clone();
    let message = error.unwrap_or_else(|| {
        let last = stderr_tail.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
        format!("Engine exited ({status}). {last}")
    });
    Err(WorkerError { message, stderr_tail, cancelled: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_lines() {
        assert_eq!(parse_line(r#"{"t":"progress","v":0.5}"#), Some(WorkerMsg::Progress(0.5)));
        assert_eq!(parse_line(r#"{"t":"stage","text":"Loading"}"#), Some(WorkerMsg::Stage("Loading".into())));
        assert_eq!(
            parse_line(r#"{"t":"stem","kind":"vocals","path":"/x/vocals.wav"}"#),
            Some(WorkerMsg::Stem(StemKind::Vocals, "/x/vocals.wav".into()))
        );
        assert_eq!(
            parse_line(r#"{"t":"download","done":10,"total":100,"file":"a.onnx"}"#),
            Some(WorkerMsg::Download { done: 10, total: Some(100), file: "a.onnx".into() })
        );
        assert_eq!(
            parse_line(r#"{"t":"download","done":10,"total":null,"file":"a"}"#),
            Some(WorkerMsg::Download { done: 10, total: None, file: "a".into() })
        );
        assert_eq!(
            parse_line(r#"{"t":"model","key":"uvr:a","downloaded":true,"on_disk":5,"total":5}"#),
            Some(WorkerMsg::Model { key: "uvr:a".into(), downloaded: true, on_disk: 5, total: Some(5) })
        );
        assert_eq!(parse_line(r#"{"t":"done"}"#), Some(WorkerMsg::Done));
        assert_eq!(parse_line(r#"{"t":"error","msg":"boom"}"#), Some(WorkerMsg::Error("boom".into())));
    }

    #[test]
    fn ignores_malformed_lines() {
        assert_eq!(parse_line("Downloading model..."), None);
        assert_eq!(parse_line(r#"{"t":"progress"}"#), None);
        assert_eq!(parse_line(r#"{"t":"stem","kind":"kazoo","path":"/x"}"#), None);
        assert_eq!(parse_line(r#"{"foo":1}"#), None);
        assert_eq!(parse_line(""), None);
    }
}
