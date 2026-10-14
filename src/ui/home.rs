//! Start screen: projects (setlists) grid.

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{Dialog, StageWeaveApp};
use crate::theme::*;

use super::library::{self, short_date};
use super::widgets::{Icon, paint_icon};

/// "Sunday 19 Oct" for the coming Sunday (today if it's Sunday).
pub fn default_project_name() -> String {
    let days = crate::store::now().div_euclid(86_400);
    let weekday = (days + 4).rem_euclid(7); // 0 = Sunday (1970-01-01 was a Thursday)
    let sunday = days + (7 - weekday) % 7;
    let date = library::short_date(sunday * 86_400);
    let mut parts = date.split(' ');
    format!("Sunday {} {}", parts.next().unwrap_or(""), parts.next().unwrap_or(""))
}

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    egui::CentralPanel::no_frame().show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(1180.0);
            let margin = ((ui.available_width() - width) / 2.0).max(24.0);
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(margin as i8, 28)).show(ui, |ui| {
                ui.set_width(width - 48.0);
                header(app, ui);
                ui.add_space(14.0);
                if !app.projects.is_empty() {
                    super::search::search_field(ui, &mut app.home_query, "Filter projects, or find a song in any setlist…", 420.0);
                    ui.add_space(14.0);
                }
                if !app.ws.exists() && !app.projects.is_empty() {
                    missing_workspace(app, ui);
                    ui.add_space(14.0);
                }
                if app.home_query.trim().is_empty() {
                    grid(app, ui, None);
                } else {
                    search_results(app, ui);
                }
            });
        });
    });
}

fn header(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Projects").size(26.0).color(Color32::WHITE));
            ui.label(
                RichText::new("One project per service or event. Add the songs, split them, and you're ready to play.")
                    .size(12.5)
                    .color(TEXT_DIM),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Settings").clicked() {
                app.show_settings = true;
            }
            if ui.button("Import Project…").on_hover_text("Open a project .zip exported from StageWeave (or drop it here)").clicked() {
                app.import_project_zip(ui.ctx());
            }
            if app.model_manager.is_some() && ui.button("Models").clicked() {
                app.show_models = true;
            }
            ui.label(RichText::new(app.ws.root().display().to_string()).size(11.0).color(TEXT_FAINT))
                .on_hover_text("Workspace folder (change it in Settings)");
        });
    });
}

fn missing_workspace(app: &mut StageWeaveApp, ui: &mut Ui) {
    egui::Frame::NONE
        .fill(DANGER.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, DANGER.gamma_multiply(0.6)))
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Workspace folder not found").strong().color(Color32::WHITE));
                    ui.label(
                        RichText::new(format!(
                            "{} isn't available. If it's on an external drive, connect it, or locate the folder.",
                            app.ws.root().display()
                        ))
                        .size(11.5)
                        .color(TEXT_DIM),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Locate Workspace…").clicked() {
                        app.locate_workspace();
                    }
                });
            });
        });
}

const CARD: egui::Vec2 = vec2(270.0, 178.0);

/// Typing in the home search: matching projects, then songs from every setlist.
fn search_results(app: &mut StageWeaveApp, ui: &mut Ui) {
    let query = app.home_query.clone();
    let songs = app.song_index();
    let projects = crate::song_library::search_projects(&app.projects, &songs, &query);
    let hits = crate::song_library::search(&songs, &app.projects, &query, false);
    if projects.is_empty() && hits.is_empty() {
        ui.add_space(20.0);
        ui.label(RichText::new(format!("No projects or songs match “{}”", query.trim())).color(TEXT_DIM));
        return;
    }
    if !projects.is_empty() {
        grid(app, ui, Some(projects));
    }
    if !hits.is_empty() {
        ui.label(RichText::new(format!("SONGS ({})", hits.len())).size(10.5).strong().color(TEXT_FAINT));
        ui.add_space(4.0);
        let ctx = ui.ctx().clone();
        egui::Frame::NONE.fill(BG_PANEL).corner_radius(8).inner_margin(6).show(ui, |ui| {
            for h in hits.iter().take(100) {
                let stems = app.song_split_stems(&h.song);
                if super::search::song_row(ui, h, stems, false, None).on_hover_text("Open this song").clicked() {
                    app.open_song_anywhere(h.song.project_id, h.song.id, &ctx);
                }
            }
        });
    }
}

/// Project cards (`only`: a search's matches, without the "New Project" card).
fn grid(app: &mut StageWeaveApp, ui: &mut Ui, only: Option<Vec<crate::store::ProjectRow>>) {
    let gap = 16.0;
    let cols = ((ui.available_width() + gap) / (CARD.x + gap)).floor().max(1.0) as usize;
    let searching = only.is_some();
    let projects = only.unwrap_or_else(|| app.projects.clone());
    let skip = if searching { 0 } else { 1 };
    let total = projects.len() + skip;
    for row in 0..total.div_ceil(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for col in 0..cols {
                let i = row * cols + col;
                if i == 0 && !searching {
                    new_card(app, ui);
                } else if let Some(p) = projects.get(i - skip) {
                    project_card(app, ui, p);
                }
            }
        });
        ui.add_space(gap);
    }
    if searching {
        return;
    }
    if projects.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("Tip: drop audio files or a folder anywhere on this screen to start a project with them.").color(TEXT_FAINT));
    }
}

fn new_card(app: &mut StageWeaveApp, ui: &mut Ui) {
    let (rect, resp) = ui.allocate_exact_size(CARD, Sense::click());
    let hot = resp.hovered() || ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let p = ui.painter();
    p.rect_filled(rect, 10.0, if hot { ACCENT.gamma_multiply(0.10) } else { BG_PANEL });
    super::project_panel::dashed_rect(p, rect.shrink(1.0), if hot { ACCENT_BRIGHT } else { BORDER_STRONG });
    let c = rect.center() - vec2(0.0, 14.0);
    p.circle_filled(c, 22.0, if hot { ACCENT } else { BG_WIDGET });
    p.line_segment([c - vec2(8.0, 0.0), c + vec2(8.0, 0.0)], Stroke::new(2.4, Color32::WHITE));
    p.line_segment([c - vec2(0.0, 8.0), c + vec2(0.0, 8.0)], Stroke::new(2.4, Color32::WHITE));
    library::centered_text(p, rect.center() + vec2(0.0, 26.0), "New Project", 14.0, Color32::WHITE);
    library::centered_text(p, rect.center() + vec2(0.0, 46.0), "or drop songs here", 11.0, TEXT_FAINT);
    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        app.dialog = Some(Dialog::NewProject { name: default_project_name() });
    }
}

fn project_card(app: &mut StageWeaveApp, ui: &mut Ui, project: &crate::store::ProjectRow) {
    let (rect, resp) = ui.allocate_exact_size(CARD, Sense::click());
    let p = ui.painter();
    let hot = resp.hovered();
    p.rect_filled(rect, 10.0, if hot { BG_PANEL_HEADER } else { BG_PANEL });
    p.rect_stroke(rect, 10.0, Stroke::new(1.0, if hot { BORDER_STRONG } else { BORDER }), egui::StrokeKind::Inside);

    // Artwork: a band of stem-coloured bars seeded by the project id.
    let art = Rect::from_min_size(rect.min, vec2(rect.width(), 84.0));
    p.rect_filled(art, egui::CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, Color32::from_rgb(22, 30, 44));
    let colors = [
        crate::model::StemKind::Vocals.color(),
        crate::model::StemKind::Drums.color(),
        crate::model::StemKind::Bass.color(),
        crate::model::StemKind::Other.color(),
    ];
    let mut seed = project.id as u64 * 2_654_435_761 + 7;
    for b in 0..40 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let h = 10.0 + (seed % 44) as f32;
        let x = art.left() + 14.0 + b as f32 * 6.1;
        if x > art.right() - 14.0 {
            break;
        }
        let c = colors[(b / 10) % 4].gamma_multiply(if hot { 0.95 } else { 0.75 });
        p.line_segment([pos2(x, art.center().y - h / 2.0), pos2(x, art.center().y + h / 2.0)], Stroke::new(3.0, c));
    }

    let text = Rect::from_min_max(pos2(rect.left() + 14.0, art.bottom() + 10.0), rect.max - vec2(14.0, 10.0));
    let tp = p.with_clip_rect(text);
    tp.text(text.left_top(), Align2::LEFT_TOP, &project.name, FontId::proportional(15.0), Color32::WHITE);
    let mins = (project.total_duration / 60.0).round() as u32;
    let meta = format!(
        "{} song{} · {} min",
        project.song_count,
        if project.song_count == 1 { "" } else { "s" },
        mins
    );
    tp.text(text.left_top() + vec2(0.0, 24.0), Align2::LEFT_TOP, meta, FontId::proportional(11.5), TEXT_DIM);
    tp.text(
        text.left_top() + vec2(0.0, 44.0),
        Align2::LEFT_TOP,
        format!("Edited {}", short_date(project.updated_at)),
        FontId::proportional(10.5),
        TEXT_FAINT,
    );
    let chev = Rect::from_center_size(pos2(rect.right() - 22.0, text.top() + 32.0), vec2(14.0, 14.0));
    paint_icon(p, chev, Icon::Play, if hot { ACCENT_BRIGHT } else { TEXT_FAINT });

    let id = project.id;
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            app.open_project(id);
        }
        if ui.button("Rename…").clicked() {
            app.dialog = Some(Dialog::RenameProject { id, name: project.name.clone() });
        }
        if ui.button("Duplicate").clicked() {
            app.duplicate_project(id);
        }
        if ui.button("Show in Finder").clicked() {
            super::settings::reveal(&app.ws.project_dir(project));
        }
        if ui.button("Export as .zip…").clicked() {
            app.dialog = Some(Dialog::ExportProject { id, include_stems: false });
        }
        ui.separator();
        if ui.button(RichText::new("Delete…").color(DANGER)).clicked() {
            app.dialog = Some(Dialog::DeleteProject { id, delete_files: false });
        }
    });
    if resp.clicked() {
        app.open_project(id);
    }
}
