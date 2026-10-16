//! Developer helpers.
//!
//! * `--demo` creates a fake "Sunday Demo" setlist (mock audio, mock splitter)
//!   so every screen can be explored without real audio. Add
//!   `STAGEWEAVE_SCREEN=home|project|song` to choose the start screen; `song`
//!   opens the first song, splits it, generates lyrics and plays.
//! * `STAGEWEAVE_OPEN=song.mp3` adds a file to a "Dev" project and opens it;
//!   `STAGEWEAVE_SPLIT=<model id>` then separates it once the engine is ready.
//! * `STAGEWEAVE_SHOW_MODELS=1` / `STAGEWEAVE_SHOW_SETTINGS=1` open those windows at startup.
//! * `STAGEWEAVE_INSTALL=1` installs the separation engine if it is missing.
//! * `STAGEWEAVE_BG=Clouds` uses a starter background; `STAGEWEAVE_CUE=40,60`
//!   adds a looping cue with drums muted; `STAGEWEAVE_SECTION=start,end` adds
//!   sections (a Chorus with its own mix); `STAGEWEAVE_LAYOUT=lyrics|perform`
//!   picks the workspace (for README screenshots).
//! * `STAGEWEAVE_SEARCH=<query>` opens ⌘K search, `STAGEWEAVE_HOME_QUERY=<query>`
//!   filters the home screen, `STAGEWEAVE_LIBRARY=<project>` opens that project's
//!   "Add from Library".
//! * `STAGEWEAVE_SCREENSHOT=out.ppm` (+ optional `STAGEWEAVE_SCREENSHOT_AT=secs`)
//!   saves a screenshot of the main window and quits. Used for UI review.
//!   With `STAGEWEAVE_SCREENSHOT_AT=stems` it waits until stems are loaded and
//!   have played for a few seconds.

use std::path::PathBuf;

use crate::app::StageWeaveApp;

pub const DEMO_LYRICS: &str = "\
Ana mipango mema, anakupenda (He has good plans, He loves You)
Every street is singing back the same
Hold the moment, never let it go
Turn it up and let the whole world know

We are the echo in the night
Burning brighter than the light
Sing it louder, sing it true
Every word comes back to you

Footsteps falling to the beat
Heartbeat racing down the street
Hold the moment, never let it go
Turn it up and let the whole world know

We are the echo in the night
Burning brighter than the light
Sing it louder, sing it true
Every word comes back to you";

#[derive(Default)]
pub struct Dev {
    pub demo: bool,
    demo_split_started: bool,
    demo_finished: bool,
    open: Option<PathBuf>,
    split_model: Option<String>,
    install: bool,
    install_started: bool,
    /// Seconds after start, or `None` for "after stems are playing".
    screenshot: Option<(PathBuf, Option<f64>)>,
    stems_at: Option<f64>,
    screenshot_requested: bool,
}

impl Dev {
    /// Demo backends (mock audio, in-memory database) without any env hooks.
    #[cfg(test)]
    pub fn demo() -> Self {
        Self { demo: true, ..Default::default() }
    }

    pub fn from_env() -> Self {
        let demo = std::env::args().any(|a| a == "--demo");
        let screenshot = std::env::var("STAGEWEAVE_SCREENSHOT").ok().map(|p| {
            let at = match std::env::var("STAGEWEAVE_SCREENSHOT_AT").ok().as_deref() {
                Some("stems") => None,
                Some(s) => Some(s.parse().unwrap_or(6.0)),
                None => Some(6.0),
            };
            (PathBuf::from(p), at)
        });
        Self {
            demo,
            screenshot,
            open: std::env::var("STAGEWEAVE_OPEN").ok().map(PathBuf::from),
            split_model: std::env::var("STAGEWEAVE_SPLIT").ok(),
            install: std::env::var("STAGEWEAVE_INSTALL").is_ok(),
            ..Default::default()
        }
    }
}

pub fn start(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if std::env::var("STAGEWEAVE_SHOW_SETTINGS").is_ok() {
        app.show_settings = true;
    }
    if std::env::var("STAGEWEAVE_AI_PANEL").is_ok() {
        app.ai_panel.open = true;
        app.ai_panel.align_text = DEMO_LYRICS.into();
        if std::env::var("STAGEWEAVE_AI_MODE").as_deref() == Ok("align") {
            app.ai_panel.mode = crate::app::AiMode::Align;
        }
    }
    if std::env::var("STAGEWEAVE_SHOW_MODELS").is_ok() {
        app.show_models = true;
    }
    if let Some(path) = app.dev.open.clone() {
        let id = match app.projects.iter().find(|p| p.name == "Dev") {
            Some(p) => p.id,
            None => app.create_project("Dev"),
        };
        app.open_project(id);
        if !app.songs.iter().any(|s| s.original_path == path.to_string_lossy()) {
            app.add_songs(&[path]);
        }
    }
    if !app.dev.demo {
        return;
    }
    seed_demo(app);
    let fake_ai = std::env::var("STAGEWEAVE_AI_FAKE_RESULT").is_ok();
    match std::env::var("STAGEWEAVE_SCREEN").as_deref() {
        Ok("home") => app.go_home(),
        Ok("song") => {
            if let Some(first) = app.songs.first().map(|s| s.id) {
                app.open_song(first, ctx);
                if fake_ai {
                    // Preview of the AI result view without calling an API.
                    let lines = DEMO_LYRICS
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .enumerate()
                        .map(|(i, l)| crate::model::LyricLine::new(l.to_string(), 14.0 + i as f64 * 9.5, 23.0 + i as f64 * 9.5))
                        .collect();
                    app.ai_panel.result =
                        Some((first, crate::app::AiResult::Transcript(crate::ai::Transcript { note: None, text: DEMO_LYRICS.into(), language: Some("english".into()), lines })));
                }
            }
        }
        _ => {}
    }
    if let Ok(q) = std::env::var("STAGEWEAVE_SEARCH") {
        (app.search.open, app.search.query) = (true, q);
    }
    if let Ok(q) = std::env::var("STAGEWEAVE_HOME_QUERY") {
        app.go_home();
        app.home_query = q;
    }
    if let Ok(name) = std::env::var("STAGEWEAVE_LIBRARY")
        && let Some(id) = app.projects.iter().find(|p| p.name == name).map(|p| p.id)
    {
        app.open_project(id);
        crate::ui::search::open_library(app);
    }
}

/// A believable Sunday setlist backed by the mock decoder.
fn seed_demo(app: &mut StageWeaveApp) {
    use crate::engine::AudioDecoder;
    use crate::store::SongMeta;
    let songs = [
        ("Echo In The Night", "The Demo Band", "G", 112.0, "Start soft, full band on chorus 2"),
        ("Amazing Grace (My Chains Are Gone)", "Chris Tomlin", "F", 74.0, "Capo 1 · acoustic intro"),
        ("Way Maker", "Sinach", "E", 68.0, "Repeat bridge x3"),
        ("Goodness of God", "Bethel Music", "Ab", 63.0, ""),
        ("Wakamau ni Mjaka", "Kwaya", "C", 96.0, "Swahili lyrics on screen"),
    ];
    for name in ["Christmas Carols 2025", "Youth Night"] {
        let id = app.create_project(name);
        let _ = app.store.add_song(id, "x/a.mp3", "/a.mp3", "Song");
    }
    let id = app.create_project("Sunday Demo");
    app.open_project(id);
    for (i, (title, artist, key, bpm, notes)) in songs.iter().enumerate() {
        let file = format!("{title}.wav");
        let Ok(sid) = app.store.add_song(id, &format!("Sunday Demo/songs/{file}"), &format!("/Users/me/Music/{file}"), title) else {
            continue;
        };
        // Placeholder file so the song isn't "missing" (the mock decoder ignores content).
        let dest = app.ws.abs(&format!("Sunday Demo/songs/{file}"));
        let _ = std::fs::create_dir_all(dest.parent().unwrap_or(std::path::Path::new(".")));
        let _ = std::fs::write(&dest, b"");
        let src = crate::engine::mock::MockDecoder.load(std::path::Path::new(&file)).ok();
        let thumb: Vec<u8> = src
            .as_ref()
            .map(|s| s.peaks.data.chunks(10).map(|c| (c.iter().fold(0.0f32, |m, v| m.max(*v)) * 255.0) as u8).collect())
            .unwrap_or_default();
        app.store.finish_import(sid, src.map_or(200.0, |s| s.info.duration), 44_100, &thumb);
        app.store.update_song_meta(
            sid,
            &SongMeta {
                title: title.to_string(),
                artist: artist.to_string(),
                song_key: key.to_string(),
                bpm: Some(*bpm),
                notes: notes.to_string(),
                model_key: (i == 2).then(|| "mock:karaoke".to_owned()),
            },
        );
        if i == 0 {
            app.store.save_song_state(sid, DEMO_LYRICS, "[]", "{}", "{}");
        }
    }
    app.reload_projects();
    app.reload_songs();
    app.selected_song = app.songs.get(1).map(|s| s.id);
}

pub fn tick(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if app.dev.install
        && !app.dev.install_started
        && let Some(env) = &app.env
        && matches!(
            env.status(),
            crate::engine::python_env::EnvStatus::NotInstalled | crate::engine::python_env::EnvStatus::Broken(_)
        )
    {
        app.dev.install_started = true;
        env.install(ctx.clone());
    }
    // STAGEWEAVE_OPEN: open the dev song once it has been imported.
    if app.dev.open.is_some()
        && app.current_song_id().is_none()
        && let Some(song) = app.songs.iter().find(|s| s.imported).map(|s| s.id)
        && !app.dev.demo_split_started
    {
        app.open_song(song, ctx);
    }
    let source_ready = app.session.source.is_some() && !app.is_loading();
    let engine_ready = app.env.as_ref().is_none_or(|e| e.info().is_some());
    let wants_split = (app.dev.demo && app.current_song_id().is_some()) || app.dev.split_model.is_some();
    if wants_split && !app.dev.demo_split_started && source_ready && engine_ready && !app.has_tracks() {
        app.dev.demo_split_started = true;
        if let (Some(model_id), Some(song)) = (app.dev.split_model.clone(), app.current_song_id())
            && let Some(m) = app.models.iter().find(|m| m.id == model_id).map(|m| m.key())
        {
            app.set_song_model(song, &m);
        }
        if let Some(song) = app.current_song_id() {
            app.split_song(song);
        }
    }
    if app.dev.demo && !app.dev.demo_finished && app.has_tracks() {
        app.dev.demo_finished = true;
        app.generate_lyrics();
        app.seek(52.0);
        app.toggle_play();
        if std::env::var("STAGEWEAVE_AI_FAKE_ALIGN").is_ok()
            && let Some(song) = app.current_song_id()
        {
            // Real on-device alignment over the demo's vocal envelope.
            let vocals = app.session.tracks.iter().find(|t| t.kind == crate::model::StemKind::Vocals).map(|t| t.peaks.clone());
            let a = crate::align::align(DEMO_LYRICS, None, vocals.as_ref(), app.session.duration());
            app.ai_panel.align_text = DEMO_LYRICS.into();
            app.ai_panel.result = Some((song, crate::app::AiResult::Alignment(a)));
            app.ai_panel.open = true;
            app.toggle_play();
        }
        // STAGEWEAVE_PANEL=song: the left panel's Song tab (models, stems).
        if std::env::var("STAGEWEAVE_PANEL").as_deref() == Ok("song") {
            app.project_tab = 1;
            app.selected_track = Some(1);
            app.editor_tab = 1;
        }
        if std::env::var("STAGEWEAVE_LYRIC_LIST").is_ok() {
            app.toggle_play(); // pause
            app.lyric_clip_select(2);
            app.lyric_begin_text_edit(4);
            app.lyric_edit.edit_in_list = true;
            app.editor_tab = 0;
        }
        if std::env::var("STAGEWEAVE_LYRIC_TIMELINE").is_ok() {
            app.toggle_play(); // pause
            app.lyric_clip_select(3);
            app.lyric_begin_text_edit(3);
        }
        match std::env::var("STAGEWEAVE_LAYOUT").as_deref() {
            Ok("lyrics") => (app.layout, app.editor_tab) = (crate::app::EditorLayout::Lyrics, 0),
            Ok("perform") => app.layout = crate::app::EditorLayout::Perform,
            _ => {}
        }
        if let Ok(title) = std::env::var("STAGEWEAVE_BG")
            && let Some(s) = crate::backgrounds::STARTERS.iter().find(|s| s.title == title)
            && crate::backgrounds::ensure_starter(&app.ws.root(), s).is_ok()
        {
            app.set_background(crate::backgrounds::background_for(s.file));
        }
        if let Some((start, end)) = std::env::var("STAGEWEAVE_CUE").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        }) {
            app.session.cues.push(crate::model::Cue { name: "Chorus".into(), start, end, mix: None });
            app.activate_cue(app.session.cues.len() - 1);
            if let Some(t) = app.session.tracks.iter_mut().find(|t| t.kind == crate::model::StemKind::Drums) {
                t.muted = true;
            }
        }
        // STAGEWEAVE_SECTION=start,end: a "Chorus" section with drums muted
        // and bass down, plus a plain "Verse" before it.
        if let Some((start, end)) = std::env::var("STAGEWEAVE_SECTION").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
        }) {
            use crate::model::{Section, StemKind, insert_section};
            let verse = Section { name: "Verse".into(), start: (start - 30.0).max(0.0), end: start, mix: None };
            insert_section(&mut app.session.sections, verse);
            let i = insert_section(&mut app.session.sections, Section { name: "Chorus".into(), start, end, mix: None });
            app.edit_section_stem(i, StemKind::Drums, |t| t.muted = true);
            app.edit_section_stem(i, StemKind::Bass, |t| t.gain_db = -8.0);
            app.seek((start + end) / 2.0);
        }
        if std::env::var("STAGEWEAVE_LYRIC_EDIT").is_ok() {
            app.seek(61.0);
            app.toggle_lyric_edit();
        }
    }

    let Some((path, at)) = app.dev.screenshot.clone() else { return };
    let now = ctx.input(|i| i.time);
    if app.dev.stems_at.is_none() && app.has_tracks() {
        app.dev.stems_at = Some(now);
        if !app.is_playing() {
            app.toggle_play();
        }
        let at = std::env::var("STAGEWEAVE_SEEK").ok().and_then(|v| v.parse().ok()).unwrap_or(5.0);
        app.seek(at);
    }
    let due = match at {
        Some(at) => now >= at,
        None => app.dev.stems_at.is_some_and(|t| now >= t + 4.0),
    };
    if !app.dev.screenshot_requested && due {
        app.dev.screenshot_requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }
    let image = ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(image) = image {
        let [w, h] = image.size;
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in &image.pixels {
            out.extend_from_slice(&[p.r(), p.g(), p.b()]);
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("screenshot failed: {e}");
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    ctx.request_repaint();
}
