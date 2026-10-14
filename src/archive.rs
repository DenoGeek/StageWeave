//! Project archives: export a project (songs, lyrics, mixer, optional stems,
//! lyric backgrounds) to one `.zip`, and import it on another machine.
//!
//! ```text
//! musickit-project.json        project + song records
//! songs/<file>                 the song audio
//! thumbs/<old id>.bin          waveform thumbnails
//! stems/<slug>-<old id>/<model>/*.wav   (optional)
//! backgrounds/<file>           lyric backgrounds the songs use
//! ```
//! Song ids change on import, so stem folders are renamed to the new ids.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::store::{SongRow, Store};
use crate::workspace::{Workspace, join_rel, to_rel, unique_path};

pub const MANIFEST: &str = "musickit-project.json";
pub const FORMAT: u64 = 1;
/// Lyric backgrounds shared by all projects: `<workspace>/Backgrounds/`.
pub const BACKGROUNDS_DIR: &str = "Backgrounds";

#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    pub include_stems: bool,
}

pub fn backgrounds_dir(ws: &Workspace) -> PathBuf {
    ws.root().join(BACKGROUNDS_DIR)
}

/// The background file a song's lyric style points at (Image / Video kinds).
fn background_file(style: Option<&str>) -> Option<String> {
    let v: Value = serde_json::from_str(style?).ok()?;
    let file = v.get("background")?.get("file")?.as_str()?;
    (!file.is_empty()).then(|| file.to_owned())
}

fn set_background_file(style: &str, file: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(style) else { return style.to_owned() };
    if let Some(bg) = v.get_mut("background").and_then(Value::as_object_mut) {
        bg.insert("file".into(), Value::String(file.to_owned()));
    }
    v.to_string()
}

fn is_media(name: &str) -> bool {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "mp3" | "m4a" | "aac" | "flac" | "ogg" | "opus" | "wav" | "aif" | "aiff" | "mp4" | "mov" | "webm" | "mkv" | "jpg" | "jpeg" | "png" | "webp" | "gif"
    )
}

fn options_for(name: &str, size: u64) -> SimpleFileOptions {
    let method = if is_media(name) { CompressionMethod::Stored } else { CompressionMethod::Deflated };
    SimpleFileOptions::default().compression_method(method).large_file(size >= u32::MAX as u64)
}

/// Copy with byte-level progress.
fn copy_counting(from: &mut impl Read, to: &mut impl Write, done: &mut u64, total: u64, progress: &dyn Fn(f32, &str), label: &str) -> io::Result<()> {
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
        *done += n as u64;
        progress((*done as f64 / total.max(1) as f64) as f32, label);
    }
}

/// The `<slug>-<id>` folder holding a song's stems.
fn stem_dir_name(ws: &Workspace, project: &crate::store::ProjectRow, song: &SongRow) -> Option<String> {
    let dir = ws.stem_dir(project, song, "x:y");
    Some(dir.parent()?.file_name()?.to_string_lossy().into_owned())
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_in(&p));
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// --------------------------------------------------------------------------- export

pub fn export_project(
    store: &Store,
    ws: &Workspace,
    project_id: i64,
    dest: &Path,
    opts: &ExportOptions,
    progress: &dyn Fn(f32, &str),
) -> Result<(), String> {
    let project = store.project(project_id).ok_or("Project not found")?;
    let songs = store.songs(project_id);
    let root = ws.root();

    // (name in zip, file on disk)
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut used_names = BTreeSet::new();
    let mut song_json = Vec::new();
    let mut backgrounds = BTreeSet::new();
    for s in &songs {
        let src = join_rel(&root, &s.path);
        if !src.is_file() {
            return Err(format!("Song file is missing: {}", src.display()));
        }
        let file_name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
        let mut name = format!("songs/{file_name}");
        let mut n = 2;
        while !used_names.insert(name.clone()) {
            let p = Path::new(&file_name);
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            name = format!("songs/{stem} ({n}){ext}");
            n += 1;
        }
        entries.push((name.clone(), src));
        let stem_dir = stem_dir_name(ws, &project, s);
        if opts.include_stems
            && let Some(d) = &stem_dir
        {
            let dir = ws.project_dir(&project).join("stems").join(d);
            for f in files_in(&dir) {
                if let Some(rel) = to_rel(&dir, &f) {
                    entries.push((format!("stems/{d}/{rel}"), f));
                }
            }
        }
        if let Some(bg) = background_file(s.lyric_style.as_deref()) {
            let p = backgrounds_dir(ws).join(&bg);
            if p.is_file() && backgrounds.insert(bg.clone()) {
                entries.push((format!("backgrounds/{bg}"), p));
            }
        }
        song_json.push(json!({
            "id": s.id,
            "position": s.position,
            "imported": s.imported,
            "title": s.title,
            "artist": s.artist,
            "duration": s.duration,
            "sample_rate": s.sample_rate,
            "model_key": s.model_key,
            "song_key": s.song_key,
            "bpm": s.bpm,
            "notes": s.notes,
            "lyrics_raw": s.lyrics_raw,
            "lyric_lines": s.lyric_lines,
            "lyric_style": s.lyric_style,
            "mixer": s.mixer,
            "file": name,
            "original_path": s.original_path,
            "stem_dir": stem_dir,
            "thumb": (!s.thumb.is_empty()).then(|| format!("thumbs/{}.bin", s.id)),
        }));
    }
    let manifest = json!({
        "format": FORMAT,
        "app_version": env!("CARGO_PKG_VERSION"),
        "project": { "name": project.name, "default_model": project.default_model },
        "songs": song_json,
        "backgrounds": backgrounds,
    });

    let part = dest.with_extension("part");
    let result = (|| -> Result<(), String> {
        let file = File::create(&part).map_err(|e| format!("Can't create {}: {e}", part.display()))?;
        let mut zip = ZipWriter::new(io::BufWriter::new(file));
        let err = |e: zip::result::ZipError| e.to_string();
        let io_err = |e: io::Error| e.to_string();
        zip.start_file(MANIFEST, options_for(MANIFEST, 0)).map_err(err)?;
        zip.write_all(serde_json::to_string_pretty(&manifest).unwrap_or_default().as_bytes()).map_err(io_err)?;
        for s in &songs {
            if !s.thumb.is_empty() {
                zip.start_file(format!("thumbs/{}.bin", s.id), options_for("t.bin", 0)).map_err(err)?;
                zip.write_all(&s.thumb).map_err(io_err)?;
            }
        }
        let total: u64 = entries.iter().map(|(_, p)| p.metadata().map_or(0, |m| m.len())).sum();
        let mut done = 0u64;
        for (name, path) in &entries {
            let size = path.metadata().map_or(0, |m| m.len());
            zip.start_file(name.as_str(), options_for(name, size)).map_err(err)?;
            let mut f = File::open(path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
            let label = name.rsplit('/').next().unwrap_or(name);
            copy_counting(&mut f, &mut zip, &mut done, total, progress, label).map_err(io_err)?;
        }
        zip.finish().map_err(err)?.flush().map_err(io_err)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&part, dest).map_err(|e| format!("Can't save {}: {e}", dest.display()))?;
            progress(1.0, "Done");
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

// --------------------------------------------------------------------------- import

/// A zip entry name split into safe components, or `None` if it could escape.
fn safe_components(name: &str) -> Option<Vec<String>> {
    if name.starts_with('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    let parts: Vec<String> = name.split('/').filter(|c| !c.is_empty() && *c != ".").map(str::to_owned).collect();
    if parts.is_empty() || parts.iter().any(|c| c == ".." || c.contains(':')) {
        return None;
    }
    Some(parts)
}

pub fn import_project(store: &Store, ws: &Workspace, zip_path: &Path, progress: &dyn Fn(f32, &str)) -> Result<i64, String> {
    let file = File::open(zip_path).map_err(|e| format!("Can't open {}: {e}", zip_path.display()))?;
    let mut zip = ZipArchive::new(io::BufReader::new(file)).map_err(|e| format!("Not a valid project archive: {e}"))?;

    // Check every entry before writing anything.
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().map_err(|e| e.to_string())?.into_owned();
        if f.enclosed_name().is_none() || safe_components(&name).is_none() {
            return Err(format!("Unsafe path in archive: {name}"));
        }
        total += f.size();
    }

    let manifest: Value = {
        let mut f = zip.by_name(MANIFEST).map_err(|_| "This zip isn't a MusicKit project (no musickit-project.json)")?;
        let mut s = String::new();
        f.read_to_string(&mut s).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| format!("Broken project file: {e}"))?
    };
    if manifest["format"].as_u64().unwrap_or(0) > FORMAT {
        return Err("This project was exported by a newer MusicKit. Update the app to import it.".into());
    }
    let name = manifest["project"]["name"].as_str().unwrap_or("Imported Project").to_owned();
    let default_model = manifest["project"]["default_model"].as_str().unwrap_or("demucs:htdemucs").to_owned();
    let existing: BTreeSet<String> = store.list_projects().into_iter().map(|p| p.name).collect();
    let mut new_name = name.clone();
    let mut n = 2;
    while existing.contains(&new_name) {
        new_name = if n == 2 { format!("{name} (imported)") } else { format!("{name} (imported {n})") };
        n += 1;
    }
    let folder = ws.new_project_folder(&new_name);
    let project_id = store.create_project(&new_name, &folder, &default_model)?;
    let mut created_bgs: Vec<PathBuf> = Vec::new();

    let result = import_into(store, ws, &mut zip, &manifest, project_id, total, &mut created_bgs, progress);
    match result {
        Ok(()) => {
            ws.write_manifest(project_id);
            progress(1.0, "Done");
            Ok(project_id)
        }
        Err(e) => {
            store.delete_project(project_id);
            let _ = std::fs::remove_dir_all(ws.abs(&folder));
            for p in created_bgs {
                let _ = std::fs::remove_file(p);
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn import_into<R: Read + io::Seek>(
    store: &Store,
    ws: &Workspace,
    zip: &mut ZipArchive<R>,
    manifest: &Value,
    project_id: i64,
    total: u64,
    created_bgs: &mut Vec<PathBuf>,
    progress: &dyn Fn(f32, &str),
) -> Result<(), String> {
    let project = store.project(project_id).ok_or("Project vanished")?;
    let root = ws.root();
    let songs_dir = ws.project_dir(&project).join("songs");
    std::fs::create_dir_all(&songs_dir).map_err(|e| e.to_string())?;
    let mut done = 0u64;

    let extract = |zip: &mut ZipArchive<R>, name: &str, dest: &Path, done: &mut u64| -> Result<(), String> {
        safe_components(name).ok_or_else(|| format!("Unsafe path in archive: {name}"))?;
        let mut f = zip.by_name(name).map_err(|_| format!("Missing from archive: {name}"))?;
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let part = dest.with_extension(format!("{}.part", dest.extension().map(|e| e.to_string_lossy()).unwrap_or_default()));
        let mut out = File::create(&part).map_err(|e| format!("Can't write {}: {e}", dest.display()))?;
        let label = name.rsplit('/').next().unwrap_or(name).to_owned();
        copy_counting(&mut f, &mut out, done, total, progress, &label).map_err(|e| e.to_string())?;
        drop(out);
        std::fs::rename(&part, dest).map_err(|e| e.to_string())
    };

    // Backgrounds: reuse an identical file, otherwise extract under a free name.
    let bg_dir = root.join(BACKGROUNDS_DIR);
    let mut bg_rename: HashMap<String, String> = HashMap::new();
    for bg in manifest["backgrounds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        let entry = format!("backgrounds/{bg}");
        let Some(parts) = safe_components(bg).filter(|p| p.len() == 1) else { continue };
        let Ok(size) = zip.by_name(&entry).map(|f| f.size()) else { continue };
        let target = bg_dir.join(&parts[0]);
        if target.is_file() && target.metadata().map_or(0, |m| m.len()) == size {
            done += size;
            continue;
        }
        let dest = if target.exists() { unique_path(&bg_dir, &parts[0]) } else { target };
        extract(zip, &entry, &dest, &mut done)?;
        created_bgs.push(dest.clone());
        let new_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if new_name != bg {
            bg_rename.insert(bg.to_owned(), new_name);
        }
    }

    // Stem files grouped by their song folder in the archive.
    let mut stem_entries: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for name in zip.file_names().filter_map(|n| n.ok().map(|c| c.into_owned())).collect::<Vec<_>>() {
        if let Some(parts) = safe_components(&name)
            && parts.len() >= 3
            && parts[0] == "stems"
            && !name.ends_with('/')
        {
            stem_entries.entry(parts[1].clone()).or_default().push((name.clone(), parts[2..].join("/")));
        }
    }

    let mut songs: Vec<&Value> = manifest["songs"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    songs.sort_by_key(|s| s["position"].as_u64().unwrap_or(0));
    for s in songs {
        let entry = s["file"].as_str().ok_or("Song without a file in archive")?;
        let parts = safe_components(entry).ok_or_else(|| format!("Unsafe path in archive: {entry}"))?;
        let file_name = parts.last().cloned().unwrap_or_else(|| "song".into());
        let dest = unique_path(&songs_dir, &file_name);
        extract(zip, entry, &dest, &mut done)?;
        let rel = to_rel(&root, &dest).ok_or("Song outside workspace")?;
        let text = |k: &str| s[k].as_str().unwrap_or_default().to_owned();
        let opt = |k: &str| s[k].as_str().map(str::to_owned);
        let title = text("title");
        let id = store.add_song(project_id, &rel, &text("original_path"), &title)?;

        let mut thumb = Vec::new();
        if let Some(t) = s["thumb"].as_str()
            && safe_components(t).is_some()
            && let Ok(mut f) = zip.by_name(t)
        {
            f.read_to_end(&mut thumb).map_err(|e| e.to_string())?;
        }
        let mut lyric_style = opt("lyric_style");
        if let Some(style) = &lyric_style
            && let Some(old) = background_file(Some(style))
            && let Some(new) = bg_rename.get(&old)
        {
            lyric_style = Some(set_background_file(style, new));
        }
        let row = SongRow {
            imported: s["imported"].as_bool().unwrap_or(true),
            title,
            artist: text("artist"),
            duration: s["duration"].as_f64().unwrap_or(0.0),
            sample_rate: s["sample_rate"].as_u64().unwrap_or(0) as u32,
            model_key: opt("model_key"),
            song_key: text("song_key"),
            bpm: s["bpm"].as_f64(),
            notes: text("notes"),
            lyrics_raw: text("lyrics_raw"),
            lyric_lines: opt("lyric_lines"),
            lyric_style,
            mixer: opt("mixer"),
            thumb,
            ..Default::default()
        };
        store.restore_song_fields(id, &row);

        // Stems: `stems/<slug>-<old id>/…` → `stems/<slug>-<new id>/…`.
        if let Some(old_dir) = s["stem_dir"].as_str()
            && let Some(files) = stem_entries.get(old_dir)
        {
            let new_song = store.songs(project_id).into_iter().find(|r| r.id == id).ok_or("Song vanished")?;
            let new_dir = ws.stem_dir(&project, &new_song, "x:y");
            let new_dir = new_dir.parent().ok_or("Bad stem folder")?;
            for (name, rel) in files {
                extract(zip, name, &join_rel(new_dir, rel), &mut done)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Env {
        store: Arc<Store>,
        ws: Arc<Workspace>,
        dir: PathBuf,
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn env(tag: &str) -> Env {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("musickit-archive-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let ws = Workspace::new(store.clone());
        ws.set_root(&dir);
        Env { store, ws, dir }
    }

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    /// A project with two songs, stems for the first and a video background.
    fn sample(e: &Env) -> i64 {
        let folder = e.ws.new_project_folder("Sunday");
        let pid = e.store.create_project("Sunday", &folder, "demucs:htdemucs").unwrap();
        let project = e.store.project(pid).unwrap();
        let style = r#"{"theme":"Aurora","background":{"kind":"Video","file":"clouds.mp4"}}"#;
        for (i, name) in ["Amazing Grace.mp3", "Forever.mp3"].iter().enumerate() {
            let rel = format!("{folder}/songs/{name}");
            write(&e.ws.abs(&rel), format!("audio {i}").as_bytes());
            let id = e.store.add_song(pid, &rel, &format!("/orig/{name}"), name).unwrap();
            let row = SongRow {
                imported: true,
                title: format!("Song {i}"),
                artist: "Choir".into(),
                duration: 100.0 + i as f64,
                sample_rate: 44_100,
                song_key: "G".into(),
                bpm: Some(72.0),
                notes: "slow".into(),
                lyrics_raw: "Line one\nLine two".into(),
                lyric_lines: Some(r#"[{"text":"Line one","start":1.0,"end":2.0}]"#.into()),
                lyric_style: (i == 0).then(|| style.to_owned()),
                mixer: Some(r#"{"master_db":-3.0,"tracks":[]}"#.into()),
                thumb: vec![1, 2, 3, i as u8],
                ..Default::default()
            };
            e.store.restore_song_fields(id, &row);
            if i == 0 {
                let song = e.store.songs(pid).into_iter().find(|s| s.id == id).unwrap();
                let dir = e.ws.stem_dir(&project, &song, "demucs:htdemucs");
                write(&dir.join("vocals.wav"), b"vocals");
                write(&dir.join("drums.wav"), b"drums");
            }
        }
        write(&backgrounds_dir(&e.ws).join("clouds.mp4"), b"cloudy video");
        pid
    }

    #[test]
    fn export_then_import_on_another_machine() {
        let a = env("a");
        let pid = sample(&a);
        let zip_path = a.dir.join("out.zip");
        export_project(&a.store, &a.ws, pid, &zip_path, &ExportOptions { include_stems: true }, &|_, _| {}).unwrap();
        assert!(zip_path.is_file() && !zip_path.with_extension("part").exists());

        let b = env("b");
        // An unrelated, different background with the same name already exists here.
        write(&backgrounds_dir(&b.ws).join("clouds.mp4"), b"something else entirely");
        let last = std::cell::Cell::new(0.0f32);
        let new_id = import_project(&b.store, &b.ws, &zip_path, &|p, _| last.set(p)).unwrap();
        assert_eq!(last.get(), 1.0);

        let project = b.store.project(new_id).unwrap();
        assert_eq!(project.name, "Sunday");
        assert_eq!(project.default_model, "demucs:htdemucs");
        let orig = a.store.songs(pid);
        let songs = b.store.songs(new_id);
        assert_eq!(songs.len(), 2);
        for (o, s) in orig.iter().zip(&songs) {
            assert_eq!(s.title, o.title);
            assert_eq!(s.artist, o.artist);
            assert_eq!(s.duration, o.duration);
            assert_eq!(s.sample_rate, o.sample_rate);
            assert_eq!(s.song_key, o.song_key);
            assert_eq!(s.bpm, o.bpm);
            assert_eq!(s.notes, o.notes);
            assert_eq!(s.lyrics_raw, o.lyrics_raw);
            assert_eq!(s.lyric_lines, o.lyric_lines);
            assert_eq!(s.mixer, o.mixer);
            assert_eq!(s.thumb, o.thumb);
            assert_eq!(s.original_path, o.original_path);
            assert!(s.imported);
            assert_eq!(std::fs::read(b.ws.abs(&s.path)).unwrap(), std::fs::read(a.ws.abs(&o.path)).unwrap());
        }
        // Stems moved to the new song id's folder.
        let stems = b.ws.stem_dir(&project, &songs[0], "demucs:htdemucs");
        assert_eq!(std::fs::read(stems.join("vocals.wav")).unwrap(), b"vocals");
        assert_eq!(std::fs::read(stems.join("drums.wav")).unwrap(), b"drums");
        // Background extracted under a free name and the style points at it.
        let bg = background_file(songs[0].lyric_style.as_deref()).unwrap();
        assert_ne!(bg, "clouds.mp4");
        assert_eq!(std::fs::read(backgrounds_dir(&b.ws).join(&bg)).unwrap(), b"cloudy video");
        assert_eq!(std::fs::read(backgrounds_dir(&b.ws).join("clouds.mp4")).unwrap(), b"something else entirely");
        assert!(b.ws.project_dir(&project).join("project.json").is_file());
    }

    #[test]
    fn export_without_stems_and_identical_background_is_reused() {
        let a = env("c");
        let pid = sample(&a);
        let zip_path = a.dir.join("small.zip");
        export_project(&a.store, &a.ws, pid, &zip_path, &ExportOptions { include_stems: false }, &|_, _| {}).unwrap();
        let names: Vec<String> = ZipArchive::new(File::open(&zip_path).unwrap()).unwrap().file_names().filter_map(|n| n.ok().map(|c| c.into_owned())).collect();
        assert!(!names.iter().any(|n| n.starts_with("stems/")));
        assert!(names.contains(&"backgrounds/clouds.mp4".to_owned()));

        let b = env("d");
        write(&backgrounds_dir(&b.ws).join("clouds.mp4"), b"cloudy video");
        let id = import_project(&b.store, &b.ws, &zip_path, &|_, _| {}).unwrap();
        let s = &b.store.songs(id)[0];
        assert_eq!(background_file(s.lyric_style.as_deref()).as_deref(), Some("clouds.mp4"));
        assert_eq!(std::fs::read_dir(backgrounds_dir(&b.ws)).unwrap().count(), 1);
    }

    #[test]
    fn importing_twice_gives_distinct_projects() {
        let a = env("e");
        let pid = sample(&a);
        let zip_path = a.dir.join("p.zip");
        export_project(&a.store, &a.ws, pid, &zip_path, &ExportOptions::default(), &|_, _| {}).unwrap();
        let b = env("f");
        let one = import_project(&b.store, &b.ws, &zip_path, &|_, _| {}).unwrap();
        let two = import_project(&b.store, &b.ws, &zip_path, &|_, _| {}).unwrap();
        let (p1, p2) = (b.store.project(one).unwrap(), b.store.project(two).unwrap());
        assert_eq!(p1.name, "Sunday");
        assert_eq!(p2.name, "Sunday (imported)");
        assert_ne!(p1.folder, p2.folder);
        assert!(b.store.songs(two).iter().all(|s| b.ws.abs(&s.path).is_file()));
    }

    #[test]
    fn zip_slip_entries_are_rejected() {
        let b = env("g");
        let zip_path = b.dir.join("evil.zip");
        {
            let mut z = ZipWriter::new(File::create(&zip_path).unwrap());
            let o = SimpleFileOptions::default();
            z.start_file(MANIFEST, o).unwrap();
            z.write_all(
                br#"{"format":1,"project":{"name":"Evil","default_model":"demucs:htdemucs"},
                    "songs":[{"title":"x","file":"../evil.txt","position":0}],"backgrounds":["../evil.txt"]}"#,
            )
            .unwrap();
            z.start_file("../evil.txt", o).unwrap();
            z.write_all(b"pwned").unwrap();
            z.finish().unwrap();
        }
        let err = import_project(&b.store, &b.ws, &zip_path, &|_, _| {}).unwrap_err();
        assert!(err.contains("Unsafe"), "{err}");
        assert!(!b.dir.parent().unwrap().join("evil.txt").exists());
        assert!(b.store.list_projects().is_empty(), "nothing created");
    }

    #[test]
    fn failed_import_cleans_up() {
        let b = env("h");
        let zip_path = b.dir.join("broken.zip");
        {
            let mut z = ZipWriter::new(File::create(&zip_path).unwrap());
            z.start_file(MANIFEST, SimpleFileOptions::default()).unwrap();
            z.write_all(br#"{"format":1,"project":{"name":"Broken"},"songs":[{"title":"x","file":"songs/missing.mp3"}]}"#).unwrap();
            z.finish().unwrap();
        }
        assert!(import_project(&b.store, &b.ws, &zip_path, &|_, _| {}).is_err());
        assert!(b.store.list_projects().is_empty());
        assert!(!b.ws.abs("Broken").exists());
    }

    #[test]
    fn safe_components_rejects_escapes() {
        assert!(safe_components("songs/a.mp3").is_some());
        for bad in ["../x", "a/../../x", "/etc/passwd", "C:/x", "a\\..\\x", ""] {
            assert!(safe_components(bad).is_none(), "{bad}");
        }
    }
}
