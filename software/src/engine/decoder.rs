//! Pure-Rust decoding via symphonia.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::model::{AudioBuffer, AudioInfo, SourceAudio};

use super::{AudioDecoder, PEAKS_PER_SEC, SUPPORTED_EXTENSIONS, is_supported};

pub struct SymphoniaDecoder;

/// Decode any supported file to interleaved stereo f32 at its native rate.
/// Returns the buffer and the original channel count.
pub fn decode_file(path: &Path) -> Result<(AudioBuffer, u16), String> {
    let file = File::open(path).map_err(|e| format!("Can't open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Unrecognised audio format: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No audio track found")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Unsupported codec: {e}"))?;

    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = track.codec_params.channels.map_or(0, |c| c.count());
    let mut data: Vec<f32> = Vec::new();
    if let Some(frames) = track.codec_params.n_frames {
        data.reserve(frames as usize * 2);
    }
    let mut scratch: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("Read error: {e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Corrupt frames are skipped, as players usually do.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Decode error: {e}")),
        };
        let spec = *decoded.spec();
        sample_rate = spec.rate;
        channels = spec.channels.count();
        let buf = match &mut scratch {
            Some(b) if b.capacity() >= decoded.capacity() * channels => b,
            _ => scratch.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
        };
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        match channels {
            1 => samples.iter().for_each(|&s| data.extend_from_slice(&[s, s])),
            2 => data.extend_from_slice(samples),
            n => {
                // Keep the front pair of multichannel files.
                for frame in samples.chunks(n) {
                    data.extend_from_slice(&frame[..2]);
                }
            }
        }
    }

    if sample_rate == 0 || data.is_empty() {
        return Err("File contains no audio".into());
    }
    Ok((AudioBuffer { sample_rate, data }, channels as u16))
}

impl AudioDecoder for SymphoniaDecoder {
    fn load(&self, path: &Path) -> Result<SourceAudio, String> {
        if !is_supported(path) {
            return Err(format!("Unsupported file type. Try one of: {}", SUPPORTED_EXTENSIONS.join(", ")));
        }
        let (audio, channels) = decode_file(path)?;
        let format = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_uppercase();
        Ok(SourceAudio {
            info: AudioInfo {
                file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                format,
                duration: audio.duration(),
                sample_rate: audio.sample_rate,
                channels,
            },
            peaks: audio.peaks(PEAKS_PER_SEC),
            audio: Some(Arc::new(audio)),
        })
    }
}
