//! AI lyric transcription through Whisper-compatible APIs (OpenAI, Groq).
//!
//! The open song's audio (preferably the isolated vocals stem) is downmixed to
//! 16 kHz mono WAV, uploaded, and the timestamped segments come back as
//! lyric lines. API keys are kept in a private file (`secrets.json`, owner-only
//! permissions) in the app-data folder, never in the database; environment
//! variables (`OPENAI_API_KEY`, `GROQ_API_KEY`) are used when no key is saved.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::model::{AudioBuffer, LyricLine};
use crate::store::Store;

const TARGET_RATE: u32 = 16_000;
/// Providers reject uploads over 25 MB; stay safely below.
const MAX_UPLOAD_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    OpenAi,
    Groq,
}

pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// Supports `verbose_json` with segment timestamps.
    pub timestamps: bool,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::OpenAi, Provider::Groq];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Groq => "Groq",
        }
    }

    /// Environment variable checked when no key is saved.
    pub fn env_var(self) -> &'static str {
        match self {
            Self::OpenAi => "OPENAI_API_KEY",
            Self::Groq => "GROQ_API_KEY",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Groq => "groq",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    fn base_url(self) -> String {
        // Dev/testing override (e.g. a local fake server).
        if let Ok(url) = std::env::var("STAGEWEAVE_AI_BASE_URL") {
            return url;
        }
        match self {
            Self::OpenAi => "https://api.openai.com/v1",
            Self::Groq => "https://api.groq.com/openai/v1",
        }
        .to_owned()
    }

    pub fn keys_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://platform.openai.com/api-keys",
            Self::Groq => "https://console.groq.com/keys",
        }
    }

    pub fn models(self) -> &'static [ModelInfo] {
        match self {
            Self::OpenAi => &[
                ModelInfo { id: "whisper-1", label: "Whisper", timestamps: true },
                ModelInfo { id: "gpt-4o-transcribe", label: "GPT-4o Transcribe (no timing)", timestamps: false },
                ModelInfo { id: "gpt-4o-mini-transcribe", label: "GPT-4o mini Transcribe (no timing)", timestamps: false },
            ],
            Self::Groq => &[
                ModelInfo { id: "whisper-large-v3", label: "Whisper Large v3", timestamps: true },
                ModelInfo { id: "whisper-large-v3-turbo", label: "Whisper Large v3 Turbo (faster)", timestamps: true },
            ],
        }
    }
}

pub const LANGUAGES: &[(&str, &str)] = &[
    ("", "Auto-detect"),
    ("en", "English"),
    ("sw", "Swahili"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("de", "German"),
    ("it", "Italian"),
    ("yo", "Yoruba"),
    ("zu", "Zulu"),
    ("am", "Amharic"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("zh", "Chinese"),
    ("ko", "Korean"),
    ("ja", "Japanese"),
];

// --------------------------------------------------------------------------- settings & keys

/// Where a key came from (shown in Settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Saved,
    Environment,
}

/// Keys (private file, falling back to environment variables) and
/// preferences (SQLite).
pub struct AiSettings {
    store: Arc<Store>,
    path: std::path::PathBuf,
    saved: Mutex<HashMap<String, String>>,
}

impl AiSettings {
    pub fn new(store: Arc<Store>) -> Self {
        let dir = crate::engine::python_env::app_dirs().0;
        Self::with_path(store, dir.join("secrets.json"))
    }

    pub fn with_path(store: Arc<Store>, path: std::path::PathBuf) -> Self {
        let saved = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { store, path, saved: Mutex::new(saved) }
    }

    pub fn key_with_source(&self, p: Provider) -> Option<(String, KeySource)> {
        if let Some(k) = self.saved.lock().unwrap().get(p.id()).filter(|k| !k.is_empty()) {
            return Some((k.clone(), KeySource::Saved));
        }
        std::env::var(p.env_var()).ok().filter(|k| !k.trim().is_empty()).map(|k| (k.trim().to_owned(), KeySource::Environment))
    }

    pub fn key(&self, p: Provider) -> Option<String> {
        self.key_with_source(p).map(|(k, _)| k)
    }

    pub fn has_key(&self, p: Provider) -> bool {
        self.key(p).is_some()
    }

    pub fn set_key(&self, p: Provider, key: &str) -> Result<(), String> {
        let mut saved = self.saved.lock().unwrap();
        saved.insert(p.id().to_owned(), key.trim().to_owned());
        self.write(&saved)
    }

    pub fn remove_key(&self, p: Provider) {
        let mut saved = self.saved.lock().unwrap();
        saved.remove(p.id());
        let _ = self.write(&saved);
    }

    /// Write via a temp file created with owner-only permissions, then rename.
    fn write(&self, saved: &HashMap<String, String>) -> Result<(), String> {
        let dir = self.path.parent().ok_or("Bad secrets path")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| format!("Couldn't save the key: {e}"))?;
        use std::io::Write;
        f.write_all(serde_json::to_string_pretty(saved).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("Couldn't save the key: {e}"))
    }

    pub fn configured(&self) -> Vec<Provider> {
        Provider::ALL.into_iter().filter(|p| self.has_key(*p)).collect()
    }

    /// Preferred provider: the saved choice if it has a key, else the first configured one.
    pub fn provider(&self) -> Option<Provider> {
        let saved = self.store.setting("ai_provider").and_then(|s| Provider::from_id(&s));
        saved.filter(|p| self.has_key(*p)).or_else(|| self.configured().first().copied())
    }

    pub fn set_provider(&self, p: Provider) {
        self.store.set_setting("ai_provider", p.id());
    }

    pub fn model(&self, p: Provider) -> &'static ModelInfo {
        let saved = self.store.setting(&format!("ai_model_{}", p.id()));
        p.models().iter().find(|m| Some(m.id) == saved.as_deref()).unwrap_or(&p.models()[0])
    }

    pub fn set_model(&self, p: Provider, id: &str) {
        self.store.set_setting(&format!("ai_model_{}", p.id()), id);
    }

    pub fn language(&self) -> String {
        self.store.setting("ai_language").unwrap_or_default()
    }

    pub fn set_language(&self, code: &str) {
        self.store.set_setting("ai_language", code);
    }
}

// --------------------------------------------------------------------------- audio

/// Downmix + resample to 16 kHz mono 16-bit WAV (what Whisper uses internally).
pub fn encode_wav_16k_mono(buf: &AudioBuffer, from: f64, to: f64) -> Vec<u8> {
    let src_rate = buf.sample_rate as f64;
    let frames = buf.frames();
    let start = ((from * src_rate) as usize).min(frames);
    let end = ((to * src_rate) as usize).min(frames);
    let step = src_rate / TARGET_RATE as f64;
    let out_len = ((end - start) as f64 / step) as usize;

    let mut pcm = Vec::with_capacity(out_len * 2);
    for i in 0..out_len {
        // Box-filter average over the source frames this output sample covers.
        let a = start + (i as f64 * step) as usize;
        let b = (start + ((i + 1) as f64 * step) as usize).clamp(a + 1, end.max(a + 1));
        let mut sum = 0.0f32;
        for f in a..b.min(frames) {
            sum += (buf.data[f * 2] + buf.data[f * 2 + 1]) * 0.5;
        }
        let v = (sum / (b - a) as f32).clamp(-1.0, 1.0);
        pcm.extend_from_slice(&((v * 32_767.0) as i16).to_le_bytes());
    }

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&TARGET_RATE.to_le_bytes());
    wav.extend_from_slice(&(TARGET_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

/// Longest audio that fits in one upload.
fn max_chunk_secs() -> f64 {
    (MAX_UPLOAD_BYTES - 44) as f64 / (TARGET_RATE as f64 * 2.0)
}

/// Where to cut audio longer than one upload. Normal songs return a single
/// piece (one request keeps Whisper's context, so it's the most accurate).
/// Longer audio is cut at the quietest half-second in the last quarter
/// before each size limit; on a vocals stem that's a real gap between phrases, so no
/// word is split.
pub fn split_points(peaks: &crate::model::Peaks, duration: f64, max_secs: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = 0.0;
    while duration - start > max_secs {
        let limit = start + max_secs;
        let (from, window) = (limit - max_secs * 0.25, 0.5);
        let mut best = (f32::MAX, limit);
        let mut t = from;
        while t + window <= limit {
            let e = peaks.max_in(t, t + window);
            if e < best.0 {
                best = (e, t + window / 2.0);
            }
            t += 0.1;
        }
        out.push((start, best.1));
        start = best.1;
    }
    out.push((start, duration));
    out
}

/// Upload-sized pieces: (offset seconds, wav bytes).
fn chunks(buf: &AudioBuffer) -> Vec<(f64, Vec<u8>)> {
    let peaks = buf.peaks(crate::engine::PEAKS_PER_SEC);
    split_points(&peaks, buf.duration(), max_chunk_secs())
        .into_iter()
        .map(|(a, b)| (a, encode_wav_16k_mono(buf, a, b)))
        .collect()
}

/// At most this many uploads in flight (keeps clear of provider rate limits).
const PARALLEL_UPLOADS: usize = 3;

// --------------------------------------------------------------------------- API

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub no_speech_prob: f64,
    /// Whisper's confidence (log-probability per token); very low = guessing.
    #[serde(default)]
    pub avg_logprob: f64,
    /// High values mean repetition loops (a classic hallucination).
    #[serde(default)]
    pub compression_ratio: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
    /// Present when word timestamps were requested.
    #[serde(default)]
    words: Vec<ApiWord>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ApiWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Transcript {
    /// e.g. "24 pieces · 2 with no singing · language: Swahili"
    pub note: Option<String>,
    pub text: String,
    pub language: Option<String>,
    /// Empty when the model doesn't return timestamps.
    pub lines: Vec<LyricLine>,
}

fn api_error(status: u16, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect());
    match status {
        401 => format!("The API key was rejected ({msg})"),
        413 => "The audio is too large for the API".into(),
        429 => format!("Rate limited or out of credit: {msg}"),
        _ => format!("HTTP {status}: {msg}"),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

/// Cheap authenticated call to check a key.
pub fn test_key(p: Provider, key: &str) -> Result<(), String> {
    let mut resp = agent()
        .get(&format!("{}/models", p.base_url()))
        .header("Authorization", &format!("Bearer {}", key.trim()))
        .call()
        .map_err(|e| format!("Network error: {e}"))?;
    let status = resp.status().as_u16();
    if status == 200 {
        Ok(())
    } else {
        Err(api_error(status, &resp.body_mut().read_to_string().unwrap_or_default()))
    }
}

fn multipart(fields: &[(&str, &str)], wav: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("stageweave-{:016x}", crate::store::now() as u64 ^ wav.len() as u64 ^ 0x9e37_79b9_7f4a_7c15);
    let mut body = Vec::with_capacity(wav.len() + 1024);
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

fn transcribe_chunk(
    p: Provider,
    key: &str,
    model: &ModelInfo,
    language: &str,
    prompt: &str,
    words: bool,
    wav: &[u8],
) -> Result<ApiResponse, String> {
    let mut fields = vec![("model", model.id), ("temperature", "0")];
    if model.timestamps {
        fields.push(("response_format", "verbose_json"));
        fields.push(("timestamp_granularities[]", "segment"));
        if words {
            fields.push(("timestamp_granularities[]", "word"));
        }
    } else {
        fields.push(("response_format", "json"));
    }
    if !language.is_empty() {
        fields.push(("language", language));
    }
    if !prompt.is_empty() {
        fields.push(("prompt", prompt));
    }
    let (boundary, body) = multipart(&fields, wav);
    // Retry rate limits and server hiccups, honouring Retry-After.
    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = agent()
            .post(&format!("{}/audio/transcriptions", p.base_url()))
            .header("Authorization", &format!("Bearer {key}"))
            .header("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send(&body[..]);
        let mut resp = match result {
            Ok(r) => r,
            Err(e) if attempt < MAX_ATTEMPTS => {
                std::thread::sleep(backoff(attempt, None));
                let _ = e;
                continue;
            }
            Err(e) => return Err(format!("Network error: {e}")),
        };
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok());
        let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
        if status == 200 {
            return serde_json::from_str(&text).map_err(|e| format!("Unexpected response: {e}"));
        }
        if (status == 429 || status >= 500) && attempt < MAX_ATTEMPTS {
            std::thread::sleep(backoff(attempt, retry_after));
            continue;
        }
        return Err(api_error(status, &text));
    }
}

const MAX_ATTEMPTS: u32 = 4;

/// Wait before retry `attempt` (1-based): Retry-After if given (capped), else 1 s, 2 s, 4 s.
fn backoff(attempt: u32, retry_after: Option<f64>) -> Duration {
    let secs = retry_after.unwrap_or(2f64.powi(attempt as i32 - 1)).clamp(0.2, 20.0);
    Duration::from_secs_f64(secs)
}

/// Turn Whisper segments into lyric lines: drop silence hallucinations and
/// split long segments at punctuation so lines fit on screen.
pub fn segments_to_lines(segments: &[Segment]) -> Vec<LyricLine> {
    const MAX_CHARS: usize = 48;
    let mut out = Vec::new();
    for s in segments {
        let text = s.text.trim();
        let is_noise = text.is_empty()
            || s.no_speech_prob > 0.6
            || text.chars().all(|c| !c.is_alphanumeric())
            || matches!(text.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()), "music" | "instrumental");
        if is_noise || s.end <= s.start {
            continue;
        }
        if text.chars().count() <= MAX_CHARS {
            out.push(LyricLine::new(text.to_owned(), s.start, s.end));
            continue;
        }
        // Split at commas / sentence ends, timing pieces by character share.
        let mut pieces: Vec<String> = Vec::new();
        let mut cur = String::new();
        for word in text.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + word.chars().count() > MAX_CHARS {
                pieces.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
            if word.ends_with([',', '.', '!', '?', ';']) && cur.chars().count() > MAX_CHARS / 2 {
                pieces.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            pieces.push(cur);
        }
        let total: usize = pieces.iter().map(|p| p.chars().count()).sum::<usize>().max(1);
        let mut t = s.start;
        for p in pieces {
            let len = (s.end - s.start) * p.chars().count() as f64 / total as f64;
            out.push(LyricLine::new(p.trim_end_matches(',').to_owned(), t, t + len));
            t += len;
        }
    }
    out
}

// --------------------------------------------------------------------------- phrase-by-phrase

/// Whisper's verbose_json names languages ("swahili"); requests need ISO codes.
pub fn language_code(name: &str) -> Option<&'static str> {
    const NAMES: &[(&str, &str)] = &[
        ("english", "en"), ("swahili", "sw"), ("french", "fr"), ("spanish", "es"), ("portuguese", "pt"),
        ("german", "de"), ("italian", "it"), ("yoruba", "yo"), ("zulu", "zu"), ("amharic", "am"),
        ("arabic", "ar"), ("hindi", "hi"), ("chinese", "zh"), ("korean", "ko"), ("japanese", "ja"),
        ("dutch", "nl"), ("russian", "ru"), ("hausa", "ha"), ("somali", "so"), ("lingala", "ln"),
        ("shona", "sn"), ("afrikaans", "af"), ("igbo", "ig"), ("kinyarwanda", "rw"), ("luganda", "lg"),
        ("tagalog", "tl"), ("indonesian", "id"), ("polish", "pl"), ("turkish", "tr"), ("ukrainian", "uk"),
    ];
    let n = name.trim().to_lowercase();
    if n.len() == 2 && NAMES.iter().any(|(_, c)| *c == n) {
        return NAMES.iter().find(|(_, c)| *c == n).map(|(_, c)| *c);
    }
    NAMES.iter().find(|(full, _)| *full == n).map(|(_, c)| *c)
}

/// Readable name for a code ("sw" → "Swahili").
pub fn language_label(code: &str) -> String {
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or_else(|| code.to_owned(), |(_, n)| (*n).to_owned())
}

const FILLER: &[&str] = &["thank you", "thanks for watching", "you", "bye", "subtitles by", "amara.org", "please subscribe", "transcribed by"];

fn is_filler(text: &str) -> bool {
    let t: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '.').collect();
    let t = t.trim().trim_end_matches('.').trim();
    FILLER.iter().any(|f| t == *f || (f.len() > 8 && t.contains(f)))
}

/// Drop Whisper's usual hallucinations from one piece's response.
fn clean_response(mut r: ApiResponse) -> ApiResponse {
    let keep_seg = |s: &Segment| {
        let dur = (s.end - s.start).max(0.1);
        let words = s.text.split_whitespace().count() as f64;
        !(s.no_speech_prob > 0.6
            || (s.avg_logprob != 0.0 && s.avg_logprob < -1.0)
            || s.compression_ratio > 2.4
            || is_filler(&s.text)
            || words / dur > 6.0
            || s.text.trim().is_empty())
    };
    let mut kept: Vec<Segment> = r.segments.iter().filter(|s| keep_seg(s)).cloned().collect();
    // Collapse loops: the same line 3+ times in a row inside one piece.
    let norm = |t: &str| crate::align::normalize(&t.replace(' ', ""));
    let mut out: Vec<Segment> = Vec::new();
    let mut k = 0;
    while k < kept.len() {
        let mut run = 1;
        while k + run < kept.len() && norm(&kept[k + run].text) == norm(&kept[k].text) {
            run += 1;
        }
        let take = if run >= 3 { 1 } else { run };
        out.extend(kept[k..k + take].iter().cloned());
        k += run;
    }
    kept = out;
    let had_segments = !r.segments.is_empty();
    r.words.retain(|w| {
        let mid = (w.start + w.end) / 2.0;
        if had_segments { kept.iter().any(|s| mid >= s.start - 0.3 && mid <= s.end + 0.3) } else { !is_filler(&r.text) }
    });
    if had_segments {
        r.text = kept.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
    } else if is_filler(&r.text) {
        r.text.clear();
    }
    r.segments = kept;
    r
}

/// Transcribe vocal pieces (3 at a time) and stitch them back into song time.
fn run_pieces(
    svc: &Service,
    req: &TranscribeRequest,
    pieces: &[crate::chunking::Piece],
    cancel: &AtomicBool,
    stage: &dyn Fn(String),
) -> Result<(ServiceOutput, String), String> {
    let n = pieces.len();
    if n == 0 {
        return Err("No singing found in the vocals stem".into());
    }
    let prompt: String = req.prompt.chars().take(300).collect();
    let call = |i: usize, lang: &str| -> Result<ApiResponse, String> {
        let p = &pieces[i];
        let wav = encode_wav_16k_mono(&req.audio, p.start, p.end);
        transcribe_chunk(svc.provider, &svc.key, svc.model, lang, &prompt, true, &wav).map(clean_response)
    };
    let mut results: Vec<Option<Result<ApiResponse, String>>> = (0..n).map(|_| None).collect();

    // Language lock: with auto-detect, the longest piece decides for all.
    let mut lang = req.language.clone();
    let mut order: Vec<usize> = (0..n).collect();
    if lang.is_empty() {
        let longest = (0..n).max_by(|&a, &b| pieces[a].len().total_cmp(&pieces[b].len())).unwrap_or(0);
        stage("Detecting the language…".into());
        let r = call(longest, "");
        if let Ok(resp) = &r
            && let Some(code) = resp.language.as_deref().and_then(language_code)
        {
            lang = code.to_owned();
        }
        if let Err(e) = &r
            && e.contains("rejected")
        {
            return Err(e.clone());
        }
        results[longest] = Some(r);
        order.retain(|&i| i != longest);
    }

    let done = std::sync::atomic::AtomicUsize::new(n - order.len());
    for batch in order.chunks(PARALLEL_UPLOADS) {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        stage(format!("Transcribing piece {} of {n}…", done.load(Ordering::Relaxed) + 1));
        std::thread::scope(|scope| {
            let (call, lang) = (&call, &lang);
            let handles: Vec<_> = batch.iter().map(|&i| (i, scope.spawn(move || call(i, lang)))).collect();
            for (i, h) in handles {
                results[i] = Some(h.join().unwrap_or_else(|_| Err("Transcription thread failed".into())));
                done.fetch_add(1, Ordering::Relaxed);
            }
        });
        // A bad key fails every piece the same way: stop early.
        if let Some(Err(e)) = batch.iter().find_map(|&i| results[i].as_ref().filter(|r| r.is_err()))
            && e.contains("rejected")
        {
            return Err(e.clone());
        }
    }

    // Stitch back into song time.
    let mut text = String::new();
    let (mut segments, mut words) = (Vec::new(), Vec::new());
    let (mut silent, mut failed, mut first_err) = (0, 0, None);
    for (p, r) in pieces.iter().zip(results) {
        match r.unwrap_or_else(|| Err("Missing result".into())) {
            Ok(r) => {
                if r.text.trim().is_empty() {
                    silent += 1;
                    continue;
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(r.text.trim());
                segments.extend(r.segments.into_iter().map(|mut s| {
                    s.start += p.start;
                    s.end += p.start;
                    s
                }));
                words.extend(r.words.into_iter().map(|w| crate::align::Word { text: w.word, start: w.start + p.start, end: w.end + p.start }));
            }
            Err(e) => {
                failed += 1;
                first_err.get_or_insert(e);
            }
        }
    }
    if failed == n {
        return Err(first_err.unwrap_or_else(|| "Every piece failed".into()));
    }
    let mut note = format!("{n} pieces");
    if silent > 0 {
        note.push_str(&format!(" · {silent} with no singing"));
    }
    if failed > 0 {
        note.push_str(&format!(" · {failed} failed ({})", first_err.unwrap_or_default()));
    }
    if !lang.is_empty() {
        note.push_str(&format!(" · language: {}", language_label(&lang)));
    }
    let language = (!lang.is_empty()).then(|| lang.clone());
    Ok((ServiceOutput { text, language, segments, words }, note))
}

/// One lyric line per sung phrase: each recognised word goes to the phrase
/// it falls in. Without word timing, fall back to Whisper's segments.
pub fn lines_from_phrases(phrases: &[(f64, f64)], words: &[crate::align::Word], segments: &[Segment]) -> Vec<LyricLine> {
    if words.is_empty() {
        return segments_to_lines(segments);
    }
    let mut buckets: Vec<Vec<&str>> = vec![Vec::new(); phrases.len()];
    for w in words {
        let mid = (w.start + w.end) / 2.0;
        let nearest = phrases
            .iter()
            .enumerate()
            .map(|(i, &(a, b))| (i, if mid < a { a - mid } else if mid > b { mid - b } else { 0.0 }))
            .min_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((i, d)) = nearest
            && d <= 1.5
        {
            buckets[i].push(w.text.trim());
        }
    }
    let mut lines = Vec::new();
    for (&(start, end), ws) in phrases.iter().zip(buckets) {
        let text = ws.into_iter().filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        // Long phrases are split into screen-sized lines.
        let seg = Segment { start, end, text, no_speech_prob: 0.0, avg_logprob: 0.0, compression_ratio: 0.0 };
        lines.extend(segments_to_lines(std::slice::from_ref(&seg)));
    }
    lines
}

// --------------------------------------------------------------------------- background job

pub enum JobResult {
    Transcript(Transcript),
    Alignment(crate::align::Alignment),
}

pub enum JobEvent {
    Stage(String),
    /// 0–1, or negative for "working, no estimate".
    Progress(f32),
    Done(Result<JobResult, String>),
}

pub struct TranscribeJob {
    pub song_id: i64,
    pub rx: Receiver<JobEvent>,
    pub cancel: Arc<AtomicBool>,
    pub stage: String,
    /// Latest progress (0–1) when the job reports it.
    pub progress: Option<f32>,
    pub started: std::time::Instant,
}

/// Which Whisper service to call.
pub struct Service {
    pub provider: Provider,
    pub key: String,
    pub model: &'static ModelInfo,
}

pub enum JobMode {
    /// Write out the lyrics from the audio.
    Transcribe,
    /// Time the user's lyrics. `service: None` = on-device (vocal energy only).
    Align { lyrics: String },
}

pub struct TranscribeRequest {
    pub mode: JobMode,
    pub service: Option<Service>,
    pub language: String,
    /// Bias for Whisper: title / artist, or the user's lyrics when aligning.
    pub prompt: String,
    pub audio: Arc<AudioBuffer>,
    /// Vocal-stem envelope (phrase detection for alignment).
    pub vocals: Option<crate::model::Peaks>,
    /// Phrase-by-phrase mode: transcribe these vocal pieces (from `audio`,
    /// which is then the vocals stem) instead of the whole song.
    pub pieces: Option<Vec<crate::chunking::Piece>>,
}

/// Whisper prompts are limited (~224 tokens); keep the start of the lyrics.
pub fn lyrics_prompt(title: &str, lyrics: &str) -> String {
    let body: String = lyrics.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(800).collect();
    if title.trim().is_empty() { body } else { format!("{}. {body}", title.trim()) }
}

/// Everything Whisper returned for the whole song (piece offsets applied).
struct ServiceOutput {
    text: String,
    language: Option<String>,
    segments: Vec<Segment>,
    words: Vec<crate::align::Word>,
}

/// Upload all pieces (a few at a time) and stitch the responses back in order.
fn run_service(
    svc: &Service,
    req: &TranscribeRequest,
    words: bool,
    cancel: &AtomicBool,
    stage: &dyn Fn(String),
) -> Result<ServiceOutput, String> {
    let parts = chunks(&req.audio);
    let mb: f64 = parts.iter().map(|(_, w)| w.len() as f64).sum::<f64>() / 1e6;
    let verb = if words { "listening for your lyrics" } else { "transcribing" };
    stage(if parts.len() == 1 {
        format!("Uploading {mb:.1} MB to {} and {verb}…", svc.provider.label())
    } else {
        format!("Uploading {mb:.1} MB in {} pieces to {} and {verb}…", parts.len(), svc.provider.label())
    });

    let mut results: Vec<Option<Result<ApiResponse, String>>> = (0..parts.len()).map(|_| None).collect();
    for batch in (0..parts.len()).collect::<Vec<_>>().chunks(PARALLEL_UPLOADS) {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&i| {
                    let wav = &parts[i].1;
                    (i, scope.spawn(move || transcribe_chunk(svc.provider, &svc.key, svc.model, &req.language, &req.prompt, words, wav)))
                })
                .collect();
            for (i, h) in handles {
                results[i] = Some(h.join().unwrap_or_else(|_| Err("Transcription thread failed".into())));
            }
        });
    }

    let mut text = String::new();
    let (mut segments, mut all_words, mut language) = (Vec::new(), Vec::new(), None);
    for ((offset, _), r) in parts.iter().zip(results) {
        let r = r.unwrap_or_else(|| Err("Missing result".into()))?;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(r.text.trim());
        language = language.or(r.language);
        segments.extend(r.segments.into_iter().map(|mut s| {
            s.start += offset;
            s.end += offset;
            s
        }));
        all_words.extend(r.words.into_iter().map(|w| crate::align::Word { text: w.word, start: w.start + offset, end: w.end + offset }));
    }
    Ok(ServiceOutput { text, language, segments, words: all_words })
}

impl TranscribeJob {
    pub fn spawn(song_id: i64, req: TranscribeRequest, repaint: egui::Context) -> Self {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            let send = |e: JobEvent| {
                let _ = tx.send(e);
                repaint.request_repaint();
            };
            send(JobEvent::Stage("Preparing audio…".into()));
            let stage = |s: String| send(JobEvent::Stage(s));
            let duration = req.audio.duration();
            let result = if let (Some(pieces), Some(svc)) = (&req.pieces, &req.service) {
                run_pieces(svc, &req, pieces, &c, &stage).map(|(out, note)| match &req.mode {
                    JobMode::Transcribe => {
                        let phrases: Vec<(f64, f64)> = pieces.iter().flat_map(|p| p.phrases.iter().copied()).collect();
                        let lines = lines_from_phrases(&phrases, &out.words, &out.segments);
                        JobResult::Transcript(Transcript { note: Some(note), text: out.text, language: out.language, lines })
                    }
                    JobMode::Align { lyrics } => {
                        stage("Matching your lyrics to the singing…".into());
                        let mut a = crate::align::align(lyrics, Some(&out.words), req.vocals.as_ref(), duration);
                        a.note = Some(note);
                        JobResult::Alignment(a)
                    }
                })
            } else {
                match (&req.mode, &req.service) {
                (JobMode::Transcribe, Some(svc)) => run_service(svc, &req, false, &c, &stage).map(|out| {
                    JobResult::Transcript(Transcript { note: None, text: out.text, language: out.language, lines: segments_to_lines(&out.segments) })
                }),
                (JobMode::Transcribe, None) => Err("Choose a transcription service".into()),
                (JobMode::Align { lyrics }, Some(svc)) => run_service(svc, &req, true, &c, &stage).map(|out| {
                    let (segments, mut words) = (out.segments, out.words);
                    // Older/other models may only return segments: use them as coarse "words".
                    if words.is_empty() {
                        words = segments
                            .iter()
                            .flat_map(|s| {
                                let ws: Vec<&str> = s.text.split_whitespace().collect();
                                let step = (s.end - s.start) / ws.len().max(1) as f64;
                                ws.into_iter().enumerate().map(move |(i, w)| crate::align::Word {
                                    text: w.to_owned(),
                                    start: s.start + i as f64 * step,
                                    end: s.start + (i + 1) as f64 * step,
                                })
                            })
                            .collect();
                    }
                    stage("Matching your lyrics to the singing…".into());
                    JobResult::Alignment(crate::align::align(lyrics, Some(&words), req.vocals.as_ref(), duration))
                }),
                (JobMode::Align { lyrics }, None) => {
                    stage("Finding vocal phrases…".into());
                    Ok(JobResult::Alignment(crate::align::align(lyrics, None, req.vocals.as_ref(), duration)))
                }
                }
            };
            if c.load(Ordering::Relaxed) {
                return send(JobEvent::Done(Err("Cancelled".into())));
            }
            send(JobEvent::Done(result));
        });
        Self { song_id, rx, cancel, stage: "Starting…".into(), progress: None, started: std::time::Instant::now() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_is_16k_mono_and_sized_right() {
        let buf = AudioBuffer { sample_rate: 44_100, data: vec![0.25; 44_100 * 2 * 2] }; // 2 s stereo
        let wav = encode_wav_16k_mono(&buf, 0.0, 2.0);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(wav.len(), 44 + 16_000 * 2 * 2);
        let first = i16::from_le_bytes(wav[44..46].try_into().unwrap());
        assert!((first as f32 / 32_767.0 - 0.25).abs() < 0.001);
        // A 6-minute song fits in one upload.
        assert!(6.0 * 60.0 * 16_000.0 * 2.0 < MAX_UPLOAD_BYTES as f64);
    }

    #[test]
    fn normal_songs_are_one_request_long_audio_cuts_at_gaps() {
        use crate::model::Peaks;
        // 6-minute song: a single piece.
        let song = Peaks { per_sec: 100.0, data: vec![0.5; 36_000] };
        assert_eq!(split_points(&song, 360.0, max_chunk_secs()), vec![(0.0, 360.0)]);

        // 30-minute recording with a silent gap at 700-702 s (before the ~786 s limit).
        let mut long = vec![0.5f32; 180_000];
        long[70_000..70_200].iter_mut().for_each(|v| *v = 0.0);
        let cuts = split_points(&Peaks { per_sec: 100.0, data: long }, 1800.0, 786.0);
        assert!(cuts.len() >= 3);
        assert!(cuts.iter().all(|(a, b)| b - a <= 786.0 + 1e-6));
        assert!((cuts[0].1 - 701.0).abs() < 1.0, "first cut lands in the vocal gap: {cuts:?}");
        assert!(cuts.windows(2).all(|w| (w[0].1 - w[1].0).abs() < 1e-9), "pieces are contiguous");
    }

    #[test]
    fn segments_become_clean_lines() {
        let seg = |s: f64, e: f64, t: &str, nsp: f64| Segment { start: s, end: e, text: t.into(), no_speech_prob: nsp, ..Default::default() };
        let lines = segments_to_lines(&[
            seg(0.0, 5.0, " [Music]", 0.1),
            seg(5.0, 8.0, " Amazing grace, how sweet the sound", 0.01),
            seg(8.0, 9.0, " Thank you.", 0.9),
            seg(9.0, 17.0, " That saved a wretch like me, I once was lost but now am found, was blind but now I see", 0.02),
        ]);
        assert_eq!(lines[0].text, "Amazing grace, how sweet the sound");
        assert!(lines.len() >= 3, "long segment is split: {lines:?}");
        assert!(lines.iter().all(|l| l.text.chars().count() <= 48));
        assert!((lines.last().unwrap().end - 17.0).abs() < 1e-9);
        assert!(lines.windows(2).all(|w| w[0].end <= w[1].start + 1e-9));
    }

    #[test]
    fn multipart_body_shape() {
        let (b, body) = multipart(&[("model", "whisper-1")], b"RIFFDATA");
        let s = String::from_utf8_lossy(&body);
        assert!(s.starts_with(&format!("--{b}\r\n")));
        assert!(s.contains("name=\"model\"\r\n\r\nwhisper-1\r\n"));
        assert!(s.contains("filename=\"audio.wav\""));
        assert!(s.ends_with(&format!("--{b}--\r\n")));
    }

    /// End-to-end against a server: `STAGEWEAVE_AI_BASE_URL=… STAGEWEAVE_AI_KEY=… cargo test live_ -- --ignored`
    #[test]
    #[ignore]
    fn live_transcribe() {
        let key = std::env::var("STAGEWEAVE_AI_KEY").unwrap_or_default();
        let buf = AudioBuffer { sample_rate: 44_100, data: vec![0.1; 44_100 * 2 * 3] };
        let wav = encode_wav_16k_mono(&buf, 0.0, 3.0);
        let r = transcribe_chunk(Provider::Groq, &key, &Provider::Groq.models()[0], "en", "Song lyrics.", true, &wav).unwrap();
        println!("words={:?}", r.words);
        let lines = segments_to_lines(&r.segments);
        println!("text={:?} lines={lines:?}", r.text);
        assert!(!lines.is_empty());
        assert!(test_key(Provider::Groq, &key).is_ok());
        assert!(test_key(Provider::Groq, "bad").unwrap_err().contains("rejected"));
    }

    #[test]
    fn keys_are_saved_privately_and_env_is_a_fallback() {
        let dir = std::env::temp_dir().join(format!("stageweave-secrets-{}", std::process::id()));
        let path = dir.join("secrets.json");
        let store = Arc::new(Store::in_memory().unwrap());
        let ai = AiSettings::with_path(store.clone(), path.clone());
        ai.set_key(Provider::Groq, "  gsk_test  ").unwrap();
        assert_eq!(ai.key_with_source(Provider::Groq), Some(("gsk_test".into(), KeySource::Saved)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Survives a restart.
        let again = AiSettings::with_path(store, path.clone());
        assert_eq!(again.key(Provider::Groq).as_deref(), Some("gsk_test"));
        again.remove_key(Provider::Groq);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("gsk_test"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lyrics_prompt_is_title_plus_flattened_start() {
        let p = lyrics_prompt("Way Maker", "You are here\n\nmoving in our midst");
        assert_eq!(p, "Way Maker. You are here moving in our midst");
        assert!(lyrics_prompt("", &"word ".repeat(500)).chars().count() <= 800);
    }

    fn seg(s: f64, e: f64, t: &str) -> Segment {
        Segment { start: s, end: e, text: t.into(), ..Default::default() }
    }

    #[test]
    fn quality_filter_drops_hallucinations() {
        let r = ApiResponse {
            text: String::new(),
            language: Some("swahili".into()),
            segments: vec![
                seg(0.0, 3.0, " Ana mipango mema"),
                Segment { no_speech_prob: 0.9, ..seg(3.0, 4.0, " Thank you.") },
                seg(4.0, 5.0, " Thank you."),
                Segment { compression_ratio: 3.1, ..seg(5.0, 8.0, " la la la la la la la la") },
                seg(8.0, 8.5, " one two three four five six seven eight nine"),
                seg(9.0, 11.0, " Yesu ni Bwana"),
                seg(11.0, 13.0, " Yesu ni Bwana"),
                seg(13.0, 15.0, " Yesu ni Bwana"),
            ],
            words: vec![
                ApiWord { word: "Ana".into(), start: 0.1, end: 0.5 },
                ApiWord { word: "Thank".into(), start: 4.1, end: 4.4 },
            ],
        };
        let c = clean_response(r);
        let texts: Vec<&str> = c.segments.iter().map(|s| s.text.trim()).collect();
        assert_eq!(texts, ["Ana mipango mema", "Yesu ni Bwana"], "filler, loops, too-fast and low-confidence lines dropped");
        assert_eq!(c.words.len(), 1, "words inside dropped segments go too");
    }

    #[test]
    fn language_names_map_to_codes() {
        assert_eq!(language_code("swahili"), Some("sw"));
        assert_eq!(language_code("Swahili"), Some("sw"));
        assert_eq!(language_code("sw"), Some("sw"));
        assert_eq!(language_code("klingon"), None);
        assert_eq!(language_label("sw"), "Swahili");
    }

    #[test]
    fn words_become_one_line_per_phrase() {
        use crate::align::Word;
        let w = |t: &str, s: f64| Word { text: format!(" {t}"), start: s, end: s + 0.3 };
        let phrases = [(10.0, 12.0), (14.0, 16.5), (20.0, 21.0)];
        let words = vec![w("Ana", 10.1), w("mipango", 10.6), w("mema", 11.2), w("anakupenda", 14.2), w("sana", 15.0)];
        let lines = lines_from_phrases(&phrases, &words, &[]);
        assert_eq!(lines.len(), 2, "silent third phrase skipped");
        assert_eq!(lines[0].text, "Ana mipango mema");
        assert_eq!((lines[0].start, lines[0].end), (10.0, 12.0), "timed to the phrase");
        assert_eq!(lines[1].text, "anakupenda sana");
    }

    /// Piece path against a server: `STAGEWEAVE_AI_BASE_URL=… STAGEWEAVE_AI_KEY=… cargo test live_pieces -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_pieces() {
        use crate::chunking::Piece;
        let key = std::env::var("STAGEWEAVE_AI_KEY").unwrap_or_default();
        let audio = Arc::new(AudioBuffer { sample_rate: 44_100, data: vec![0.1; 44_100 * 2 * 60] });
        let piece = |a: f64, b: f64| Piece { start: a, end: b, phrases: vec![(a + 0.3, b - 0.3)] };
        // Longest piece (20-32) goes first for language detection.
        let pieces = vec![piece(2.0, 8.0), piece(20.0, 32.0), piece(40.0, 46.0), piece(50.0, 55.0)];
        let req = TranscribeRequest {
            mode: JobMode::Transcribe,
            service: Some(Service { provider: Provider::Groq, key: key.clone(), model: &Provider::Groq.models()[0] }),
            language: String::new(),
            prompt: "Test. Song lyrics.".into(),
            audio,
            vocals: None,
            pieces: Some(pieces.clone()),
        };
        let svc = req.service.as_ref().unwrap();
        let (out, note) = run_pieces(svc, &req, &pieces, &AtomicBool::new(false), &|s| println!("stage: {s}")).unwrap();
        println!("note={note}\nwords={:?}", out.words.iter().map(|w| (w.text.as_str(), w.start)).collect::<Vec<_>>());
        assert!(note.contains("language: Swahili"), "{note}");
        // Word times shifted into song time (server answers 0.2 s into each piece).
        assert!(out.words.iter().any(|w| (w.start - 20.2).abs() < 1e-6));
        assert!(out.words.iter().any(|w| (w.start - 50.2).abs() < 1e-6));
    }

    #[test]
    fn api_errors_are_readable() {
        assert!(api_error(401, r#"{"error":{"message":"Incorrect API key"}}"#).contains("Incorrect API key"));
        assert!(api_error(429, "slow down").starts_with("Rate limited"));
    }
}
