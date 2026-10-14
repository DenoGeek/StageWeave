//! Project screen: the setlist with split-all, reordering and song details.

use egui::{Align, Align2, Color32, FontId, Id, Layout, Panel, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Dialog, StageWeaveApp, SongStatus};
use crate::store::{SongMeta, SongRow};
use crate::theme::*;

use super::library::{self, paint_thumb, status_badge, thin_progress};
use super::widgets::{Icon, icon_button, panel_frame, primary_button, section_label};

const ROW_H: f32 = 62.0;

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    let Some(project) = app.current_project().cloned() else {
        app.go_home();
        return;
    };
    let songs = app.songs.clone();
    let statuses: Vec<SongStatus> = songs.iter().map(|s| app.song_status(s)).collect();

    Panel::top("project_bar")
        .frame(egui::Frame::NONE.fill(BG_APP).inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 6 }))
        .show_separator_line(false)
        .show(ui, |ui| top_bar(app, ui, &project, &statuses));

    Panel::right("song_details")
        .frame(panel_frame())
        .default_size(330.0)
        .size_range(280.0..=480.0)
        .show_separator_line(false)
        .show(ui, |ui| details(app, ui));

    egui::CentralPanel::no_frame().show(ui, |ui| {
        panel_frame().show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            if songs.is_empty() {
                empty_state(app, ui);
            } else {
                song_list(app, ui, &songs, &statuses);
            }
        });
    });
}

fn top_bar(app: &mut StageWeaveApp, ui: &mut Ui, project: &crate::store::ProjectRow, statuses: &[SongStatus]) {
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new(RichText::new("‹  Projects").color(TEXT_DIM)).frame(false)).clicked() {
            app.go_home();
            return;
        }
        ui.add_space(8.0);
        let title = ui.add(egui::Label::new(RichText::new(&project.name).size(20.0).color(Color32::WHITE)).sense(Sense::click()));
        if title.on_hover_text("Click to rename").clicked() {
            app.dialog = Some(Dialog::RenameProject { id: project.id, name: project.name.clone() });
        }
        let split = statuses.iter().filter(|s| matches!(s, SongStatus::Split(_))).count();
        ui.label(
            RichText::new(format!(
                "{} songs · {} min · {split} of {} split",
                statuses.len(),
                (project.total_duration / 60.0).round(),
                statuses.len()
            ))
            .color(TEXT_FAINT),
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let remaining = statuses.iter().filter(|s| matches!(s, SongStatus::NotSplit)).count();
            let busy = statuses.iter().filter(|s| matches!(s, SongStatus::Queued | SongStatus::Splitting { .. })).count();
            let btn = egui::Button::new(RichText::new(format!("Split All ({remaining})")).color(Color32::WHITE)).fill(ACCENT);
            if ui.add_enabled(remaining > 0 && app_ready(app), btn).on_hover_text("Queue every song that isn't split yet").clicked() {
                let n = app.split_all();
                app.toast(crate::app::ToastKind::Info, format!("Queued {n} song{}", if n == 1 { "" } else { "s" }));
            }
            if busy > 0 && ui.button(format!("Cancel All ({busy})")).clicked() {
                app.queue.cancel_all();
            }
            if ui.button("Add Songs…").clicked() {
                app.add_songs_dialog();
            }
            ui.add_space(10.0);

            // Default model for the project.
            let current = app.model_by_key(&project.default_model).map_or("—".to_owned(), |m| m.name.clone());
            let mut chosen = None;
            egui::ComboBox::from_id_salt("project_default_model")
                .selected_text(current)
                .width(170.0)
                .show_ui(ui, |ui| {
                    for m in &app.models {
                        if ui.selectable_label(m.key() == project.default_model, format!("{} · {}", m.engine.label(), m.name)).clicked() {
                            chosen = Some(m.key());
                        }
                    }
                });
            if let Some(k) = chosen {
                app.set_project_default_model(&k);
            }
            ui.label(RichText::new("Default model").color(TEXT_FAINT));
        });
    });
}

fn app_ready(app: &StageWeaveApp) -> bool {
    app.env.as_ref().is_none_or(|e| e.info().is_some())
}

fn empty_state(app: &mut StageWeaveApp, ui: &mut Ui) {
    let rect = ui.max_rect().shrink(40.0);
    let resp = ui.interact(rect, Id::new("project_empty"), Sense::click());
    let hot = resp.hovered() || ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let p = ui.painter();
    p.rect_filled(rect, 12.0, if hot { ACCENT.gamma_multiply(0.06) } else { BG_INSET });
    super::project_panel::dashed_rect(p, rect.shrink(1.0), if hot { ACCENT_BRIGHT } else { BORDER_STRONG });
    library::paint_glyph(p, rect.center() - vec2(0.0, 40.0), if hot { ACCENT_BRIGHT } else { TEXT_FAINT });
    library::centered_text(p, rect.center() + vec2(0.0, 10.0), "Add this week's songs", 17.0, Color32::WHITE);
    library::centered_text(p, rect.center() + vec2(0.0, 34.0), "Drop audio files or a folder here, or click to browse", 12.0, TEXT_DIM);
    library::centered_text(
        p,
        rect.center() + vec2(0.0, 56.0),
        &format!("Copies go to {}", app.current_project().map(|p| app.ws.project_dir(p).display().to_string()).unwrap_or_default()),
        10.5,
        TEXT_FAINT,
    );
    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        app.add_songs_dialog();
    }
}

#[derive(Clone, Copy)]
struct DragState {
    song: i64,
    grab_offset: f32,
}

fn song_list(app: &mut StageWeaveApp, ui: &mut Ui, songs: &[SongRow], statuses: &[SongStatus]) {
    // Column header.
    let (hdr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let hp = ui.painter();
    hp.rect_filled(hdr, egui::CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, BG_PANEL_HEADER);
    let cols = Columns::new(hdr);
    for (x, label) in [(cols.num, "#"), (cols.title, "Title"), (cols.key, "Key"), (cols.bpm, "BPM"), (cols.dur, "Length"), (cols.model, "Model"), (cols.status, "Status")] {
        hp.text(pos2(x, hdr.center().y), Align2::LEFT_CENTER, label, FontId::proportional(11.0), TEXT_FAINT);
    }

    let drag_id = Id::new("setlist_drag");
    let mut drag: Option<DragState> = ui.data(|d| d.get_temp(drag_id));
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let time = ui.input(|i| i.time);
    let ctx = ui.ctx().clone();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let top = ui.cursor().top();
        let mut drop_index = None;
        for (i, song) in songs.iter().enumerate() {
            let status = &statuses[i];
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let selected = app.selected_song == Some(song.id);
            let dragging_this = drag.is_some_and(|d| d.song == song.id);
            let p = ui.painter().clone();
            let fill = if dragging_this {
                BG_WIDGET.gamma_multiply(0.5)
            } else if selected {
                ACCENT.gamma_multiply(0.14)
            } else if resp.hovered() {
                BG_PANEL_HEADER
            } else if i % 2 == 1 {
                BG_TRACK_LANE_ALT
            } else {
                BG_PANEL
            };
            p.rect_filled(rect, 0.0, fill);
            p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BG_APP));
            let cols = Columns::new(rect);
            let fade = if dragging_this { 0.4 } else { 1.0 };

            // Drag handle.
            let handle = Rect::from_center_size(pos2(cols.handle, rect.center().y), vec2(18.0, 30.0));
            let hresp = ui.interact(handle, Id::new(("drag", song.id)), Sense::drag()).on_hover_cursor(egui::CursorIcon::Grab);
            for dy in [-6.0, 0.0, 6.0] {
                for dx in [-3.0, 3.0] {
                    p.circle_filled(handle.center() + vec2(dx, dy), 1.4, if hresp.hovered() { TEXT } else { TEXT_FAINT });
                }
            }
            if hresp.drag_started()
                && let Some(ptr) = pointer
            {
                drag = Some(DragState { song: song.id, grab_offset: ptr.y - rect.top() });
            }

            p.text(pos2(cols.num, rect.center().y), Align2::LEFT_CENTER, format!("{}", i + 1), FontId::monospace(13.0), TEXT_DIM.gamma_multiply(fade));
            let thumb = Rect::from_min_size(pos2(cols.thumb, rect.center().y - 17.0), vec2(96.0, 34.0));
            paint_thumb(&p, thumb, &song.thumb, ACCENT_BRIGHT.gamma_multiply(0.75 * fade));

            let tclip = Rect::from_min_max(pos2(cols.title, rect.top()), pos2(cols.key - 10.0, rect.bottom()));
            let tp = p.with_clip_rect(tclip);
            tp.text(pos2(cols.title, rect.center().y - 9.0), Align2::LEFT_CENTER, &song.title, FontId::proportional(14.0), Color32::WHITE.gamma_multiply(fade));
            let sub = if song.artist.is_empty() { song.notes.lines().next().unwrap_or("").to_owned() } else { song.artist.clone() };
            tp.text(pos2(cols.title, rect.center().y + 10.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.0), TEXT_FAINT);
            if !song.lyrics_raw.trim().is_empty() {
                // Lyrics indicator.
                tp.circle_filled(pos2(cols.key - 20.0, rect.center().y - 9.0), 3.0, ACCENT_BRIGHT);
            }
            p.text(pos2(cols.key, rect.center().y), Align2::LEFT_CENTER, &song.song_key, FontId::proportional(12.5), TEXT);
            p.text(pos2(cols.bpm, rect.center().y), Align2::LEFT_CENTER, song.bpm.map(|b| format!("{b:.0}")).unwrap_or_default(), FontId::monospace(12.0), TEXT);
            p.text(pos2(cols.dur, rect.center().y), Align2::LEFT_CENTER, if song.imported { super::project_panel::mmss(song.duration) } else { "—".into() }, FontId::monospace(12.0), TEXT_DIM);

            // Per-song model (override) picker.
            let model_rect = Rect::from_min_size(pos2(cols.model, rect.center().y - 11.0), vec2(cols.status - cols.model - 12.0, 22.0));
            let mut picked = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(model_rect), |ui| {
                let effective = app.effective_model(song);
                let label = if song.model_key.is_some() { effective.name.clone() } else { format!("{} (default)", effective.name) };
                egui::ComboBox::from_id_salt(("song_model", song.id))
                    .selected_text(RichText::new(label).size(11.5))
                    .width(model_rect.width())
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(song.model_key.is_none(), "Project default").clicked() {
                            picked = Some(None);
                        }
                        ui.separator();
                        for m in &app.models {
                            if ui.selectable_label(song.model_key.as_deref() == Some(&m.key()), format!("{} · {}", m.engine.label(), m.name)).clicked() {
                                picked = Some(Some(m.key()));
                            }
                        }
                    });
            });
            if let Some(k) = picked {
                let mut meta = meta_of(song);
                meta.model_key = k;
                app.update_song_meta(song.id, &meta);
            }

            // Status + actions.
            let badge = status_badge(&p, pos2(cols.status + 128.0, rect.center().y), status);
            if let SongStatus::Splitting { progress, .. } | SongStatus::Importing(progress) = status {
                let bar = Rect::from_min_max(pos2(cols.status, badge.bottom() + 4.0), pos2(cols.status + 128.0, badge.bottom() + 6.0));
                thin_progress(&p, bar, *progress, time);
            }
            let actions = Rect::from_min_max(pos2(cols.actions, rect.top()), pos2(rect.right() - 10.0, rect.bottom()));
            ui.scope_builder(egui::UiBuilder::new().max_rect(actions).layout(Layout::right_to_left(Align::Center)), |ui| {
                let can_open = song.imported && !matches!(status, SongStatus::Missing);
                if ui.add_enabled(can_open, egui::Button::new(RichText::new("Open").color(Color32::WHITE)).fill(ACCENT)).clicked() {
                    app.open_song(song.id, &ctx);
                }
                match status {
                    SongStatus::NotSplit
                        if icon_button(ui, Icon::Import, 24.0, false, "Split this song").clicked() => {
                            app.split_song(song.id);
                        }
                    SongStatus::Queued | SongStatus::Splitting { .. }
                        if icon_button(ui, Icon::Close, 24.0, false, "Cancel split").clicked() => {
                            app.queue.cancel(song.id);
                        }
                    SongStatus::Missing
                        if ui.small_button("Locate…").clicked() => {
                            app.relocate_song(song.id);
                        }
                    _ => {}
                }
            });

            if resp.clicked() {
                app.selected_song = Some(song.id);
            }
            if resp.double_clicked() && song.imported {
                app.open_song(song.id, &ctx);
            }
            resp.context_menu(|ui| {
                if ui.button("Open").clicked() {
                    app.open_song(song.id, &ctx);
                }
                if ui.button("Split").clicked() {
                    app.split_song(song.id);
                }
                if ui.button("Show in Finder").clicked() {
                    super::settings::reveal(&app.ws.abs(&song.path).parent().map(|p| p.to_path_buf()).unwrap_or_default());
                }
                ui.separator();
                if ui.button(RichText::new("Remove from Project").color(DANGER)).clicked() {
                    app.remove_song(song.id);
                }
            });

            // Where would a dragged row land?
            if let (Some(_), Some(ptr)) = (drag, pointer)
                && ptr.y >= rect.top()
                && ptr.y < rect.bottom()
            {
                drop_index = Some(if ptr.y < rect.center().y { i } else { i + 1 });
            }
        }

        // Insertion line + ghost while dragging.
        if let (Some(d), Some(ptr)) = (drag, pointer) {
            let idx = drop_index.unwrap_or(if ptr.y < top { 0 } else { songs.len() });
            let y = top + idx as f32 * ROW_H;
            let p = ui.painter();
            p.line_segment([pos2(ui.max_rect().left() + 8.0, y), pos2(ui.max_rect().right() - 8.0, y)], Stroke::new(2.5, ACCENT_BRIGHT));
            p.circle_filled(pos2(ui.max_rect().left() + 8.0, y), 4.0, ACCENT_BRIGHT);
            if let Some(song) = songs.iter().find(|s| s.id == d.song) {
                let ghost = Rect::from_min_size(pos2(ui.max_rect().left() + 30.0, ptr.y - d.grab_offset), vec2(320.0, ROW_H - 8.0));
                let gp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("drag_ghost")));
                gp.rect_filled(ghost, 8.0, BG_PANEL_HEADER);
                gp.rect_stroke(ghost, 8.0, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
                gp.text(ghost.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, &song.title, FontId::proportional(14.0), Color32::WHITE);
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);

            if ui.input(|i| i.pointer.any_released()) {
                let from = songs.iter().position(|s| s.id == d.song).unwrap_or(0);
                let to = if idx > from { idx - 1 } else { idx };
                if to != from {
                    app.move_song(d.song, to);
                }
                drag = None;
            }
        }
        ui.add_space(20.0);
    });

    ui.data_mut(|data| match drag {
        Some(d) => {
            data.insert_temp(drag_id, d);
        }
        None => data.remove::<DragState>(drag_id),
    });
}

struct Columns {
    handle: f32,
    num: f32,
    thumb: f32,
    title: f32,
    key: f32,
    bpm: f32,
    dur: f32,
    model: f32,
    status: f32,
    actions: f32,
}

impl Columns {
    fn new(r: Rect) -> Self {
        let right = r.right();
        let actions = right - 110.0;
        let status = actions - 150.0;
        let model = status - 190.0;
        let dur = model - 70.0;
        let bpm = dur - 56.0;
        let key = bpm - 56.0;
        Self { handle: r.left() + 16.0, num: r.left() + 34.0, thumb: r.left() + 58.0, title: r.left() + 168.0, key, bpm, dur, model, status, actions }
    }
}

fn meta_of(song: &SongRow) -> SongMeta {
    SongMeta {
        title: song.title.clone(),
        artist: song.artist.clone(),
        song_key: song.song_key.clone(),
        bpm: song.bpm,
        notes: song.notes.clone(),
        model_key: song.model_key.clone(),
    }
}

fn details(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.set_min_size(ui.available_size());
    let Some(song) = app.selected_song.and_then(|id| app.songs.iter().find(|s| s.id == id).cloned()) else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No song selected").color(TEXT_DIM));
            ui.label(RichText::new("Pick a song to edit its key, tempo and notes.").size(11.5).color(TEXT_FAINT));
        });
        return;
    };
    let status = app.song_status(&song);
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(14).show(ui, |ui| {
            let (thumb, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
            paint_thumb(ui.painter(), thumb, &song.thumb, ACCENT_BRIGHT.gamma_multiply(0.85));
            ui.add_space(8.0);

            let mut meta = meta_of(&song);
            let mut changed = false;
            section_label(ui, "Song");
            egui::Grid::new(("song_meta", song.id)).num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label(RichText::new("Title").color(TEXT_DIM));
                changed |= ui.add(egui::TextEdit::singleline(&mut meta.title).desired_width(f32::INFINITY)).changed();
                ui.end_row();
                ui.label(RichText::new("Artist").color(TEXT_DIM));
                changed |= ui.add(egui::TextEdit::singleline(&mut meta.artist).desired_width(f32::INFINITY)).changed();
                ui.end_row();
                ui.label(RichText::new("Key").color(TEXT_DIM));
                changed |= ui.add(egui::TextEdit::singleline(&mut meta.song_key).hint_text("e.g. G, Bb, F#m").desired_width(90.0)).changed();
                ui.end_row();
                ui.label(RichText::new("BPM").color(TEXT_DIM));
                ui.horizontal(|ui| {
                    let mut bpm = meta.bpm.unwrap_or(0.0);
                    if ui.add(egui::DragValue::new(&mut bpm).range(0.0..=300.0).speed(0.5).max_decimals(1)).changed() {
                        meta.bpm = (bpm > 0.0).then_some(bpm);
                        changed = true;
                    }
                    if meta.bpm.is_some() && ui.small_button("Clear").clicked() {
                        meta.bpm = None;
                        changed = true;
                    }
                });
                ui.end_row();
            });
            section_label(ui, "Notes");
            changed |= ui
                .add(
                    egui::TextEdit::multiline(&mut meta.notes)
                        .hint_text("Capo 2 · start soft · repeat chorus…")
                        .desired_rows(4)
                        .desired_width(f32::INFINITY),
                )
                .changed();
            if changed {
                app.update_song_meta(song.id, &meta);
            }

            section_label(ui, "Stems");
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(150.0, 20.0), Sense::hover());
                status_badge(ui.painter(), pos2(r.left() + 140.0, r.center().y), &status);
            });
            let model = app.effective_model(&song);
            ui.label(RichText::new(format!("{} · {}", model.engine.label(), model.name)).color(TEXT));
            ui.label(RichText::new(model.description.clone()).size(11.0).color(TEXT_FAINT));
            ui.add_space(6.0);
            match status {
                SongStatus::NotSplit
                    if primary_button(ui, "Split This Song", app_ready(app)).clicked() => {
                        app.split_song(song.id);
                    }
                SongStatus::Queued | SongStatus::Splitting { .. }
                    if ui.button("Cancel Split").clicked() => {
                        app.queue.cancel(song.id);
                    }
                _ => {}
            }
            ui.add_space(6.0);
            if primary_button(ui, "Open in Editor", song.imported && !matches!(status, SongStatus::Missing)).clicked() {
                app.open_song(song.id, &ctx);
            }

            section_label(ui, "File");
            let file = app.ws.abs(&song.path);
            ui.add(egui::Label::new(RichText::new(file.display().to_string()).size(10.5).color(TEXT_FAINT)).wrap());
            ui.horizontal(|ui| {
                if ui.small_button("Show in Finder").clicked() {
                    super::settings::reveal(&file.parent().map(|p| p.to_path_buf()).unwrap_or_default());
                }
                if ui.small_button(RichText::new("Remove from Project").color(DANGER)).clicked() {
                    app.remove_song(song.id);
                }
            });
            ui.label(RichText::new(format!("Added from {}", song.original_path)).size(10.0).color(TEXT_FAINT));
        });
    });
}
