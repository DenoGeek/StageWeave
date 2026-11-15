//! Searching every project's songs, and reusing a song (audio, stems, lyrics,
//! timing, cues and mix) in another setlist without splitting it again.
//!
//! Reused files are hard-linked when the file system allows it, so a song in
//! three setlists takes the disk space of one. Each project folder still holds
//! complete files, so exporting or deleting one project never affects another.
//! (Re-splitting deletes the stem folder before writing, so links are never
//! written through.)

use std::path::Path;

use crate::model::{LyricLine, SongMixer};
use crate::store::{ProjectRow, SongRow, Store};
use crate::workspace::{Workspace, title_from_path, to_rel, unique_path};

/// Lower-case words of a search query.
pub fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Every term appears somewhere in `text`.
pub fn matches_all(terms: &[String], text: &str) -> bool {
    let text = text.to_lowercase();
    terms.iter().all(|t| text.contains(t.as_str()))
}

/// What a song brings with it when reused.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readiness {
    pub lyrics: bool,
    /// Lyric lines have timing (slides are ready).
    pub timed: usize,
    pub cues: usize,
}

pub fn readiness(song: &SongRow) -> Readiness {
    let timed = song.lyric_lines.as_deref().and_then(|j| serde_json::from_str::<Vec<LyricLine>>(j).ok()).map_or(0, |l| l.len());
    let cues = song.mixer.as_deref().and_then(|j| serde_json::from_str::<SongMixer>(j).ok()).map_or(0, |m| m.cues.len());
    Readiness { lyrics: !song.lyrics_raw.trim().is_empty(), timed, cues }
}

#[derive(Clone, Debug)]
pub struct SongHit {
    pub song: SongRow,
    pub project: String,
    /// Lower is better: 0 title, 1 artist / key / notes, 2 lyrics.
    pub rank: u8,
    /// The lyric line that matched, when the match was in the lyrics.
    pub snippet: Option<String>,
    pub ready: Readiness,
    /// How many other setlists hold the same song (same source file).
    pub copies: usize,
}

/// Songs matching `query` (all of them for an empty query), best first.
/// Copies of the same song in several setlists are folded into the most
/// complete one (`dedupe`).
pub fn search(songs: &[SongRow], projects: &[ProjectRow], query: &str, dedupe: bool) -> Vec<SongHit> {
    let terms = terms(query);
    let project_name = |id: i64| projects.iter().find(|p| p.id == id).map_or(String::new(), |p| p.name.clone());
    let mut hits: Vec<SongHit> = Vec::new();
    for s in songs {
        let title = format!("{} {}", s.title, title_from_path(Path::new(&s.path)));
        let meta = format!("{} {} {} {}", s.artist, s.song_key, s.notes, project_name(s.project_id));
        let (rank, snippet) = if terms.is_empty() || matches_all(&terms, &title) {
            (0, None)
        } else if matches_all(&terms, &format!("{title} {meta}")) {
            (1, None)
        } else if matches_all(&terms, &format!("{title} {meta} {}", s.lyrics_raw)) {
            // Show the lyric line holding the most terms.
            let line = s
                .lyrics_raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .max_by_key(|l| terms.iter().filter(|t| l.to_lowercase().contains(t.as_str())).count());
            (2, line.map(str::to_owned))
        } else {
            continue;
        };
        hits.push(SongHit { song: s.clone(), project: project_name(s.project_id), rank, snippet, ready: readiness(s), copies: 0 });
    }
    if dedupe {
        hits = fold_copies(hits);
    }
    // Stable: keeps most-recent-project-first within a rank.
    hits.sort_by_key(|h| h.rank);
    hits
}

/// One entry per source file, keeping the copy with the most work done.
fn fold_copies(hits: Vec<SongHit>) -> Vec<SongHit> {
    let mut out: Vec<SongHit> = Vec::new();
    for h in hits {
        let key = |s: &SongRow| if s.original_path.is_empty() { format!("#{}", s.id) } else { s.original_path.to_lowercase() };
        match out.iter_mut().find(|o| key(&o.song) == key(&h.song)) {
            Some(o) => {
                let better = |x: &SongHit| (x.ready.timed > 0, x.ready.cues, x.ready.lyrics, x.song.model_key.is_some());
                let copies = o.copies + 1;
                if better(&h) > better(o) {
                    *o = h;
                }
                o.copies = copies;
            }
            None => out.push(h),
        }
    }
    out
}

/// Projects whose name matches, or that hold a matching song.
pub fn search_projects(projects: &[ProjectRow], songs: &[SongRow], query: &str) -> Vec<ProjectRow> {
    let terms = terms(query);
    if terms.is_empty() {
        return projects.to_vec();
    }
    let by_name: Vec<ProjectRow> = projects.iter().filter(|p| matches_all(&terms, &p.name)).cloned().collect();
    let by_song = projects.iter().filter(|p| {
        !by_name.iter().any(|n| n.id == p.id)
            && songs.iter().any(|s| s.project_id == p.id && matches_all(&terms, &format!("{} {}", s.title, s.artist)))
    });
    by_name.iter().cloned().chain(by_song.cloned()).collect()
}

/// Link `src` to `dest`, or copy it where links aren't possible (FAT/exFAT
/// drives, different volumes).
fn link_or_copy(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(dest);
    if std::fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    crate::workspace::copy_with_progress(src, dest, &|_| {})
}

fn link_tree(src: &Path, dest: &Path) -> Result<(), String> {
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let (from, to) = (e.path(), dest.join(e.file_name()));
        if from.is_dir() {
            link_tree(&from, &to)?;
        } else if from.extension().is_none_or(|x| x != "part") {
            link_or_copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Add song `src_id` to project `dest_project` with everything already done
/// for it. Returns the new song's id.
pub fn reuse_song(store: &Store, ws: &Workspace, src_id: i64, dest_project: i64) -> Result<i64, String> {
    let src = store.song(src_id).ok_or("That song no longer exists")?;
    let src_project = store.project(src.project_id).ok_or("That song's project no longer exists")?;
    let project = store.project(dest_project).ok_or("Project not found")?;
    if let Some(crate::model::ItemSource::Deck { dir, slides }) = &src.source {
        // A slide deck: link its whole folder.
        let from = ws.abs(dir);
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "deck".into());
        let to = unique_path(&ws.project_dir(&project).join("decks"), &name);
        link_tree(&from, &to).map_err(|e| format!("Couldn't copy the slides: {e}"))?;
        let rel = to_rel(&ws.root(), &to).ok_or("Deck is outside the workspace")?;
        let source = crate::model::ItemSource::Deck { dir: rel.clone(), slides: *slides };
        let id = store.add_item(dest_project, src.kind, &format!("{rel}/deck.json"), &src.original_path, &src.title, &source)?;
        store.restore_song_fields(id, &src);
        return Ok(id);
    }
    let audio = ws.abs(&src.path);
    if !src.imported || !audio.is_file() {
        return Err(format!("“{}” can't be reused: its audio file is missing", src.title));
    }

    let file_name = audio.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
    let dest = unique_path(&ws.project_dir(&project).join("songs"), &file_name);
    link_or_copy(&audio, &dest)?;
    let rel = to_rel(&ws.root(), &dest).ok_or("Song is outside the workspace")?;
    let added = match &src.source {
        Some(crate::model::ItemSource::YouTube { url, video_id, video, show_video }) => {
            // Bring the video along too.
            let video = video.as_ref().map(|v| ws.abs(v)).filter(|v| v.is_file()).and_then(|v| {
                let name = v.file_name()?.to_string_lossy().into_owned();
                let to = unique_path(&ws.project_dir(&project).join("songs"), &name);
                link_or_copy(&v, &to).ok()?;
                to_rel(&ws.root(), &to)
            });
            let source = crate::model::ItemSource::YouTube { url: url.clone(), video_id: video_id.clone(), video, show_video: *show_video };
            store.add_item(dest_project, src.kind, &rel, &src.original_path, &src.title, &source)
        }
        _ => store.add_song(dest_project, &rel, &src.original_path, &src.title),
    };
    let id = match added {
        Ok(id) => id,
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            return Err(e);
        }
    };

    let mut row = src.clone();
    // Stems are filed under the model they were split with; pin it if this
    // project's default differs.
    if row.model_key.is_none() && src_project.default_model != project.default_model {
        row.model_key = Some(src_project.default_model.clone());
    }
    store.restore_song_fields(id, &row);

    // `stems/<slug>-<old id>/` → `stems/<slug>-<new id>/`
    let new_row = store.song(id).ok_or("Song vanished")?;
    let stems = |p: &ProjectRow, s: &SongRow| ws.stem_dir(p, s, "x:x").parent().map(Path::to_path_buf);
    if let (Some(from), Some(to)) = (stems(&src_project, &src), stems(&project, &new_row))
        && from.is_dir()
        && let Err(e) = link_tree(&from, &to)
    {
        store.remove_song(id);
        let _ = std::fs::remove_dir_all(&to);
        let _ = std::fs::remove_file(&dest);
        return Err(format!("Couldn't copy the stems: {e}"));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn song(id: i64, project: i64, title: &str, original: &str) -> SongRow {
        SongRow { id, project_id: project, title: title.into(), original_path: original.into(), imported: true, ..Default::default() }
    }

    fn project(id: i64, name: &str) -> ProjectRow {
        ProjectRow { id, name: name.into(), ..Default::default() }
    }

    #[test]
    fn search_ranks_title_then_details_then_lyrics() {
        let mut a = song(1, 1, "Way Maker", "/m/way.mp3");
        a.artist = "Sinach".into();
        let mut b = song(2, 1, "Goodness of God", "/m/god.mp3");
        b.lyrics_raw = "I love You Lord\nAll my life You have been faithful\nWay maker in my life".into();
        let mut c = song(3, 2, "Promises", "/m/p.mp3");
        c.notes = "way maker medley".into();
        let songs = vec![b.clone(), c.clone(), a.clone()];
        let projects = vec![project(1, "Sunday"), project(2, "Youth")];

        let hits = search(&songs, &projects, "way MAKER", false);
        assert_eq!(hits.iter().map(|h| h.song.id).collect::<Vec<_>>(), vec![1, 3, 2]);
        assert_eq!(hits[2].snippet.as_deref(), Some("Way maker in my life"));
        assert_eq!(hits[1].project, "Youth");

        // Words can come from different fields.
        assert_eq!(search(&songs, &projects, "sinach way", false).len(), 1);
        assert_eq!(search(&songs, &projects, "youth promises", false)[0].song.id, 3);
        assert!(search(&songs, &projects, "nothing like this", false).is_empty());
        assert_eq!(search(&songs, &projects, "  ", false).len(), 3);
    }

    #[test]
    fn copies_fold_into_the_most_complete_one() {
        let bare = song(1, 1, "Way Maker", "/m/Way.mp3");
        let mut done = song(2, 2, "Way Maker", "/m/way.mp3");
        done.lyric_lines = Some(r#"[{"text":"a","start":1.0,"end":2.0}]"#.into());
        let other = song(3, 2, "Way Maker (Live)", "/m/live.mp3");
        let hits = search(&[bare, done, other], &[project(1, "A"), project(2, "B")], "way", true);
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].song.id, hits[0].copies, hits[0].ready.timed), (2, 1, 1));
        assert_eq!(hits[1].song.id, 3);
    }

    #[test]
    fn projects_match_by_name_or_by_their_songs() {
        let projects = vec![project(1, "Sunday 12 Oct"), project(2, "Youth Night"), project(3, "Carols")];
        let songs = vec![song(1, 2, "Oceans", ""), song(2, 3, "Silent Night", "")];
        let names = |q: &str| search_projects(&projects, &songs, q).into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names("night"), vec!["Youth Night", "Carols"]);
        assert_eq!(names("oceans"), vec!["Youth Night"]);
        assert_eq!(names(""), vec!["Sunday 12 Oct", "Youth Night", "Carols"]);
    }

    #[test]
    fn reused_song_keeps_stems_lyrics_and_cues() {
        let dir = std::env::temp_dir().join(format!("sw-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Arc::new(Store::in_memory().unwrap());
        let ws = Workspace::new(store.clone());
        ws.set_root(&dir);

        let a = store.create_project("Sunday", "Sunday", "demucs:htdemucs").unwrap();
        let b = store.create_project("Youth", "Youth", "uvr:other.onnx").unwrap();
        std::fs::create_dir_all(dir.join("Sunday/songs")).unwrap();
        std::fs::write(dir.join("Sunday/songs/Oceans.mp3"), b"audio").unwrap();
        let src = store.add_song(a, "Sunday/songs/Oceans.mp3", "/Users/me/Oceans.mp3", "Oceans").unwrap();
        store.finish_import(src, 300.0, 44_100, &[1, 2, 3]);
        let mixer = r#"{"master_db":0.0,"tracks":[],"cues":[{"name":"Bridge","start":10.0,"end":20.0}]}"#;
        store.save_song_state(src, "You call me out", r#"[{"text":"You call me out","start":1.0,"end":3.0}]"#, "{}", mixer);
        let src_row = store.song(src).unwrap();
        let src_stems = ws.stem_dir(&store.project(a).unwrap(), &src_row, "demucs:htdemucs");
        std::fs::create_dir_all(&src_stems).unwrap();
        std::fs::write(src_stems.join("vocals.wav"), b"v").unwrap();

        // A same-named file already in the destination gets " (2)".
        std::fs::create_dir_all(dir.join("Youth/songs")).unwrap();
        std::fs::write(dir.join("Youth/songs/Oceans.mp3"), b"other").unwrap();

        let id = reuse_song(&store, &ws, src, b).unwrap();
        let row = store.song(id).unwrap();
        assert_eq!(row.project_id, b);
        assert_eq!(row.path, "Youth/songs/Oceans (2).mp3");
        assert_eq!(std::fs::read(ws.abs(&row.path)).unwrap(), b"audio");
        assert!(row.imported && row.duration == 300.0);
        assert_eq!((row.lyrics_raw.as_str(), row.mixer.as_deref()), ("You call me out", Some(mixer)));
        assert_eq!(row.model_key.as_deref(), Some("demucs:htdemucs"), "pinned to the model its stems came from");
        assert_eq!(row.thumb, vec![1u8, 2, 3]);
        let stems = ws.stem_dir(&store.project(b).unwrap(), &row, "demucs:htdemucs");
        assert_eq!(std::fs::read(stems.join("vocals.wav")).unwrap(), b"v");

        // Removing the original leaves the reused copy intact.
        std::fs::remove_dir_all(dir.join("Sunday")).unwrap();
        assert!(ws.abs(&row.path).is_file() && stems.join("vocals.wav").is_file());

        assert!(reuse_song(&store, &ws, src, b).is_err(), "source audio is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
