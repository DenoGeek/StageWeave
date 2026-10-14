use egui::{Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, Ui, vec2};

use crate::app::{EditorLayout, StageWeaveApp, Screen};
use crate::engine::python_env::EnvStatus;
use crate::theme::*;

use super::widgets::{Icon, icon_button};

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.horizontal_centered(|ui| {
        logo(ui);
        ui.add_space(6.0);
        // MenuBar fills the row it's given, so give it just enough.
        ui.scope_builder(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(ui.cursor().min, vec2(250.0, ui.available_height()))), |ui| {
            menus(app, ui);
        });
        breadcrumb(app, ui);

        let in_song = matches!(app.screen, Screen::Song { .. });
        if in_song {
            // Centered workspace switcher.
            let full = ui.max_rect();
            let switch_w = 270.0;
            let rect = egui::Rect::from_center_size(full.center(), vec2(switch_w, 26.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
            workspace_switcher(app, &mut child);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if search_button(ui).clicked() {
                app.search.open = true;
            }
            if !in_song {
                return;
            }
            let can_pop = app.session.lyrics.is_generated();
            let r = icon_button(
                ui,
                Icon::PopOut,
                26.0,
                app.lyric_window_open,
                if app.lyric_window_open {
                    "Close lyric window (⇧⌘P)"
                } else {
                    "Open lyric video in its own window (⇧⌘P)"
                },
            );
            if r.clicked() && can_pop {
                app.toggle_lyric_window();
            }
        });
    });
}

/// Projects › Sunday 12 Oct › Song title
fn breadcrumb(app: &mut StageWeaveApp, ui: &mut Ui) {
    let crumb = |ui: &mut Ui, text: &str, link: bool| {
        let rt = RichText::new(text).size(12.5).color(if link { TEXT_DIM } else { Color32::WHITE });
        let r = ui.add_sized(
            [ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), FontId::proportional(12.5), Color32::WHITE).size().x).min(200.0), 20.0],
            egui::Label::new(rt).truncate().sense(if link { Sense::click() } else { Sense::hover() }),
        );
        if link { r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() } else { false }
    };
    let sep = |ui: &mut Ui| {
        ui.label(RichText::new("›").color(TEXT_FAINT));
    };
    let project = app.current_project().map(|p| (p.id, p.name.clone()));
    let song = app.current_song().map(|s| s.title.clone());
    if crumb(ui, "Projects", app.screen != Screen::Home) {
        app.go_home();
        return;
    }
    if let Some((id, name)) = project {
        sep(ui);
        if crumb(ui, &name, song.is_some()) {
            app.open_project(id);
            return;
        }
    }
    if let Some(title) = song {
        sep(ui);
        crumb(ui, &title, false);
    }
}

/// "Search  ⌘K" pill that opens the global search.
fn search_button(ui: &mut Ui) -> egui::Response {
    let keys = if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl+K" };
    let (rect, resp) = ui.allocate_exact_size(vec2(150.0, 24.0), Sense::click());
    let resp = resp.on_hover_text("Search all projects, songs and lyrics");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Search"));
    let p = ui.painter();
    p.rect_filled(rect, 6.0, if resp.hovered() { BG_WIDGET } else { BG_INSET });
    p.rect_stroke(rect, 6.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    super::widgets::paint_icon(p, egui::Rect::from_center_size(rect.left_center() + vec2(14.0, 0.0), vec2(13.0, 13.0)), Icon::Search, TEXT_FAINT);
    p.text(rect.left_center() + vec2(28.0, 0.0), Align2::LEFT_CENTER, "Search", FontId::proportional(12.0), TEXT_DIM);
    p.text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, keys, FontId::proportional(10.5), TEXT_FAINT);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, ACCENT);
    // Three little stem bars.
    let c = rect.center();
    for (i, h) in [8.0, 13.0, 6.0].iter().enumerate() {
        let x = c.x - 5.0 + i as f32 * 5.0;
        p.line_segment([egui::pos2(x, c.y - h / 2.0), egui::pos2(x, c.y + h / 2.0)], Stroke::new(2.4, Color32::WHITE));
    }
    ui.label(RichText::new("StageWeave").strong().color(Color32::WHITE).size(13.5));
}

fn menus(app: &mut StageWeaveApp, ui: &mut Ui) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("File", |ui| {
            if ui.button("New Project…").clicked() {
                app.dialog = Some(crate::app::Dialog::NewProject { name: super::home::default_project_name() });
            }
            ui.add_enabled_ui(app.project_id().is_some(), |ui| {
                if ui.add(egui::Button::new("Add Songs…").shortcut_text("⌘O")).clicked() {
                    app.add_songs_dialog();
                }
                if ui.button("Add from Library…").clicked() {
                    super::search::open_library(app);
                }
                if ui.button("Close Project").clicked() {
                    app.go_home();
                }
            });
            if ui.add(egui::Button::new("Search…").shortcut_text(if cfg!(target_os = "macos") { "⌘K" } else { "Ctrl+K" })).clicked() {
                app.search.open = true;
            }
            ui.separator();
            if ui.button("Import Project…").clicked() {
                app.import_project_zip(ui.ctx());
            }
            if let Some(id) = app.project_id()
                && ui.button("Export Project…").clicked()
            {
                app.dialog = Some(crate::app::Dialog::ExportProject { id, include_stems: false });
            }
            ui.separator();
            if ui.add_enabled(app.current_song_id().is_some(), egui::Button::new("Export Lyric Video…")).clicked() {
                app.export.open = true;
            }
            ui.separator();
            if ui.add(egui::Button::new("Settings…").shortcut_text("⌘,")).clicked() {
                app.show_settings = true;
            }
            ui.separator();
            if ui.button("Quit").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.menu_button("Edit", |ui| {
            let fmt = |m: egui::Modifiers, k: egui::Key| ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(m, k));
            let undo_label = app.history.undo_label().map_or("Undo".to_owned(), |l| format!("Undo {l}"));
            let redo_label = app.history.redo_label().map_or("Redo".to_owned(), |l| format!("Redo {l}"));
            let redo_keys = if cfg!(target_os = "macos") {
                fmt(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
            } else {
                fmt(egui::Modifiers::COMMAND, egui::Key::Y)
            };
            if ui
                .add_enabled(app.history.undo_label().is_some(), egui::Button::new(undo_label).shortcut_text(fmt(egui::Modifiers::COMMAND, egui::Key::Z)))
                .clicked()
            {
                app.undo();
            }
            if ui.add_enabled(app.history.redo_label().is_some(), egui::Button::new(redo_label).shortcut_text(redo_keys)).clicked() {
                app.redo();
            }
            ui.separator();
            ui.add_enabled_ui(app.has_tracks(), |ui| {
                if ui.button("Unmute All").clicked() {
                    app.session.tracks.iter_mut().for_each(|t| {
                        t.muted = false;
                        t.solo = false;
                    });
                    app.sync_mix();
                }
                if ui.button("Reset Mixer").clicked() {
                    app.session.tracks.iter_mut().for_each(|t| {
                        t.gain_db = 0.0;
                        t.pan = 0.0;
                        t.muted = false;
                        t.solo = false;
                    });
                    app.sync_mix();
                }
            });
        });
        ui.menu_button("Playback", |ui| {
            ui.add_enabled_ui(app.can_play(), |ui| {
                let label = if app.is_playing() { "Pause" } else { "Play" };
                if ui.add(egui::Button::new(label).shortcut_text("Space")).clicked() {
                    app.toggle_play();
                }
                if ui.add(egui::Button::new("Go to Start").shortcut_text("Home")).clicked() {
                    app.seek(0.0);
                }
                ui.checkbox(&mut app.loop_enabled, "Loop (L)");
                ui.separator();
                let mark = if app.cue.recording.is_some() { "End Cue & Loop" } else { "Mark Cue Start" };
                if ui.add(egui::Button::new(mark).shortcut_text("Q")).clicked() {
                    app.cue_key();
                }
                if ui.add_enabled(app.cue.active.is_some(), egui::Button::new("Exit Cue").shortcut_text("Esc")).clicked() {
                    app.exit_cue();
                }
            });
        });
        ui.menu_button("Window", |ui| {
            if ui.add(egui::Button::new("Models…").shortcut_text("⇧⌘M")).clicked() {
                app.show_models = true;
            }
            ui.separator();
            ui.add_enabled_ui(app.session.lyrics.is_generated(), |ui| {
                let label = if app.lyric_window_open { "Close Lyric Window" } else { "Open Lyric Window" };
                if ui.add(egui::Button::new(label).shortcut_text("⇧⌘P")).clicked() {
                    app.toggle_lyric_window();
                }
            });
            ui.separator();
            for (ws, name) in [(EditorLayout::Edit, "Edit"), (EditorLayout::Lyrics, "Lyrics"), (EditorLayout::Perform, "Perform")] {
                if ui.radio(app.layout == ws, format!("{name} Workspace")).clicked() {
                    set_workspace(app, ws);
                }
            }
        });
        ui.menu_button("Help", |ui| {
            ui.label(RichText::new("Keyboard").strong());
            for (k, d) in [
                ("Space", "Play / pause"),
                ("Home / End", "Jump to start / end"),
                ("← / →", "Nudge 5 seconds"),
                ("L", "Toggle loop"),
                ("N", "New lyric line at the playhead"),
                ("E", "Stretch the previous line to the playhead"),
                ("Q", "Mark a cue start / close it and loop"),
                ("Esc", "Exit the cue"),
                ("Enter", "Stamp line (tap sync)"),
                ("⌘ + scroll", "Zoom timeline"),
                ("⇧⌘P", "Lyric window"),
                ("F / double-click", "Fullscreen (lyric window)"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(k).monospace().color(ACCENT_BRIGHT));
                    ui.label(RichText::new(d).color(TEXT_DIM));
                });
            }
        });
    });
}

pub fn set_workspace(app: &mut StageWeaveApp, ws: EditorLayout) {
    app.layout = ws;
    match ws {
        EditorLayout::Edit => app.editor_tab = 1,
        EditorLayout::Lyrics => app.editor_tab = 0,
        EditorLayout::Perform => {
            if app.session.lyrics.is_generated() && !app.lyric_window_open {
                app.lyric_window_open = true;
            }
        }
    }
}

fn workspace_switcher(app: &mut StageWeaveApp, ui: &mut Ui) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 6.0, BG_PANEL);
    let items = [(EditorLayout::Edit, "Edit"), (EditorLayout::Lyrics, "Lyrics"), (EditorLayout::Perform, "Perform")];
    let w = rect.width() / items.len() as f32;
    for (i, (ws, label)) in items.into_iter().enumerate() {
        let r = egui::Rect::from_min_size(rect.min + vec2(i as f32 * w, 0.0), vec2(w, rect.height())).shrink(2.0);
        let resp = ui.interact(r, ui.id().with(("ws", i)), Sense::click());
        let active = app.layout == ws;
        if active {
            ui.painter().rect_filled(r, 5.0, BG_WIDGET_HOVER);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 5.0, BG_WIDGET);
        }
        let color = if active { Color32::WHITE } else { TEXT_DIM };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.5), color);
        if active {
            ui.painter().rect_filled(
                egui::Rect::from_center_size(egui::pos2(r.center().x, r.bottom() - 1.5), vec2(18.0, 2.0)),
                1.0,
                ACCENT_BRIGHT,
            );
        }
        if resp.clicked() {
            set_workspace(app, ws);
        }
    }
}

pub fn status_bar(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.horizontal_centered(|ui| {
        let small = |s: String| RichText::new(s).size(11.0).color(TEXT_FAINT);
        let importing = app.importer.progress.len();
        let (dot, text) = if let Some(s) = app.queue.summary() {
            (ACCENT_BRIGHT, s)
        } else if importing > 0 {
            (ACCENT_BRIGHT, format!("Copying {importing} song{} into the workspace…", if importing == 1 { "" } else { "s" }))
        } else if app.has_tracks() {
            (SUCCESS, format!("{} stems ready", app.session.tracks.len()))
        } else if app.session.source.is_some() {
            (SOLO_ON, "Ready to separate".to_owned())
        } else {
            match app.screen {
                Screen::Home => (TEXT_FAINT, format!("{} projects", app.projects.len())),
                _ => (TEXT_FAINT, "Ready".to_owned()),
            }
        };
        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(r.center(), 3.5, dot);
        ui.label(RichText::new(text).size(11.0).color(TEXT_DIM));

        if let Some(i) = app.tap_sync {
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "TAP SYNC · press Enter at the start of line {} of {}  ·  Esc to stop",
                    i + 1,
                    app.session.lyrics.lines.len()
                ))
                .size(11.0)
                .color(SOLO_ON),
            );
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let engine = match app.env.as_ref().map(|e| e.status()) {
                None => "Engine: preview (demo)".to_owned(),
                Some(EnvStatus::Ready(info)) => info.summary(),
                Some(EnvStatus::Checking) => "Checking engine…".to_owned(),
                Some(EnvStatus::NotInstalled) => "Engine not installed".to_owned(),
                Some(EnvStatus::Installing { step, of, .. }) => format!("Installing engine {step}/{of}…"),
                Some(EnvStatus::Broken(_)) => "Engine needs attention".to_owned(),
            };
            if ui.add(egui::Label::new(small(engine)).sense(Sense::click())).on_hover_text("Open settings").clicked() {
                app.show_settings = true;
            }
            if let Some(src) = &app.session.source {
                ui.separator();
                ui.label(small(format!(
                    "{} · {:.1} kHz · {}",
                    src.info.format,
                    src.info.sample_rate as f32 / 1000.0,
                    if src.info.channels == 2 { "Stereo" } else { "Mono" }
                )));
            }
            if app.lyric_window_open {
                ui.separator();
                ui.label(RichText::new("● LYRIC WINDOW LIVE").size(10.5).color(DANGER));
            }
        });
    });
}
