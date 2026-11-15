//! Slide decks in a setlist: PowerPoint / Keynote / Impress files (turned
//! into a PDF by LibreOffice), PDFs and folders of images.
//!
//! Pages are rendered to PNGs with pdfium (a shared library downloaded once
//! into `<app data>/engine/pdfium/`). Videos and sounds embedded in a .pptx
//! are copied out and placed on their slide, with their audio converted to
//! WAV by ffmpeg so it plays on the normal transport.
//!
//! ```text
//! <project>/decks/<name>/
//!   deck.json                slides, in order, with their media
//!   slide-001.png …
//!   media/media1.mp4         embedded media
//!   media/media1.mp4.wav     its audio (silence if it has none)
//! ```

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use quick_xml::events::{BytesStart, Event};

use crate::engine::python_env::{EnginePaths, background_command};

pub const DECK_FILE: &str = "deck.json";
/// Width slides are rendered at.
const RENDER_W: u16 = 1920;

/// File types that become a deck through LibreOffice.
pub const OFFICE_EXTENSIONS: &[&str] = &["pptx", "ppt", "pps", "ppsx", "key", "odp"];
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

pub fn extension(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// A presentation (or PDF) that can be imported as one deck.
pub fn is_deck_file(p: &Path) -> bool {
    let e = extension(p);
    OFFICE_EXTENSIONS.contains(&e.as_str()) || e == "pdf"
}

pub fn is_image(p: &Path) -> bool {
    IMAGE_EXTENSIONS.contains(&extension(p).as_str())
}

// --------------------------------------------------------------------------- deck.json

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Deck {
    pub slides: Vec<Slide>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Slide {
    /// Image file, relative to the deck folder.
    pub image: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<SlideMedia>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    #[default]
    Video,
    Audio,
}

/// A video or sound placed on a slide.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlideMedia {
    /// Relative to the deck folder.
    pub file: String,
    pub kind: MediaKind,
    /// Where it sits on the slide: x, y, width, height as fractions of the slide.
    pub rect: [f32; 4],
    /// Starts by itself when the slide is shown (else on click / Space).
    #[serde(default)]
    pub autoplay: bool,
    #[serde(default, rename = "loop")]
    pub looping: bool,
    /// Audio as WAV (relative to the deck folder), made with ffmpeg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

impl Deck {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(dir.join(DECK_FILE)).map_err(|e| format!("Can't read the slides: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("Broken {DECK_FILE}: {e}"))
    }

    fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::write(dir.join(DECK_FILE), serde_json::to_string_pretty(self).unwrap_or_default()).map_err(|e| e.to_string())
    }
}

// --------------------------------------------------------------------------- tools

/// LibreOffice's command-line converter, if it's installed.
pub fn locate_soffice() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/Applications/LibreOffice.app/Contents/MacOS/soffice"]
    } else if cfg!(windows) {
        &[r"C:\Program Files\LibreOffice\program\soffice.exe", r"C:\Program Files (x86)\LibreOffice\program\soffice.exe"]
    } else {
        &["/usr/bin/soffice", "/usr/bin/libreoffice", "/usr/local/bin/soffice", "/snap/bin/libreoffice", "/opt/libreoffice/program/soffice"]
    };
    candidates.iter().map(PathBuf::from).find(|p| p.is_file()).or_else(|| {
        let works = background_command("soffice").arg("--version").output().is_ok_and(|o| o.status.success());
        works.then(|| PathBuf::from("soffice"))
    })
}

/// pdfium build matching the `pdfium_8125` bindings in Cargo.toml.
const PDFIUM_RELEASE: &str = "chromium/8125";

fn pdfium_asset() -> Option<(&'static str, &'static str)> {
    // (release asset, library path inside it)
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("pdfium-mac-arm64.tgz", "lib/libpdfium.dylib"),
        ("macos", "x86_64") => ("pdfium-mac-x64.tgz", "lib/libpdfium.dylib"),
        ("windows", "x86_64") => ("pdfium-win-x64.tgz", "bin/pdfium.dll"),
        ("windows", "aarch64") => ("pdfium-win-arm64.tgz", "bin/pdfium.dll"),
        ("linux", "x86_64") => ("pdfium-linux-x64.tgz", "lib/libpdfium.so"),
        ("linux", "aarch64") => ("pdfium-linux-arm64.tgz", "lib/libpdfium.so"),
        _ => return None,
    })
}

/// The pdfium library that renders slides; downloaded on first use.
pub struct PdfiumLib {
    dir: PathBuf,
}

impl PdfiumLib {
    pub fn new(paths: &EnginePaths) -> Self {
        Self { dir: paths.root.join("pdfium") }
    }

    fn lib_path(&self) -> Option<PathBuf> {
        let (_, inner) = pdfium_asset()?;
        Some(self.dir.join(Path::new(inner).file_name()?))
    }

    /// The library, downloading it first if needed (blocking).
    pub fn ensure(&self, progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
        let (asset, inner) = pdfium_asset().ok_or("Slides can't be rendered on this platform")?;
        let lib = self.lib_path().ok_or("Slides can't be rendered on this platform")?;
        if lib.is_file() {
            return Ok(lib);
        }
        let url = format!("https://github.com/bblanchon/pdfium-binaries/releases/download/{PDFIUM_RELEASE}/{asset}");
        let tgz = self.dir.join(asset);
        crate::download::fetch(&url, &tgz, &AtomicBool::new(false), &|done, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                progress(done as f32 / t as f32);
            }
        })?;
        let result = (|| {
            let gz = flate2::read::GzDecoder::new(std::fs::File::open(&tgz).map_err(|e| e.to_string())?);
            let mut tar = tar::Archive::new(gz);
            for entry in tar.entries().map_err(|e| e.to_string())? {
                let mut e = entry.map_err(|e| e.to_string())?;
                let path = e.path().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                if path.trim_start_matches("./") == inner {
                    let mut buf = Vec::new();
                    e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                    let tmp = crate::download::part_path(&lib);
                    std::fs::write(&tmp, buf).map_err(|e| e.to_string())?;
                    #[cfg(target_os = "macos")]
                    {
                        let _ = background_command("xattr").args(["-d", "com.apple.quarantine"]).arg(&tmp).output();
                    }
                    return std::fs::rename(&tmp, &lib).map_err(|e| e.to_string());
                }
            }
            Err("pdfium wasn't in the download".to_owned())
        })();
        let _ = std::fs::remove_file(&tgz);
        result.map(|()| lib)
    }
}

/// pdfium can only be loaded once per process.
fn pdfium(lib: &Path) -> Result<&'static pdfium_render::prelude::Pdfium, String> {
    static PDFIUM: OnceLock<Result<pdfium_render::prelude::Pdfium, String>> = OnceLock::new();
    PDFIUM
        .get_or_init(|| {
            let bindings = pdfium_render::prelude::Pdfium::bind_to_library(lib).map_err(|e| format!("Can't load pdfium: {e}"))?;
            Ok(pdfium_render::prelude::Pdfium::new(bindings))
        })
        .as_ref()
        .map_err(Clone::clone)
}

// --------------------------------------------------------------------------- import

/// Everything an import needs from the app.
pub struct DeckTools<'a> {
    pub pdfium: &'a PdfiumLib,
    /// For media audio; `None` keeps media silent.
    pub ffmpeg: Option<PathBuf>,
}

/// Build a deck in `out` (a new, empty folder) from `sources`: one
/// presentation or PDF, or several images (in the given order).
pub fn import(sources: &[PathBuf], out: &Path, tools: &DeckTools, report: &dyn Fn(f32, &str)) -> Result<Deck, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("Can't create {}: {e}", out.display()))?;
    let first = sources.first().ok_or("Nothing to import")?;
    let mut deck = Deck::default();
    if sources.iter().all(|p| is_image(p)) {
        for (i, src) in sources.iter().enumerate() {
            let name = format!("slide-{:03}.{}", i + 1, extension(src));
            std::fs::copy(src, out.join(&name)).map_err(|e| format!("Can't copy {}: {e}", src.display()))?;
            deck.slides.push(Slide { image: name, media: Vec::new() });
            report((i + 1) as f32 / sources.len() as f32, "Copying slides");
        }
    } else {
        let pdf = if extension(first) == "pdf" {
            first.clone()
        } else {
            report(0.02, "Converting with LibreOffice");
            to_pdf(first, out)?
        };
        report(0.3, "Rendering slides");
        let lib = tools.pdfium.ensure(&|f| report(0.3 + f * 0.1, "Downloading the slide renderer"))?;
        let images = render_pdf(&lib, &pdf, out, &|f| report(0.4 + f * 0.4, "Rendering slides"))?;
        if pdf.starts_with(out) {
            let _ = std::fs::remove_file(&pdf);
        }
        deck.slides = images.into_iter().map(|image| Slide { image, media: Vec::new() }).collect();
        if extension(first) == "pptx" || extension(first) == "ppsx" {
            report(0.8, "Copying videos and sounds");
            match pptx_media(first, out) {
                Ok(per_slide) => {
                    for (slide, media) in deck.slides.iter_mut().zip(per_slide) {
                        slide.media = media;
                    }
                }
                Err(e) => eprintln!("slides: no media from {}: {e}", first.display()),
            }
        }
    }
    if deck.slides.is_empty() {
        return Err("No slides found".into());
    }
    if let Some(ff) = &tools.ffmpeg {
        let n = deck.slides.iter().map(|s| s.media.len()).sum::<usize>().max(1);
        for (k, m) in deck.slides.iter_mut().flat_map(|s| s.media.iter_mut()).enumerate() {
            report(0.85 + 0.15 * k as f32 / n as f32, "Preparing media audio");
            let wav = format!("{}.wav", m.file);
            if media_audio(ff, &out.join(&m.file), &out.join(&wav)).is_ok() {
                m.audio = Some(wav);
            }
        }
    }
    deck.save(out)?;
    report(1.0, "Done");
    Ok(deck)
}

/// A `file:///` URL for LibreOffice. Unescaped spaces (as in macOS's
/// "Application Support") make LibreOffice abort at startup, so escape everything
/// except unreserved characters, `/` and a Windows drive's `:`.
fn file_url(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:///");
    for b in p.trim_start_matches('/').bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Office document → PDF with LibreOffice (headless).
fn to_pdf(src: &Path, out: &Path) -> Result<PathBuf, String> {
    let soffice = locate_soffice().ok_or(
        "PowerPoint and Keynote files need LibreOffice (free, libreoffice.org). Or export the slides as PDF or images and add those.",
    )?;
    let tmp = out.join(".convert");
    let _ = std::fs::create_dir_all(&tmp);
    // A private profile, so a LibreOffice window that's already open doesn't block us.
    let profile = file_url(&tmp.join("profile"));
    let o = background_command(&soffice)
        .arg(format!("-env:UserInstallation={profile}"))
        .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
        .arg(&tmp)
        .arg(src)
        .output()
        .map_err(|e| format!("Couldn't run LibreOffice: {e}"))?;
    let pdf = tmp.join(src.with_extension("pdf").file_name().unwrap_or_default());
    if !pdf.is_file() {
        let err = String::from_utf8_lossy(&o.stderr);
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("LibreOffice couldn't convert the file. {}", err.lines().last().unwrap_or_default().trim()));
    }
    let dest = out.join("deck.pdf");
    std::fs::rename(&pdf, &dest).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(dest)
}

/// Render every page of `pdf` to `out/slide-NNN.png`. Returns the file names.
fn render_pdf(lib: &Path, pdf: &Path, out: &Path, report: &dyn Fn(f32)) -> Result<Vec<String>, String> {
    use pdfium_render::prelude::*;
    let pdfium = pdfium(lib)?;
    let doc = pdfium.load_pdf_from_file(pdf, None).map_err(|e| format!("Can't open the PDF: {e}"))?;
    let pages = doc.pages();
    let n = pages.len() as usize;
    let cfg = PdfRenderConfig::new().set_target_width(RENDER_W as i32).set_maximum_height(RENDER_W as i32 * 2);
    let mut names = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let bitmap = page.render_with_config(&cfg).map_err(|e| format!("Can't render slide {}: {e}", i + 1))?;
        let img = bitmap.as_image().map_err(|e| format!("Can't render slide {}: {e}", i + 1))?;
        let name = format!("slide-{:03}.png", i + 1);
        img.into_rgb8().save(out.join(&name)).map_err(|e| format!("Can't save slide {}: {e}", i + 1))?;
        names.push(name);
        report((i + 1) as f32 / n.max(1) as f32);
    }
    Ok(names)
}

/// Media audio as a 48 kHz stereo WAV; silence for a video without sound.
fn media_audio(ffmpeg: &Path, src: &Path, dest: &Path) -> Result<(), String> {
    let run = |args: &[&str]| {
        background_command(ffmpeg)
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .args(args)
            .arg(dest)
            .output()
            .is_ok_and(|o| o.status.success() && dest.is_file())
    };
    let s = src.to_string_lossy();
    if run(&["-i", &s, "-map", "0:a:0", "-vn", "-ac", "2", "-ar", "48000"]) {
        return Ok(());
    }
    let dur = probe_duration(ffmpeg, src).ok_or("Can't read the media's length")?;
    if run(&["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", &format!("{dur:.3}")]) {
        Ok(())
    } else {
        Err("Couldn't prepare the media's audio".into())
    }
}

/// Length of a media file, from ffmpeg's "Duration: 00:01:02.50" line.
fn probe_duration(ffmpeg: &Path, src: &Path) -> Option<f64> {
    let o = background_command(ffmpeg).arg("-hide_banner").arg("-i").arg(src).output().ok()?;
    let text = String::from_utf8_lossy(&o.stderr);
    let d = text.split("Duration: ").nth(1)?.split(',').next()?.trim();
    let mut parts = d.split(':').map(|p| p.parse::<f64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    Some(h * 3600.0 + m * 60.0 + s)
}

// --------------------------------------------------------------------------- pptx media

fn read_entry<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn attr(e: &BytesStart, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key || a.key.local_name().as_ref() == key).and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

/// A namespaced attribute (`r:id`), not the plain one (`id`).
fn ns_attr(e: &BytesStart, local: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.prefix().is_some() && a.key.local_name().as_ref() == local)
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

/// Relationship id → target path, from a `.rels` file. Internal targets
/// are resolved against `base` (the folder of the part they belong to).
fn rels(xml: &str, base: &str) -> HashMap<String, (String, bool)> {
    let mut out = HashMap::new();
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e) | Event::Start(e)) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    let external = attr(&e, "TargetMode").as_deref() == Some("External");
                    let path = if external { target } else { resolve(base, &target) };
                    out.insert(id, (path, external));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// "ppt/slides" + "../media/a.mp4" → "ppt/media/a.mp4"
fn resolve(base: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for p in target.split('/') {
        match p {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// One media shape found on a slide, before its file is copied.
#[derive(Debug, Default, PartialEq)]
struct MediaShape {
    shape_id: String,
    rel: String,
    kind: MediaKind,
    /// EMU: x, y, cx, cy
    xfrm: [i64; 4],
}

/// Media shapes on a slide and the timing that plays them:
/// (shapes, shape ids that start by themselves, shape ids that loop).
fn slide_media(xml: &str) -> (Vec<MediaShape>, Vec<String>, Vec<String>) {
    let mut shapes = Vec::new();
    let (mut auto, mut looping) = (Vec::new(), Vec::new());
    let mut r = quick_xml::Reader::from_str(xml);
    // Inside a <p:pic>: its id, media link, kind and position.
    let mut pic: Option<MediaShape> = None;
    let mut embed: Option<String> = None;
    let mut in_xfrm = false;
    // Timing: stack of <p:cTn> (presetClass, nodeType, repeatCount); inside <p:cmd>?
    let mut ctn: Vec<(String, String, String)> = Vec::new();
    let mut in_cmd = false;
    let mut in_media_node = false;
    // The current <p:cMediaNode> repeats forever.
    let mut node_loops = false;
    loop {
        let ev = r.read_event();
        let (e, empty) = match &ev {
            Ok(Event::Start(e)) => (Some(e.clone()), false),
            Ok(Event::Empty(e)) => (Some(e.clone()), true),
            Ok(Event::End(e)) => {
                match e.local_name().as_ref() {
                    "pic" => {
                        if let Some(mut p) = pic.take() {
                            // Prefer the embedded copy (p14:media) over a link.
                            if let Some(rel) = embed.take() {
                                p.rel = rel;
                            }
                            if !p.rel.is_empty() {
                                shapes.push(p);
                            }
                        }
                        embed = None;
                    }
                    "xfrm" => in_xfrm = false,
                    "cTn" => {
                        ctn.pop();
                    }
                    "cmd" => in_cmd = false,
                    "cMediaNode" => {
                        in_media_node = false;
                        node_loops = false;
                    }
                    _ => {}
                }
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => continue,
        };
        let Some(e) = e else { continue };
        match e.local_name().as_ref() {
            "pic" if !empty => pic = Some(MediaShape::default()),
            "cNvPr" => {
                if let Some(p) = pic.as_mut().filter(|p| p.shape_id.is_empty()) {
                    p.shape_id = attr(&e, "id").unwrap_or_default();
                }
            }
            "videoFile" | "audioFile" => {
                if let Some(p) = pic.as_mut() {
                    p.kind = if e.local_name().as_ref() == "videoFile" { MediaKind::Video } else { MediaKind::Audio };
                    p.rel = attr(&e, "link").unwrap_or_default();
                }
            }
            "media" if pic.is_some() => embed = attr(&e, "embed").filter(|r| !r.is_empty()),
            "xfrm" if pic.is_some() && !empty => in_xfrm = true,
            "off" if in_xfrm => {
                if let Some(p) = pic.as_mut() {
                    p.xfrm[0] = attr(&e, "x").and_then(|v| v.parse().ok()).unwrap_or(0);
                    p.xfrm[1] = attr(&e, "y").and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
            "ext" if in_xfrm => {
                if let Some(p) = pic.as_mut() {
                    p.xfrm[2] = attr(&e, "cx").and_then(|v| v.parse().ok()).unwrap_or(0);
                    p.xfrm[3] = attr(&e, "cy").and_then(|v| v.parse().ok()).unwrap_or(0);
                }
            }
            "cTn" => {
                let entry = (attr(&e, "presetClass").unwrap_or_default(), attr(&e, "nodeType").unwrap_or_default(), attr(&e, "repeatCount").unwrap_or_default());
                // <p:video><p:cMediaNode><p:cTn repeatCount="indefinite">: loops.
                node_loops |= in_media_node && entry.2 == "indefinite";
                if !empty {
                    ctn.push(entry);
                }
            }
            "cMediaNode" if !empty => in_media_node = true,
            "cmd" if !empty => in_cmd = attr(&e, "cmd").is_some_and(|c| c.starts_with("playFrom")),
            "spTgt" => {
                let Some(id) = attr(&e, "spid") else { continue };
                if in_media_node && node_loops {
                    looping.push(id.clone());
                }
                if in_cmd {
                    // The nearest media-call effect decides how it starts.
                    let node = ctn.iter().rev().find(|c| c.0 == "mediacall").map(|c| c.1.clone()).unwrap_or_default();
                    if node == "afterEffect" || node == "withEffect" {
                        auto.push(id);
                    }
                }
            }
            _ => {}
        }
    }
    (shapes, auto, looping)
}

/// The media on each shown slide of a .pptx (in show order, hidden slides
/// skipped, like the PDF export), copied into `out/media/`.
fn pptx_media(pptx: &Path, out: &Path) -> Result<Vec<Vec<SlideMedia>>, String> {
    let f = std::fs::File::open(pptx).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("Not a PowerPoint file: {e}"))?;
    let pres = read_entry(&mut zip, "ppt/presentation.xml").ok_or("No presentation.xml")?;
    let pres_rels = rels(&read_entry(&mut zip, "ppt/_rels/presentation.xml.rels").unwrap_or_default(), "ppt");
    // Slide size and order.
    let (mut cx, mut cy) = (12_192_000i64, 6_858_000i64);
    let mut order: Vec<String> = Vec::new();
    let mut r = quick_xml::Reader::from_str(&pres);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e) | Event::Start(e)) => match e.local_name().as_ref() {
                "sldSz" => {
                    cx = attr(&e, "cx").and_then(|v| v.parse().ok()).unwrap_or(cx);
                    cy = attr(&e, "cy").and_then(|v| v.parse().ok()).unwrap_or(cy);
                }
                "sldId" => {
                    if let Some((target, _)) = ns_attr(&e, "id").and_then(|rid| pres_rels.get(&rid)) {
                        order.push(target.clone());
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let media_dir = out.join("media");
    let mut copied: HashMap<String, String> = HashMap::new();
    let mut result = Vec::new();
    for part in order {
        let Some(xml) = read_entry(&mut zip, &part) else { continue };
        if hidden(&xml) {
            continue;
        }
        let (dir, name) = part.rsplit_once('/').unwrap_or(("", &part));
        let slide_rels = rels(&read_entry(&mut zip, &format!("{dir}/_rels/{name}.rels")).unwrap_or_default(), dir);
        let (shapes, auto, looping) = slide_media(&xml);
        let mut media = Vec::new();
        for s in shapes {
            let Some((target, external)) = slide_rels.get(&s.rel) else { continue };
            let file = if let Some(f) = copied.get(target) {
                f.clone()
            } else {
                let name = Path::new(target).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "media".into());
                let rel = format!("media/{name}");
                let dest = out.join(&rel);
                std::fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;
                let ok = if *external {
                    // Linked, not embedded: look next to the presentation.
                    let linked = target.strip_prefix("file:///").map(PathBuf::from).unwrap_or_else(|| pptx.parent().unwrap_or(Path::new(".")).join(target));
                    std::fs::copy(&linked, &dest).is_ok()
                } else {
                    match zip.by_name(target) {
                        Ok(mut f) => std::fs::File::create(&dest).and_then(|mut o| std::io::copy(&mut f, &mut o)).is_ok(),
                        Err(_) => false,
                    }
                };
                if !ok {
                    continue;
                }
                copied.insert(target.clone(), rel.clone());
                rel
            };
            let [x, y, w, h] = s.xfrm;
            let rect = [x as f32 / cx as f32, y as f32 / cy as f32, w as f32 / cx as f32, h as f32 / cy as f32];
            media.push(SlideMedia {
                file,
                kind: s.kind,
                rect,
                autoplay: auto.contains(&s.shape_id),
                looping: looping.contains(&s.shape_id),
                audio: None,
            });
        }
        result.push(media);
    }
    Ok(result)
}

/// `<p:sld show="0">`: hidden in the show (and left out of the PDF).
fn hidden(slide_xml: &str) -> bool {
    let mut r = quick_xml::Reader::from_str(slide_xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == "sld" => return attr(&e, "show").as_deref() == Some("0"),
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLIDE: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:a="a" xmlns:p="p" xmlns:r="r" xmlns:p14="p14">
 <p:cSld><p:spTree>
  <p:pic>
   <p:nvPicPr><p:cNvPr id="4" name="intro"/><p:cNvPicPr/>
    <p:nvPr><a:videoFile r:link="rId1"/><p:extLst><p:ext><p14:media r:embed="rId2"/></p:ext></p:extLst></p:nvPr>
   </p:nvPicPr>
   <p:spPr><a:xfrm><a:off x="1219200" y="685800"/><a:ext cx="6096000" cy="3429000"/></a:xfrm></p:spPr>
  </p:pic>
  <p:pic>
   <p:nvPicPr><p:cNvPr id="7" name="bell"/><p:nvPr><a:audioFile r:link="rId3"/></p:nvPr></p:nvPicPr>
   <p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></a:xfrm></p:spPr>
  </p:pic>
 </p:spTree></p:cSld>
 <p:timing><p:tnLst><p:par><p:cTn id="1" nodeType="tmRoot"><p:childTnLst>
  <p:par><p:cTn id="5" presetClass="mediacall" nodeType="afterEffect"><p:childTnLst>
   <p:cmd type="call" cmd="playFrom(0.0)"><p:cBhvr><p:cTn id="6"/><p:tgtEl><p:spTgt spid="4"/></p:tgtEl></p:cBhvr></p:cmd>
  </p:childTnLst></p:cTn></p:par>
  <p:par><p:cTn id="8" presetClass="mediacall" nodeType="clickEffect"><p:childTnLst>
   <p:cmd type="call" cmd="playFrom(0.0)"><p:cBhvr><p:cTn id="9"/><p:tgtEl><p:spTgt spid="7"/></p:tgtEl></p:cBhvr></p:cmd>
  </p:childTnLst></p:cTn></p:par>
  <p:video><p:cMediaNode vol="80000"><p:cTn id="10" repeatCount="indefinite" fill="hold"/><p:tgtEl><p:spTgt spid="4"/></p:tgtEl></p:cMediaNode></p:video>
 </p:childTnLst></p:cTn></p:par></p:tnLst></p:timing>
</p:sld>"#;

    #[test]
    fn media_shapes_and_their_timing_are_found() {
        let (shapes, auto, looping) = slide_media(SLIDE);
        assert_eq!(shapes.len(), 2);
        assert_eq!(shapes[0], MediaShape { shape_id: "4".into(), rel: "rId2".into(), kind: MediaKind::Video, xfrm: [1219200, 685800, 6096000, 3429000] });
        assert_eq!((shapes[1].kind, shapes[1].rel.as_str()), (MediaKind::Audio, "rId3"));
        assert_eq!(auto, ["4"], "the video plays by itself, the sound on click");
        assert_eq!(looping, ["4"]);
        assert!(!hidden(SLIDE));
        assert!(hidden(r#"<p:sld xmlns:p="p" show="0"/>"#));
    }

    #[test]
    fn relationship_targets_resolve() {
        let xml = r#"<Relationships><Relationship Id="rId2" Target="../media/media1.mp4" Type="x"/><Relationship Id="rId1" Target="file:///C:/v.mp4" TargetMode="External"/></Relationships>"#;
        let r = rels(xml, "ppt/slides");
        assert_eq!(r["rId2"], ("ppt/media/media1.mp4".to_owned(), false));
        assert!(r["rId1"].1);
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
    }

    /// Writes a small .pptx with an embedded video and reads it back.
    #[test]
    fn pptx_media_is_copied_and_placed() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("sw-pptx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pptx");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opt = zip::write::SimpleFileOptions::default();
        let mut add = |name: &str, data: &[u8]| {
            z.start_file(name, opt).unwrap();
            z.write_all(data).unwrap();
        };
        add("ppt/presentation.xml", br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="257" r:id="rId8"/><p:sldId id="256" r:id="rId7"/><p:sldId id="258" r:id="rId9"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#);
        add("ppt/_rels/presentation.xml.rels", br#"<Relationships><Relationship Id="rId7" Target="slides/slide1.xml"/><Relationship Id="rId8" Target="slides/slide2.xml"/><Relationship Id="rId9" Target="slides/slide3.xml"/></Relationships>"#);
        add("ppt/slides/slide1.xml", SLIDE.as_bytes());
        add("ppt/slides/slide2.xml", br#"<p:sld xmlns:p="p"/>"#);
        add("ppt/slides/slide3.xml", br#"<p:sld xmlns:p="p" show="0"/>"#);
        add("ppt/slides/_rels/slide1.xml.rels", br#"<Relationships><Relationship Id="rId2" Target="../media/media1.mp4"/><Relationship Id="rId3" Target="../media/media2.m4a"/></Relationships>"#);
        add("ppt/media/media1.mp4", b"video bytes");
        add("ppt/media/media2.m4a", b"audio bytes");
        z.finish().unwrap();

        let out = dir.join("deck");
        let media = pptx_media(&path, &out).unwrap();
        // slide2 is first in the show, slide3 is hidden.
        assert_eq!(media.len(), 2);
        assert!(media[0].is_empty());
        let v = &media[1][0];
        assert_eq!((v.file.as_str(), v.kind, v.autoplay, v.looping), ("media/media1.mp4", MediaKind::Video, true, true));
        assert!((v.rect[0] - 0.1).abs() < 1e-3 && (v.rect[2] - 0.5).abs() < 1e-3);
        assert_eq!(std::fs::read(out.join("media/media2.m4a")).unwrap(), b"audio bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn libreoffice_profile_urls_are_escaped() {
        assert_eq!(
            file_url(Path::new("/Users/me/Library/Application Support/StageWeave/x/.convert/profile")),
            "file:///Users/me/Library/Application%20Support/StageWeave/x/.convert/profile"
        );
        assert_eq!(file_url(Path::new(r"C:\Users\Me Too\p")), "file:///C:/Users/Me%20Too/p");
        assert_eq!(file_url(Path::new("/a/é#%")), "file:///a/%C3%A9%23%25");
    }

    /// LibreOffice used to abort when the app-data path had a space (macOS
    /// "Application Support"). Runs when LibreOffice is installed.
    #[test]
    fn libreoffice_converts_in_a_folder_with_spaces() {
        if locate_soffice().is_none() {
            eprintln!("LibreOffice not installed; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("sw deck spaces {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("notes.txt");
        std::fs::write(&src, "Hello").unwrap();
        let pdf = to_pdf(&src, &dir).unwrap();
        assert!(pdf.is_file() && !dir.join(".convert").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Converts a real presentation with LibreOffice and pdfium (downloaded):
    /// `SW_TEST_PPTX=deck.pptx cargo test real_pptx_import -- --ignored`
    #[test]
    #[ignore]
    fn real_pptx_import() {
        let src = PathBuf::from(std::env::var("SW_TEST_PPTX").expect("set SW_TEST_PPTX to a .pptx with a video on slide 2"));
        let dir = std::env::temp_dir().join(format!("sw-deck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pdfium = PdfiumLib { dir: dir.join("pdfium") };
        let ffmpeg = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().map(PathBuf::from).find(|p| p.is_file());
        let out = dir.join("deck");
        let deck = import(&[src], &out, &DeckTools { pdfium: &pdfium, ffmpeg }, &|f, what| eprintln!("{:3.0}% {what}", f * 100.0)).unwrap();
        assert_eq!(deck.slides.len(), 3);
        let img = image::open(out.join(&deck.slides[0].image)).unwrap();
        assert_eq!(img.width(), RENDER_W as u32);
        let m = &deck.slides[1].media[0];
        assert_eq!(m.kind, MediaKind::Video);
        assert!(out.join(&m.file).is_file());
        let wav = out.join(m.audio.as_ref().expect("audio prepared"));
        let (buf, _) = crate::engine::decode_file_for_tests(&wav);
        assert!((buf.duration() - 3.0).abs() < 0.2, "{}", buf.duration());
        assert_eq!(Deck::load(&out).unwrap(), deck);
        assert!(!out.join("deck.pdf").exists() && !out.join(".convert").exists(), "temp files cleaned up");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
