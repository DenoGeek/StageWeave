//! Window → Models…: what's downloaded, sizes, live download progress.

use egui::{Align, Color32, Layout, RichText, Sense, Stroke, Ui, vec2};

use crate::app::MusicKitApp;
use crate::engine::models::{ModelView, Transfer, human_bytes, transfer_label};
use crate::engine::python_env::EnvStatus;
use crate::theme::*;

use super::project_panel::progress_bar;
use super::widgets::section_label;

pub fn draw(app: &mut MusicKitApp, ctx: &egui::Context) {
    if !app.show_models {
        return;
    }
    let mut open = true;
    egui::Window::new("Models")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([680.0, 560.0])
        .min_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| body(app, ui));
    app.show_models = open;
}

fn body(app: &mut MusicKitApp, ui: &mut Ui) {
    let (Some(mm), Some(env)) = (app.model_manager.clone(), app.env.clone()) else {
        ui.label(RichText::new("Demo mode uses a mock engine with no models.").color(TEXT_DIM));
        return;
    };
    let ready = matches!(env.status(), EnvStatus::Ready(_));
    let keys = app.model_keys();
    let views = mm.views(&keys);
    if mm.any_active() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    // Summary header.
    let downloaded = views.values().filter(|v| v.is_downloaded()).count();
    let on_disk: u64 = views.values().map(|v| v.record.bytes_on_disk).sum();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Separation models").size(15.0).color(Color32::WHITE));
        ui.label(
            RichText::new(format!("{downloaded} of {} downloaded · {} on disk", keys.len(), human_bytes(on_disk)))
                .size(11.5)
                .color(TEXT_DIM),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Show Folder").clicked() {
                super::settings::reveal(&env.paths.models);
            }
            let refreshing = mm.is_refreshing();
            if ui.add_enabled(ready && !refreshing, egui::Button::new("Refresh").small()).clicked() {
                mm.refresh(ui.ctx(), keys.clone(), true);
            }
            if refreshing {
                ui.add(egui::Spinner::new().size(12.0).color(ACCENT_BRIGHT));
            }
        });
    });
    ui.label(
        RichText::new("Models download once and are reused. Downloads resume if interrupted.")
            .size(11.0)
            .color(TEXT_FAINT),
    );
    if !ready {
        ui.add_space(6.0);
        ui.label(RichText::new("Install the separation engine first (Song panel).").color(SOLO_ON));
    }
    ui.add_space(4.0);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut group = None;
        for model in app.models.clone() {
            if group != Some(model.engine) {
                group = Some(model.engine);
                section_label(ui, model.engine.label());
            }
            let key = model.key();
            let Some(view) = views.get(&key) else { continue };
            let avg = mm.store.separation_seconds(&key);
            let ctx = ui.ctx().clone();
            model_row(ui, &model, view, avg, ready, |action| match action {
                Action::Download => mm.download(&ctx, &key),
                Action::Cancel => mm.cancel(&key),
                Action::Delete => mm.delete(&ctx, &key),
            });
            ui.add_space(4.0);
        }

        let recent = mm.store.recent_separations(8);
        if !recent.is_empty() {
            section_label(ui, "Recent separations");
            for r in recent {
                let name = app.models.iter().find(|m| m.key() == r.model_key).map_or(r.model_key.clone(), |m| m.name.clone());
                let song = std::path::Path::new(&r.source_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.add_sized([180.0, 16.0], egui::Label::new(RichText::new(song).color(TEXT)).truncate());
                    ui.label(RichText::new(name).color(TEXT_DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(ago(r.created_at)).size(11.0).color(TEXT_FAINT));
                        ui.label(RichText::new(format!("{:.0} s", r.seconds)).monospace().size(11.0).color(TEXT_FAINT));
                    });
                });
            }
        }
    });
}

enum Action {
    Download,
    Cancel,
    Delete,
}

fn model_row(
    ui: &mut Ui,
    model: &crate::engine::SplitModel,
    view: &ModelView,
    avg_seconds: Option<f64>,
    ready: bool,
    mut act: impl FnMut(Action),
) {
    let rec = &view.record;
    egui::Frame::NONE
        .fill(BG_INSET)
        .corner_radius(6)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Left: identity.
                ui.vertical(|ui| {
                    ui.set_width(220.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&model.name).strong().color(Color32::WHITE));
                        let tag_color = if model.tag == "Best" { SUCCESS } else { TEXT_DIM };
                        ui.label(RichText::new(&model.tag).size(10.0).color(tag_color));
                    });
                    ui.label(RichText::new(&model.description).size(11.0).color(TEXT_DIM));
                    let (r, _) = ui.allocate_exact_size(vec2(12.0 * model.stems.len() as f32, 10.0), Sense::hover());
                    for (i, k) in model.stems.iter().enumerate() {
                        ui.painter().circle_filled(egui::pos2(r.left() + 5.0 + i as f32 * 12.0, r.center().y), 4.0, k.color());
                    }
                });

                // Right: actions.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match &view.transfer {
                        Transfer::Queued | Transfer::Downloading { .. } => {
                            if ui.button("Cancel").clicked() {
                                act(Action::Cancel);
                            }
                        }
                        _ if rec.downloaded => {
                            if ui
                                .button(RichText::new("Delete").color(TEXT_DIM))
                                .on_hover_text("Remove the model files. It will download again when needed.")
                                .clicked()
                            {
                                act(Action::Delete);
                            }
                        }
                        Transfer::Failed(_) => {
                            if ui.add_enabled(ready, egui::Button::new("Retry")).clicked() {
                                act(Action::Download);
                            }
                        }
                        _ => {
                            let label = if rec.bytes_on_disk > 0 { "Resume" } else { "Download" };
                            let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT);
                            if ui.add_enabled(ready, btn).clicked() {
                                act(Action::Download);
                            }
                        }
                    }

                    // Middle: status.
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.set_width(ui.available_width() - 8.0);
                        status(ui, view, avg_seconds);
                    });
                });
            });
        });
}

fn status(ui: &mut Ui, view: &ModelView, avg_seconds: Option<f64>) {
    let rec = &view.record;
    let size = rec.bytes_total.map(human_bytes);
    match &view.transfer {
        Transfer::Downloading { done, total, bytes_per_sec, file } => {
            let total = total.or(rec.bytes_total);
            let frac = total.map_or(-1.0, |t| *done as f32 / t.max(1) as f32);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Downloading").color(ACCENT_BRIGHT));
                if frac >= 0.0 {
                    ui.label(RichText::new(format!("{:.0}%", frac * 100.0)).monospace().color(ACCENT_BRIGHT));
                }
            });
            progress_bar(ui, frac);
            ui.label(RichText::new(transfer_label(*done, total, *bytes_per_sec)).size(10.5).monospace().color(TEXT_DIM));
            if !file.is_empty() {
                ui.add(egui::Label::new(RichText::new(file).size(10.0).color(TEXT_FAINT)).truncate());
            }
        }
        Transfer::Queued => {
            ui.label(RichText::new("Queued").color(TEXT_DIM));
            progress_bar(ui, -1.0);
        }
        Transfer::Failed(e) => {
            ui.label(RichText::new("Download failed").color(DANGER));
            ui.add(egui::Label::new(RichText::new(e).size(10.5).color(TEXT_DIM)).wrap());
        }
        Transfer::Idle if rec.downloaded => {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                super::widgets::paint_check(ui.painter(), r, SUCCESS);
                ui.label(RichText::new(format!("Downloaded · {}", human_bytes(rec.bytes_on_disk))).color(SUCCESS));
            });
            let mut usage = if rec.use_count > 0 {
                format!("Used {}× · last {}", rec.use_count, rec.last_used_at.map(ago).unwrap_or_default())
            } else {
                "Not used yet".to_owned()
            };
            if let Some(s) = avg_seconds {
                usage.push_str(&format!(" · ~{s:.0} s per song"));
            }
            ui.label(RichText::new(usage).size(10.5).color(TEXT_FAINT));
        }
        Transfer::Idle => {
            if rec.bytes_on_disk > 0 {
                let of = size.map(|s| format!(" of {s}")).unwrap_or_default();
                ui.label(RichText::new(format!("Partly downloaded · {}{of}", human_bytes(rec.bytes_on_disk))).color(SOLO_ON));
            } else {
                ui.label(RichText::new("Not downloaded").color(TEXT_DIM));
                ui.label(
                    RichText::new(size.map_or("Size: checking…".to_owned(), |s| format!("Download size {s}")))
                        .size(10.5)
                        .color(TEXT_FAINT),
                );
            }
            if let Some(e) = &rec.error {
                ui.add(egui::Label::new(RichText::new(format!("Last error: {e}")).size(10.0).color(DANGER)).truncate());
            }
        }
    }
}

pub fn ago(ts: i64) -> String {
    let secs = (crate::store::now() - ts).max(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}
