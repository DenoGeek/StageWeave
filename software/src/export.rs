//! Export the lyric video as an MP4 with audio.
//!
//! Frames are drawn by the same renderer as the monitor (`lyrics_view::paint`)
//! into an offscreen GPU texture, read back and piped to ffmpeg. An image or
//! video background is composited by ffmpeg underneath the (premultiplied,
//! transparent) lyric frames, with the video looped for the whole song.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;

use crate::backgrounds::cover_filter;
use crate::engine::python_env::background_command;
use crate::model::{AudioBuffer, Lyrics};
use crate::ui::lyrics_view::{self, Backdrop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportAudio {
    /// The imported song file, untouched.
    Original,
    /// The stems as currently mixed (mutes, solos, faders, pans).
    Mix,
}

/// Export dialog state.
pub struct ExportPanel {
    pub open: bool,
    pub height: u32,
    pub fps: u32,
    pub audio: ExportAudio,
    pub job: Option<ExportJob>,
    pub result: Option<Result<PathBuf, String>>,
}

impl Default for ExportPanel {
    fn default() -> Self {
        Self { open: false, height: 1080, fps: 30, audio: ExportAudio::Original, job: None, result: None }
    }
}

/// Stems with their stereo gains, the master gain, and sections that switch
/// to their own gains (`start`, `end`, one gain pair per stem).
pub struct Mixdown {
    pub tracks: Vec<(Arc<AudioBuffer>, [f32; 2])>,
    pub master: f32,
    pub sections: Vec<(f64, f64, Vec<[f32; 2]>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgKind {
    Image,
    Video,
}

/// Everything an export needs, captured on the UI thread.
pub struct ExportSpec {
    pub lyrics: Lyrics,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio: PathBuf,
    /// Stems + stereo gains and master gain: written to `audio` first (and deleted afterwards).
    pub mix: Option<Mixdown>,
    pub background: Option<(PathBuf, BgKind)>,
    pub ffmpeg: PathBuf,
    pub dest: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub fraction: f32,
    pub stage: String,
}

pub struct ExportJob {
    pub dest: PathBuf,
    pub progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    done: Arc<Mutex<Option<Result<PathBuf, String>>>>,
}

impl ExportJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    /// The outcome, once finished.
    pub fn poll(&self) -> Option<Result<PathBuf, String>> {
        self.done.lock().unwrap().take()
    }
}

pub fn start(spec: ExportSpec, ctx: egui::Context) -> ExportJob {
    let progress = Arc::new(Mutex::new(Progress { fraction: 0.0, stage: "Starting…".into() }));
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(Mutex::new(None));
    let job = ExportJob { dest: spec.dest.clone(), progress: progress.clone(), cancel: cancel.clone(), done: done.clone() };
    std::thread::Builder::new()
        .name("lyric-export".into())
        .spawn(move || {
            let report = |fraction: f32, stage: &str| {
                *progress.lock().unwrap() = Progress { fraction, stage: stage.to_owned() };
                ctx.request_repaint();
            };
            let r = run(&spec, &cancel, &report);
            if spec.mix.is_some() {
                let _ = std::fs::remove_file(&spec.audio);
            }
            *done.lock().unwrap() = Some(r.map(|()| spec.dest.clone()));
            ctx.request_repaint();
        })
        .expect("spawn export thread");
    job
}

/// ffmpeg arguments: inputs are [background], raw frames on stdin, audio.
pub fn ffmpeg_args(spec: &ExportSpec, out: &Path) -> Vec<String> {
    let s = |v: &str| v.to_owned();
    let (w, h, fps) = (spec.width, spec.height, spec.fps);
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y"].map(s).to_vec();
    if let Some((path, kind)) = &spec.background {
        match kind {
            BgKind::Video => a.extend(["-stream_loop", "-1"].map(s)),
            BgKind::Image => a.extend(["-loop", "1", "-framerate", &fps.to_string()].map(s)),
        }
        a.extend([s("-i"), path.to_string_lossy().into_owned()]);
    }
    a.extend(["-f", "rawvideo", "-pix_fmt", "rgba", "-s", &format!("{w}x{h}"), "-framerate", &fps.to_string(), "-i", "-"].map(s));
    a.extend([s("-i"), spec.audio.to_string_lossy().into_owned()]);
    let audio_input = if spec.background.is_some() { 2 } else { 1 };
    if spec.background.is_some() {
        let filter = format!(
            "[0:v]{},fps={fps},format=rgba[bg];[bg][1:v]overlay=alpha=premultiplied:shortest=1:format=auto,format=yuv420p[v]",
            cover_filter(w, h)
        );
        a.extend([s("-filter_complex"), filter, s("-map"), s("[v]")]);
    } else {
        a.extend(["-map", "0:v", "-pix_fmt", "yuv420p"].map(s));
    }
    a.extend([s("-map"), format!("{audio_input}:a:0")]);
    a.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-r", &fps.to_string()].map(s));
    a.extend(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", "-t", &format!("{:.3}", spec.duration), "-f", "mp4"].map(s));
    a.push(out.to_string_lossy().into_owned());
    a
}

fn run(spec: &ExportSpec, cancel: &AtomicBool, report: &dyn Fn(f32, &str)) -> Result<(), String> {
    if let Some(Mixdown { tracks, master, sections }) = &spec.mix {
        report(0.0, "Mixing the stems…");
        write_mixdown(tracks, *master, sections, &spec.audio)?;
    }
    report(0.0, "Starting the renderer…");
    let mut renderer = FrameRenderer::new(spec.width, spec.height)?;
    let part = crate::download::part_path(&spec.dest);
    let mut child = background_command(&spec.ffmpeg)
        .args(ffmpeg_args(spec, &part))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Can't run ffmpeg: {e}"))?;
    let mut stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdin = child.stdin.take().expect("piped");
    let total = (spec.duration * spec.fps as f64).ceil().max(1.0) as usize;
    let backdrop = if spec.background.is_some() { Backdrop::Transparent } else { Backdrop::Theme };
    let mut result = Ok(());
    for i in 0..total {
        if cancel.load(Ordering::Relaxed) {
            result = Err("Export cancelled".to_owned());
            break;
        }
        let t = i as f64 / spec.fps as f64;
        let frame = renderer.render(|painter, rect| lyrics_view::paint(painter, rect, &spec.lyrics, t, spec.duration, t, backdrop))?;
        if stdin.write_all(&frame).is_err() {
            result = Err("ffmpeg stopped".to_owned());
            break;
        }
        if i % 15 == 0 {
            report(i as f32 / total as f32, &format!("Rendering {} / {}", timecode(t), timecode(spec.duration)));
        }
    }
    drop(stdin);
    if result.is_err() {
        let _ = child.kill();
    } else {
        report(1.0, "Finishing…");
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err_text = errors.join().unwrap_or_default();
    let result = result.and_then(|()| {
        if status.success() {
            std::fs::rename(&part, &spec.dest).map_err(|e| format!("Can't save {}: {e}", spec.dest.display()))
        } else {
            let last = err_text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
            Err(format!("ffmpeg failed: {last}"))
        }
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn timecode(t: f64) -> String {
    let t = t.max(0.0) as u64;
    format!("{}:{:02}", t / 60, t % 60)
}

// --------------------------------------------------------------------------- audio

/// Mix stems with per-track stereo gains into a 16-bit WAV.
pub fn write_mixdown(
    tracks: &[(Arc<AudioBuffer>, [f32; 2])],
    master: f32,
    sections: &[(f64, f64, Vec<[f32; 2]>)],
    dest: &Path,
) -> Result<(), String> {
    let rate = tracks.first().map_or(44_100, |(b, _)| b.sample_rate);
    let frames = tracks.iter().map(|(b, _)| b.frames()).max().unwrap_or(0);
    let mut f = std::io::BufWriter::new(std::fs::File::create(dest).map_err(|e| e.to_string())?);
    let data_len = (frames * 4) as u32;
    let mut header = Vec::with_capacity(44);
    header.extend(b"RIFF");
    header.extend((36 + data_len).to_le_bytes());
    header.extend(b"WAVEfmt ");
    header.extend(16u32.to_le_bytes());
    header.extend(1u16.to_le_bytes()); // PCM
    header.extend(2u16.to_le_bytes());
    header.extend(rate.to_le_bytes());
    header.extend((rate * 4).to_le_bytes());
    header.extend(4u16.to_le_bytes());
    header.extend(16u16.to_le_bytes());
    header.extend(b"data");
    header.extend(data_len.to_le_bytes());
    f.write_all(&header).map_err(|e| e.to_string())?;
    let mut chunk = Vec::with_capacity(64 * 1024);
    for i in 0..frames {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        let t = i as f64 / rate as f64;
        let section = sections.iter().find(|(a, b, _)| t >= *a && t < *b);
        for (k, (buf, gain)) in tracks.iter().enumerate() {
            let [gl, gr] = section.and_then(|(_, _, g)| g.get(k).copied()).unwrap_or(*gain);
            if let Some(s) = buf.data.get(i * 2..i * 2 + 2) {
                l += s[0] * gl;
                r += s[1] * gr;
            }
        }
        for v in [l * master, r * master] {
            chunk.extend(((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        if chunk.len() >= 64 * 1024 {
            f.write_all(&chunk).map_err(|e| e.to_string())?;
            chunk.clear();
        }
    }
    f.write_all(&chunk).map_err(|e| e.to_string())?;
    f.flush().map_err(|e| e.to_string())
}

// --------------------------------------------------------------------------- offscreen rendering

/// Draws egui shapes into an offscreen texture and reads the pixels back.
pub struct FrameRenderer {
    ctx: egui::Context,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    texture: wgpu::Texture,
    buffer: wgpu::Buffer,
    size: [u32; 2],
    padded_row: u32,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl FrameRenderer {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        let setup = egui_wgpu::WgpuSetup::CreateNew(egui_wgpu::WgpuSetupCreateNew::without_display_handle());
        let instance = pollster::block_on(setup.new_instance());
        let config = egui_wgpu::WgpuConfiguration { wgpu_setup: setup, ..Default::default() };
        let state = pollster::block_on(egui_wgpu::RenderState::create(&config, &instance, None, egui_wgpu::RendererOptions::default()))
            .map_err(|e| format!("Can't start the GPU renderer: {e}"))?;
        let device = state.device.clone();
        let queue = state.queue.clone();
        let renderer = egui_wgpu::Renderer::new(&device, FORMAT, egui_wgpu::RendererOptions::default());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lyric_export"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lyric_export_readback"),
            size: (padded_row * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);
        Ok(Self { ctx, device, queue, renderer, texture, buffer, size: [width, height], padded_row })
    }

    /// Render one frame; returns tightly packed RGBA (premultiplied alpha).
    pub fn render(&mut self, draw: impl Fn(&egui::Painter, egui::Rect)) -> Result<Vec<u8>, String> {
        let [w, h] = self.size;
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32));
        let input = egui::RawInput { screen_rect: Some(rect), ..Default::default() };
        let mut out = self.ctx.run_ui(input, |ui| draw(&ui.ctx().layer_painter(egui::LayerId::background()), rect));
        let mut textures = std::mem::take(&mut out.textures_delta);
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [w, h], pixels_per_point: out.pixels_per_point };
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("lyric_export") });
        let user = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, &prims, &screen);
        let view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("lyric_export"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        depth_slice: None,
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &prims, &screen);
        }
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(self.padded_row), rows_per_image: None },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(user.into_iter().chain(std::iter::once(encoder.finish())));
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        textures.clear();

        let slice = self.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| drop(tx.send(r)));
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) })
            .map_err(|e| format!("GPU error: {e}"))?;
        rx.recv().map_err(|e| e.to_string())?.map_err(|e| format!("GPU readback failed: {e}"))?;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| format!("GPU readback failed: {e}"))?;
            for row in data.chunks(self.padded_row as usize).take(h as usize) {
                pixels.extend_from_slice(&row[..(w * 4) as usize]);
            }
        }
        self.buffer.unmap();
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LyricLine;

    fn spec(bg: Option<(PathBuf, BgKind)>) -> ExportSpec {
        ExportSpec {
            lyrics: Lyrics::default(),
            duration: 12.5,
            width: 1280,
            height: 720,
            fps: 30,
            audio: "song.mp3".into(),
            mix: None,
            background: bg,
            ffmpeg: "ffmpeg".into(),
            dest: "out.mp4".into(),
        }
    }

    #[test]
    fn args_loop_the_background_and_map_audio() {
        let a = ffmpeg_args(&spec(Some(("clouds.mp4".into(), BgKind::Video))), Path::new("out.part"));
        let j = a.join(" ");
        assert!(j.contains("-stream_loop -1 -i clouds.mp4"), "{j}");
        assert!(j.contains("overlay=alpha=premultiplied"), "{j}");
        assert!(j.contains("-map 2:a:0") && j.contains("-t 12.500") && j.ends_with("-f mp4 out.part"), "{j}");

        let j = ffmpeg_args(&spec(Some(("sky.jpg".into(), BgKind::Image))), Path::new("o")).join(" ");
        assert!(j.contains("-loop 1 -framerate 30 -i sky.jpg"), "{j}");

        let j = ffmpeg_args(&spec(None), Path::new("o")).join(" ");
        assert!(j.contains("-map 0:v") && j.contains("-map 1:a:0") && !j.contains("overlay"), "{j}");
    }

    #[test]
    fn mixdown_applies_gains() {
        let buf = |v: f32| Arc::new(AudioBuffer { sample_rate: 8000, data: vec![v; 16] });
        let dest = std::env::temp_dir().join(format!("mk-mix-{}.wav", std::process::id()));
        write_mixdown(&[(buf(0.5), [1.0, 0.0]), (buf(0.25), [1.0, 1.0])], 1.0, &[], &dest).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        let _ = std::fs::remove_file(&dest);
        assert_eq!(bytes.len(), 44 + 8 * 4);
        let l = i16::from_le_bytes([bytes[44], bytes[45]]);
        let r = i16::from_le_bytes([bytes[46], bytes[47]]);
        assert_eq!(l, (0.75 * 32767.0) as i16);
        assert_eq!(r, (0.25 * 32767.0) as i16);
    }

    #[test]
    fn mixdown_follows_sections() {
        // 8 frames at 8 Hz; the second stem is muted from 0.5 s.
        let buf = |v: f32| Arc::new(AudioBuffer { sample_rate: 8, data: vec![v; 16] });
        let dest = std::env::temp_dir().join(format!("mk-mix-sec-{}.wav", std::process::id()));
        let sections = vec![(0.5, 2.0, vec![[1.0, 1.0], [0.0, 0.0]])];
        write_mixdown(&[(buf(0.5), [1.0, 1.0]), (buf(0.25), [1.0, 1.0])], 1.0, &sections, &dest).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        let _ = std::fs::remove_file(&dest);
        let left = |frame: usize| i16::from_le_bytes([bytes[44 + frame * 4], bytes[45 + frame * 4]]);
        assert_eq!(left(3), (0.75 * 32767.0) as i16, "before the section");
        assert_eq!(left(4), (0.5 * 32767.0) as i16, "inside the section");
    }

    /// Needs a GPU: `cargo test renders_a_frame -- --ignored`
    #[test]
    #[ignore]
    fn renders_a_frame() {
        let lyrics = Lyrics { lines: vec![LyricLine::new("Amazing grace", 0.0, 4.0)], ..Default::default() };
        let mut r = FrameRenderer::new(320, 180).unwrap();
        let px = r.render(|p, rect| lyrics_view::paint(p, rect, &lyrics, 1.0, 10.0, 1.0, Backdrop::Theme)).unwrap();
        assert_eq!(px.len(), 320 * 180 * 4);
        assert!(px.chunks(4).all(|p| p[3] == 255), "theme backdrop is opaque");
        let bright = px.chunks(4).filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200).count();
        assert!(bright > 50, "text was drawn ({bright} bright px)");
        let px = r.render(|p, rect| lyrics_view::paint(p, rect, &lyrics, 1.0, 10.0, 1.0, Backdrop::Transparent)).unwrap();
        assert!(px.chunks(4).any(|p| p[3] < 255), "transparent backdrop lets the background through");
    }

    /// Full pipeline with a real ffmpeg and the Clouds starter:
    /// `STAGEWEAVE_EXPORT_OUT=/tmp/x cargo test export_end_to_end -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn export_end_to_end() {
        let out_dir = PathBuf::from(std::env::var("STAGEWEAVE_EXPORT_OUT").expect("set STAGEWEAVE_EXPORT_OUT"));
        std::fs::create_dir_all(&out_dir).unwrap();
        let clouds = out_dir.join("Clouds.mp4");
        std::fs::write(&clouds, crate::backgrounds::STARTERS[0].data).unwrap();
        // 6 s of a 440 Hz tone.
        let tone = Arc::new(AudioBuffer {
            sample_rate: 44_100,
            data: (0..44_100 * 6).flat_map(|i| {
                let v = (i as f32 / 44_100.0 * 440.0 * std::f32::consts::TAU).sin() * 0.3;
                [v, v]
            }).collect(),
        });
        let lyrics = Lyrics {
            title: "Export Test".into(),
            lines: vec![
            LyricLine::new("Ana mipango mema (He has good plans)", 1.0, 3.5),
                LyricLine::new("Amazing grace", 3.5, 6.0),
            ],
            ..Default::default()
        };
        for (name, bg) in [("video", Some((clouds.clone(), BgKind::Video))), ("theme", None)] {
            let dest = out_dir.join(format!("export-{name}.mp4"));
            let spec = ExportSpec {
                lyrics: lyrics.clone(),
                duration: 6.0,
                width: 1280,
                height: 720,
                fps: 25,
                audio: out_dir.join(format!("mix-{name}.wav")),
                mix: Some(Mixdown { tracks: vec![(tone.clone(), [1.0, 1.0])], master: 1.0, sections: Vec::new() }),
                background: bg,
                ffmpeg: "ffmpeg".into(),
                dest: dest.clone(),
            };
            let t0 = std::time::Instant::now();
            run(&spec, &AtomicBool::new(false), &|_, _| {}).unwrap();
            println!("{name}: {:.1}s to export", t0.elapsed().as_secs_f32());
            let probe = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height:format=duration", "-of", "compact"])
                .arg(&dest)
                .output()
                .unwrap();
            let info = String::from_utf8_lossy(&probe.stdout);
            println!("{info}");
            assert!(info.contains("codec_name=h264") && info.contains("width=1280") && info.contains("codec_type=audio"));
            // A frame mid-line for eyeballing.
            let _ = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-ss", "2.0", "-i"])
                .arg(&dest)
                .args(["-frames:v", "1"])
                .arg(out_dir.join(format!("frame-{name}.png")))
                .status();
        }
    }
}
