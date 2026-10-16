//! Modal dialogs for the library screens.

use egui::{Color32, RichText};

use crate::app::{Dialog, StageWeaveApp};
use crate::theme::*;

use super::project_panel::progress_bar;
use super::widgets::primary_button;

fn modal(ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) -> bool {
    // Dim the app behind the dialog.
    let screen = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("modal_dim")))
        .rect_filled(screen, 0.0, Color32::from_black_alpha(140));
    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::new("library_dialog"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(420.0)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            add(ui);
        });
    open
}

pub fn draw(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if let Some(job) = &app.archive_job {
        let (f, what) = job.progress.lock().unwrap().clone();
        let _ = modal(ctx, job.title, |ui| {
            ui.label(RichText::new(if what.is_empty() { "Preparing…".to_owned() } else { what }).color(TEXT_DIM));
            progress_bar(ui, f);
            ui.label(RichText::new(format!("{:.0}%", f * 100.0)).monospace().color(ACCENT_BRIGHT));
        });
        return;
    }
    let Some(dialog) = app.dialog.take() else { return };
    let mut keep;
    let mut next: Option<Dialog> = None;
    match dialog {
        Dialog::NewProject { mut name } => {
            let mut create = false;
            keep = modal(ctx, "New Project", |ui| {
                ui.label(RichText::new("Name").color(TEXT_DIM));
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                r.request_focus();
                create |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label(
                    RichText::new(format!("Songs you add are copied into {}", app.ws.root().join("…").display()))
                        .size(11.0)
                        .color(TEXT_FAINT),
                );
                ui.add_space(8.0);
                create |= primary_button(ui, "Create Project", !name.trim().is_empty()).clicked();
            });
            if create && !name.trim().is_empty() {
                let id = app.create_project(name.trim());
                if id >= 0 {
                    app.open_project(id);
                }
                keep = false;
            } else {
                next = Some(Dialog::NewProject { name });
            }
        }
        Dialog::RenameProject { id, mut name } => {
            let mut save = false;
            keep = modal(ctx, "Rename Project", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                r.request_focus();
                save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.label(RichText::new("The project's folder in the workspace is renamed too.").size(11.0).color(TEXT_FAINT));
                ui.add_space(8.0);
                save |= primary_button(ui, "Rename", !name.trim().is_empty()).clicked();
            });
            if save && !name.trim().is_empty() {
                app.rename_project(id, &name);
                keep = false;
            } else {
                next = Some(Dialog::RenameProject { id, name });
            }
        }
        Dialog::DeleteProject { id, mut delete_files } => {
            let name = app.projects.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_default();
            let mut confirm = false;
            keep = modal(ctx, "Delete Project", |ui| {
                ui.label(RichText::new(format!("Delete \"{name}\"?")).color(Color32::WHITE));
                ui.label(RichText::new("Its songs, lyrics and mixer settings are removed from StageWeave.").color(TEXT_DIM));
                ui.add_space(6.0);
                ui.checkbox(&mut delete_files, "Also delete its song copies and stems from the workspace");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    confirm = ui.add(egui::Button::new(RichText::new("Delete").color(Color32::WHITE)).fill(DANGER)).clicked();
                });
            });
            if confirm {
                app.delete_project(id, delete_files);
                keep = false;
            } else {
                next = Some(Dialog::DeleteProject { id, delete_files });
            }
        }
        Dialog::ChangeWorkspace { to } => {
            let mut action = 0;
            keep = modal(ctx, "Change Workspace", |ui| {
                ui.label(RichText::new("New workspace folder:").color(TEXT_DIM));
                ui.label(RichText::new(to.display().to_string()).monospace().color(Color32::WHITE));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Your {} project(s) live in {}. Move them (songs and stems) to the new folder? \
                         Files are copied and checked before the old copies are removed.",
                        app.projects.len(),
                        app.ws.root().display()
                    ))
                    .color(TEXT_DIM),
                );
                ui.add_space(8.0);
                if primary_button(ui, "Move My Projects There", true).clicked() {
                    action = 1;
                }
            });
            if action == 1 {
                app.start_workspace_move(to, ctx);
                return;
            }
            next = Some(Dialog::ChangeWorkspace { to });
        }
        Dialog::ReplaceLine { line, t } => {
            let (mut replace, mut both, mut cancel) = (false, false, false);
            let existing = app.session.lyrics.lines.get(line).cloned();
            keep = modal(ctx, "Replace Line?", |ui| {
                if let Some(l) = &existing {
                    ui.label(RichText::new(format!("Line {} · {}", line + 1, timecode(l.start))).size(11.0).color(TEXT_FAINT));
                    let text = if l.text.trim().is_empty() { "(empty line)" } else { l.text.as_str() };
                    ui.label(RichText::new(text).size(14.0).color(TEXT));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("is already playing at {}. Replace it with a new line starting there?", timecode(t)))
                            .color(TEXT_DIM),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    replace = ui.add(egui::Button::new(RichText::new("Replace").color(Color32::WHITE)).fill(ACCENT).shortcut_text("Enter")).clicked();
                    both = ui.button("Keep Both").on_hover_text("Add the new line and shorten the existing one").clicked();
                    cancel = ui.add(egui::Button::new("Cancel").shortcut_text("Esc")).clicked();
                });
            });
            keep &= existing.is_some() && !cancel;
            if keep && replace {
                app.lyric_replace_at(line, t);
                keep = false;
            } else if keep && both {
                app.lyric_add_at(t);
                keep = false;
            }
            next = Some(Dialog::ReplaceLine { line, t });
        }
        Dialog::ExportProject { id, mut include_stems } => {
            let mut go = false;
            let name = app.store.project(id).map(|p| p.name).unwrap_or_default();
            keep = modal(ctx, "Export Project", |ui| {
                ui.label(RichText::new(format!("Save \"{name}\" as a .zip you can open on another computer (File › Import Project, or drop it on the window).")).color(TEXT_DIM));
                ui.add_space(6.0);
                ui.label(RichText::new("Includes the songs, lyrics and timing, mixer settings, notes and lyric backgrounds.").size(11.5).color(TEXT_FAINT));
                ui.add_space(6.0);
                ui.checkbox(&mut include_stems, "Include split stems (much bigger file, but no need to split again)");
                ui.add_space(8.0);
                go = primary_button(ui, "Export…", true).clicked();
            });
            if keep && go {
                app.export_project_zip(id, include_stems, ctx);
                keep = false;
            }
            next = Some(Dialog::ExportProject { id, include_stems });
        }
        Dialog::AlignModel => {
            let (mut download, mut models) = (false, false);
            let model = crate::engine::local_align::model(&app.store);
            keep = modal(ctx, "Download the Alignment Model?", |ui| {
                ui.label(RichText::new(model.name).strong().color(Color32::WHITE));
                ui.label(
                    RichText::new(format!(
                        "Align needs this model once ({}). After that it works offline: your audio and lyrics never leave this computer.",
                        crate::engine::models::human_bytes(model.bytes)
                    ))
                    .color(TEXT_DIM),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new("It understands English and was trained on speech, not singing, so check the result before you use it.")
                        .size(11.5)
                        .color(TEXT_FAINT),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    download = primary_button(ui, "Download & Align", true).clicked();
                    models = ui.button("Open Models…").clicked();
                });
            });
            if keep && download {
                app.dialog = None;
                app.local_align(ctx, true);
                keep = false;
            } else if keep && models {
                app.show_models = true;
                keep = false;
            }
            next = Some(Dialog::AlignModel);
        }
        Dialog::MovingWorkspace { progress, file } => {
            let _ = modal(ctx, "Moving Workspace", |ui| {
                ui.label(RichText::new(format!("Copying {file}")).color(TEXT_DIM));
                progress_bar(ui, progress);
                ui.label(RichText::new(format!("{:.0}%", progress * 100.0)).monospace().color(ACCENT_BRIGHT));
            });
            keep = true; // can't be dismissed while moving
            next = Some(Dialog::MovingWorkspace { progress, file });
        }
    }
    if keep && app.dialog.is_none() {
        app.dialog = next;
    }
}
