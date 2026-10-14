use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::app::{StageWeaveApp, SongStatus};
use crate::engine::queue::JobState;
use crate::engine::models::{Transfer, human_bytes};
use crate::engine::python_env::EnvStatus;
use crate::theme::*;

use super::widgets::{self, Icon, horizontal_meter, icon_button, panel_header, primary_button, section_label};

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.set_min_size(ui.available_size());
    let mut tab = app.project_tab;
    panel_header(ui, &["Setlist", "Song"], &mut tab, |ui| {
        if icon_button(ui, Icon::Import, 22.0, false, "Add songs to the setlist (⌘O)").clicked() {
            app.add_songs_dialog();
        }
    });
    app.project_tab = tab;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            if app.project_tab == 0 {
                setlist(app, ui);
                return;
            }
            if app.is_loading() {
                let what = if app.load_job.is_some() { "Loading song…" } else { "Loading stems…" };
                egui::Frame::NONE.fill(BG_INSET).corner_radius(6).inner_margin(12).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT_BRIGHT));
                        ui.label(RichText::new(what).color(TEXT));
                    });
                    progress_bar(ui, -1.0);
                });
                ui.add_space(10.0);
            }
            if app.session.source.is_none() {
                return;
            }
            media_card(app, ui);
            ui.add_space(10.0);
            separation(app, ui);
            if app.has_tracks() {
                ui.add_space(10.0);
                stem_bin(app, ui);
            }
        });
    });
}

/// Compact song switcher for the editor.
fn setlist(app: &mut StageWeaveApp, ui: &mut Ui) {
    let current = app.current_song_id();
    let songs = app.songs.clone();
    if let Some(p) = app.current_project() {
        ui.label(RichText::new(&p.name).strong().color(Color32::WHITE));
        ui.label(RichText::new(format!("{} songs · PageDown / PageUp to switch", songs.len())).size(10.5).color(TEXT_FAINT));
        ui.add_space(6.0);
    }
    let ctx = ui.ctx().clone();
    for (i, song) in songs.iter().enumerate() {
        let status = app.song_status(song);
        let is_current = current == Some(song.id);
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        let p = ui.painter();
        if is_current {
            p.rect_filled(rect, 6.0, ACCENT.gamma_multiply(0.18));
            p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 1.5, ACCENT_BRIGHT);
        } else if resp.hovered() {
            p.rect_filled(rect, 6.0, BG_WIDGET);
        }
        p.text(pos2(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, format!("{}", i + 1), FontId::monospace(11.0), TEXT_FAINT);
        // Badge first so the title can be clipped before it.
        let badge = super::library::status_badge(p, pos2(rect.right() - 8.0, rect.top() + 14.0), &status);
        let text_rect = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top()), pos2(badge.left() - 6.0, rect.top() + 22.0));
        let sub_rect = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top() + 20.0), pos2(rect.right() - 8.0, rect.bottom()));
        p.with_clip_rect(text_rect).text(pos2(text_rect.left(), rect.top() + 14.0), Align2::LEFT_CENTER, &song.title, FontId::proportional(12.5), if is_current { Color32::WHITE } else { TEXT });
        let sub = [song.song_key.as_str(), &song.bpm.map(|b| format!("{b:.0} BPM")).unwrap_or_default(), &mmss(song.duration)]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        p.with_clip_rect(sub_rect).text(pos2(sub_rect.left(), rect.top() + 30.0), Align2::LEFT_CENTER, sub, FontId::proportional(10.5), TEXT_FAINT);

        if let SongStatus::Splitting { progress, .. } | SongStatus::Importing(progress) = status {
            let bar = Rect::from_min_max(pos2(sub_rect.left(), rect.bottom() - 4.0), pos2(rect.right() - 8.0, rect.bottom() - 2.0));
            super::library::thin_progress(p, bar, progress, ui.input(|i| i.time));
        }
        if resp.clicked() && !is_current {
            app.open_song(song.id, &ctx);
            app.project_tab = 1;
        }
    }
    ui.add_space(8.0);
    if ui.button("Open Song List").clicked() {
        app.leave_song();
    }
}

pub fn dashed_rect(painter: &egui::Painter, rect: Rect, color: Color32) {
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    painter.extend(Shape::dashed_line(&pts, Stroke::new(1.2, color), 6.0, 4.0));
}

fn media_card(app: &mut StageWeaveApp, ui: &mut Ui) {
    let Some(src) = &app.session.source else { return };
    let info = src.info.clone();
    let peaks = src.peaks.clone();
    let title = app.current_song().map_or(info.file_name.clone(), |s| s.title.clone());
    let mut close = false;
    egui::Frame::NONE
        .fill(BG_INSET)
        .corner_radius(6)
        .stroke(Stroke::new(1.0, BORDER))
        .show(ui, |ui| {
            // Thumbnail waveform.
            let (thumb, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
            let painter = ui.painter().with_clip_rect(thumb);
            painter.rect_filled(thumb, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, Color32::from_rgb(20, 34, 52));
            let spp = info.duration / thumb.width() as f64;
            widgets::paint_waveform(&painter, thumb.shrink2(vec2(6.0, 8.0)), &peaks, 0.0, spp, ACCENT_BRIGHT.gamma_multiply(0.85), 1.0);
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(&title).strong().color(Color32::WHITE)).truncate());
                        ui.label(
                            RichText::new(format!("{}  ·  {}  ·  {:.1} kHz", timecode(info.duration), info.format, info.sample_rate as f32 / 1000.0))
                                .size(11.0)
                                .color(TEXT_DIM),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, 20.0, false, "Back to the song list").clicked() {
                            close = true;
                        }
                    });
                });
            });
        });
    if close {
        app.leave_song();
    }
}

/// Thin progress bar; `progress < 0` animates as indeterminate.
pub fn progress_bar(ui: &mut Ui, progress: f32) {
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(bar, 3.0, BG_WIDGET);
    if progress >= 0.0 {
        let mut fill = bar;
        fill.set_right(bar.left() + bar.width() * progress.min(1.0));
        ui.painter().rect_filled(fill, 3.0, ACCENT_BRIGHT);
    } else {
        let t = ui.input(|i| i.time) as f32;
        let w = bar.width() * 0.3;
        let x = bar.left() + ((t * 0.6).fract() * 1.3 - 0.3) * bar.width();
        let seg = egui::Rect::from_min_max(pos2(x.max(bar.left()), bar.top()), pos2((x + w).min(bar.right()), bar.bottom()));
        if seg.width() > 0.0 {
            ui.painter().rect_filled(seg, 3.0, ACCENT_BRIGHT);
        }
        ui.ctx().request_repaint();
    }
}

fn log_view(ui: &mut Ui, id: &str, lines: &[String]) {
    egui::CollapsingHeader::new(RichText::new("Show log").size(11.0).color(TEXT_DIM))
        .id_salt(id)
        .show(ui, |ui| {
            egui::Frame::NONE.fill(Color32::from_rgb(12, 12, 13)).corner_radius(4).inner_margin(6).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for l in lines {
                        ui.add(egui::Label::new(RichText::new(l).monospace().size(10.0).color(TEXT_DIM)).wrap());
                    }
                });
            });
        });
}

/// Shown until the Python separation engine is ready. Returns true when ready.
fn engine_card(app: &mut StageWeaveApp, ui: &mut Ui) -> bool {
    let Some(env) = app.env.clone() else { return true };
    let status = env.status();
    if matches!(status, EnvStatus::Ready(_)) {
        return true;
    }
    egui::Frame::NONE
        .fill(BG_INSET)
        .corner_radius(6)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match status {
                EnvStatus::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT_BRIGHT));
                        ui.label(RichText::new("Checking separation engine…").color(TEXT));
                    });
                }
                EnvStatus::NotInstalled => {
                    ui.label(RichText::new("Install the separation engine").strong().color(Color32::WHITE));
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(
                            "Demucs and UVR models run in a private Python environment. \
                             It's a one-time download of about 2 GB; models download the first time you use them.",
                        )
                        .size(11.5)
                        .color(TEXT_DIM),
                    );
                    ui.add_space(8.0);
                    if primary_button(ui, "Install Engine", true).clicked() {
                        env.install(ui.ctx().clone());
                    }
                }
                EnvStatus::Installing { step, of, text } => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT_BRIGHT));
                        ui.label(RichText::new(format!("Step {step} of {of}")).size(11.0).color(TEXT_FAINT));
                    });
                    ui.label(RichText::new(text).color(TEXT));
                    progress_bar(ui, -1.0);
                    log_view(ui, "install_log", &env.log_tail(200));
                }
                EnvStatus::Broken(err) => {
                    ui.label(RichText::new("Separation engine needs attention").strong().color(DANGER));
                    ui.add(egui::Label::new(RichText::new(err).size(11.5).color(TEXT_DIM)).wrap());
                    ui.add_space(6.0);
                    if primary_button(ui, "Reinstall Engine", true).clicked() {
                        env.install(ui.ctx().clone());
                    }
                    log_view(ui, "broken_log", &env.log_tail(200));
                }
                EnvStatus::Ready(_) => {}
            }
        });
    ui.add_space(8.0);
    false
}

fn separation(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        section_label(ui, "Stem separation");
        if app.model_manager.is_some() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(egui::Label::new(RichText::new("Manage models").size(11.0).color(ACCENT_BRIGHT)).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.show_models = true;
                }
            });
        }
    });
    let ready = engine_card(app, ui);
    let views = app.model_manager.as_ref().map(|mm| mm.views(&app.model_keys())).unwrap_or_default();
    if app.model_manager.as_ref().is_some_and(|mm| mm.any_active()) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
    }
    let Some(song) = app.current_song().cloned() else { return };
    let effective = app.effective_model(&song).key();
    let job = app.queue.state(song.id).cloned();
    let running = job.is_some();
    let mut group = None;
    for i in 0..app.models.len() {
        let model = &app.models[i];
        if group != Some(model.engine) {
            group = Some(model.engine);
            if i > 0 {
                ui.add_space(4.0);
            }
            ui.label(RichText::new(model.engine.label()).size(11.0).color(TEXT_DIM));
        }
        let selected = model.key() == effective;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), if running { Sense::hover() } else { Sense::click() });
        let p = ui.painter().clone();
        let fill = if selected { ACCENT.gamma_multiply(0.14) } else if resp.hovered() { BG_WIDGET } else { BG_INSET };
        p.rect_filled(rect, 6.0, fill);
        p.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, if selected { ACCENT } else { BORDER }),
            egui::StrokeKind::Inside,
        );
        // Radio dot.
        let dot = pos2(rect.left() + 16.0, rect.center().y);
        p.circle_stroke(dot, 6.0, Stroke::new(1.5, if selected { ACCENT_BRIGHT } else { TEXT_FAINT }));
        if selected {
            p.circle_filled(dot, 3.2, ACCENT_BRIGHT);
        }
        p.text(pos2(rect.left() + 30.0, rect.top() + 14.0), Align2::LEFT_CENTER, &model.name, FontId::proportional(13.0), Color32::WHITE);
        p.text(pos2(rect.left() + 30.0, rect.top() + 31.0), Align2::LEFT_CENTER, &model.description, FontId::proportional(11.0), TEXT_DIM);
        // Quality tag (or download state) + stem colour chips.
        let view = views.get(&model.key());
        let (tag, tag_color) = match view {
            Some(v) => match &v.transfer {
                Transfer::Downloading { done, total, .. } => match total.or(v.record.bytes_total) {
                    Some(t) => (format!("{:.0}%", *done as f32 / t.max(1) as f32 * 100.0), ACCENT_BRIGHT),
                    None => ("Downloading".to_owned(), ACCENT_BRIGHT),
                },
                Transfer::Queued => ("Queued".to_owned(), ACCENT_BRIGHT),
                Transfer::Failed(_) => ("Failed".to_owned(), DANGER),
                Transfer::Idle if !v.record.downloaded => (
                    v.record.bytes_total.map_or("Download".to_owned(), human_bytes),
                    TEXT_FAINT,
                ),
                Transfer::Idle => (model.tag.clone(), if model.tag == "Best" { SUCCESS } else { TEXT_DIM }),
            },
            None => (model.tag.clone(), if model.tag == "Best" { SUCCESS } else { TEXT_DIM }),
        };
        if let Some(Transfer::Downloading { done, total, .. }) = view.map(|v| &v.transfer)
            && let Some(t) = total.or(view.and_then(|v| v.record.bytes_total))
        {
            let mut bar = egui::Rect::from_min_max(pos2(rect.left() + 6.0, rect.bottom() - 4.0), pos2(rect.right() - 6.0, rect.bottom() - 2.0));
            p.rect_filled(bar, 1.0, BG_WIDGET);
            bar.set_right(bar.left() + bar.width() * (*done as f32 / t.max(1) as f32).min(1.0));
            p.rect_filled(bar, 1.0, ACCENT_BRIGHT);
        }
        let g = p.layout_no_wrap(tag, FontId::proportional(9.5), tag_color);
        let not_downloaded = view.is_some_and(|v| !v.record.downloaded && v.transfer == Transfer::Idle);
        let icon_w = if not_downloaded { 11.0 } else { 0.0 };
        let pill = egui::Rect::from_min_size(
            pos2(rect.right() - 10.0 - g.size().x - 10.0 - icon_w, rect.top() + 7.0),
            g.size() + vec2(10.0 + icon_w, 4.0),
        );
        p.rect_filled(pill, 8.0, tag_color.gamma_multiply(0.15));
        if not_downloaded {
            let ir = egui::Rect::from_center_size(pos2(pill.left() + 9.0, pill.center().y), vec2(9.0, 9.0));
            super::widgets::paint_icon(&p, ir, Icon::Import, tag_color);
        }
        p.galley(pill.min + vec2(5.0 + icon_w, 2.0), g, tag_color);
        for (k, kind) in model.stems.iter().rev().enumerate() {
            let c = pos2(rect.right() - 14.0 - k as f32 * 11.0, rect.bottom() - 13.0);
            p.circle_filled(c, 4.0, kind.color());
        }
        if resp.clicked() && !selected {
            let key = app.models[i].key();
            app.set_song_model(song.id, &key);
            // Switch to that model's stems if they already exist.
            app.reload_stems(&ui.ctx().clone());
        }
        ui.add_space(2.0);
    }
    ui.add_space(6.0);

    if let Some(job) = job {
        let (progress, stage, elapsed) = match job {
            JobState::Queued => (-1.0, "Queued behind other songs".to_owned(), 0),
            JobState::Running { progress, stage, started } => (progress, stage, started.elapsed().as_secs()),
        };
        egui::Frame::NONE.fill(BG_INSET).corner_radius(6).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0).color(ACCENT_BRIGHT));
                // Percentage first (right-aligned) so the stage text truncates instead of overlapping it.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pct = if progress >= 0.0 { format!("{:.0}%", progress * 100.0) } else { String::new() };
                    ui.label(RichText::new(pct).monospace().color(ACCENT_BRIGHT));
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&stage).color(TEXT)).truncate())
                            .on_hover_text(&stage);
                    });
                });
            });
            progress_bar(ui, progress);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    app.queue.cancel(song.id);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("{}:{:02}", elapsed / 60, elapsed % 60)).monospace().size(11.0).color(TEXT_FAINT));
                });
            });
        });
    } else {
        let needs_download = views.get(&effective).filter(|v| !v.record.downloaded);
        let status = app.song_status(&song);
        let label = match (needs_download, &status) {
            (_, SongStatus::Split(_)) => "Split Again".to_owned(),
            (Some(v), _) => match v.record.bytes_total {
                Some(t) => format!("Download ({}) & Separate", human_bytes(t)),
                None => "Download & Separate".to_owned(),
            },
            (None, _) => "Separate Stems".to_owned(),
        };
        let enabled = ready && !app.is_loading();
        if primary_button(ui, &label, enabled).clicked() {
            if let SongStatus::Split(_) = status {
                // Re-run: remove the old stems first so the result isn't treated as cached.
                if let Some(p) = app.current_project().cloned() {
                    let _ = std::fs::remove_dir_all(app.ws.stem_dir(&p, &song, &effective));
                }
            }
            app.split_song(song.id);
        }
        if let Some(err) = app.last_split_error.clone() {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Last run failed: {err}")).size(11.0).color(DANGER));
                if let Some(ps) = &app.python_splitter
                    && ui.small_button("Details").clicked()
                {
                    app.error_details = Some(ps.last_error_log.lock().unwrap().clone());
                }
            });
        }
    }
}

fn stem_bin(app: &mut StageWeaveApp, ui: &mut Ui) {
    section_label(ui, &format!("Stems · {}", app.session.tracks.len()));
    let dur = app.session.duration();
    for i in 0..app.session.tracks.len() {
        let audible = app.session.is_audible(i);
        let selected = app.selected_track == Some(i);
        let t = &app.session.tracks[i];
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
        let p = ui.painter();
        if selected {
            p.rect_filled(rect, 5.0, ACCENT.gamma_multiply(0.16));
        } else if resp.hovered() {
            p.rect_filled(rect, 5.0, BG_WIDGET);
        }
        let icon_r = Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(24.0, 24.0));
        p.rect_filled(icon_r, 5.0, t.color.gamma_multiply(if audible { 0.9 } else { 0.3 }));
        p.text(icon_r.center(), Align2::CENTER_CENTER, t.kind.icon(), FontId::proportional(13.0), Color32::BLACK);
        let text_c = if audible { Color32::WHITE } else { TEXT_FAINT };
        p.text(pos2(icon_r.right() + 10.0, rect.top() + 12.0), Align2::LEFT_CENTER, &t.name, FontId::proportional(12.5), text_c);
        let sub = if let Some(path) = &t.path {
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            format!("{}  ·  {}", timecode(dur), if t.muted { "muted" } else if t.solo { "solo" } else { "stem" })
        };
        p.text(pos2(icon_r.right() + 10.0, rect.top() + 26.0), Align2::LEFT_CENTER, sub, FontId::proportional(10.5), TEXT_FAINT);
        horizontal_meter(
            p,
            Rect::from_min_size(pos2(rect.right() - 54.0, rect.center().y - 2.0), vec2(46.0, 4.0)),
            t.meter,
        );
        if resp.clicked() {
            app.selected_track = Some(i);
            app.editor_tab = 1;
        }
    }
}

/// "3:56"
pub fn mmss(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}
