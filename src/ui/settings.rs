//! Settings window and the split error-details window.

use std::path::Path;
use std::time::Instant;

use egui::{Color32, RichText, Ui};

use crate::app::{StageWeaveApp, ToastKind};
use crate::engine::python_env::{DevicePref, EnvStatus};
use crate::theme::*;

use super::widgets::section_label;

pub fn draw(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| body(app, ui));
    app.show_settings = open;
}

fn body(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.set_width(460.0);

    section_label(ui, "Workspace");
    let root = app.ws.root();
    ui.label(RichText::new("Songs you add to projects are copied here, and their stems are saved here.").size(11.5).color(TEXT_DIM));
    ui.horizontal(|ui| {
        let color = if app.ws.exists() { TEXT } else { DANGER };
        ui.add(egui::Label::new(RichText::new(root.display().to_string()).monospace().color(color)).truncate());
    });
    let ws_size = cached_sizes(ui, &[&root])[0];
    ui.horizontal(|ui| {
        ui.label(RichText::new(human_size(ws_size)).color(TEXT_FAINT));
        if ui.small_button("Show").clicked() {
            reveal(&root);
        }
        let moving = matches!(app.dialog, Some(crate::app::Dialog::MovingWorkspace { .. }));
        if ui.add_enabled(!moving, egui::Button::new("Change…").small()).clicked() {
            app.choose_workspace();
        }
        if !app.ws.exists() && ui.small_button("Locate…").clicked() {
            app.locate_workspace();
        }
    });

    ai_section(app, ui);

    section_label(ui, "Audio output");
    ui.label(RichText::new(app.playback.describe()).color(TEXT));

    let Some(env) = app.env.clone() else {
        section_label(ui, "Separation engine");
        ui.label(RichText::new("Demo mode uses a mock engine.").color(TEXT_DIM));
        credits(ui);
        return;
    };

    section_label(ui, "Separation engine");
    match env.status() {
        EnvStatus::Ready(info) => {
            egui::Grid::new("engine_info").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Device", info.device.clone().unwrap_or_default().to_uppercase()),
                    ("Demucs", info.demucs.clone().unwrap_or_default()),
                    ("audio-separator (UVR)", info.audio_separator.clone().unwrap_or_default()),
                    ("PyTorch", info.torch.clone().unwrap_or_default()),
                    ("Python", info.python.clone().unwrap_or_default()),
                ] {
                    ui.label(RichText::new(k).color(TEXT_DIM));
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                }
            });
        }
        EnvStatus::Checking => {
            ui.label(RichText::new("Checking…").color(TEXT_DIM));
        }
        EnvStatus::NotInstalled => {
            ui.label(RichText::new("Not installed").color(SOLO_ON));
        }
        EnvStatus::Installing { step, of, text } => {
            ui.label(RichText::new(format!("Installing ({step}/{of}): {text}")).color(ACCENT_BRIGHT));
        }
        EnvStatus::Broken(e) => {
            ui.label(RichText::new(e).color(DANGER));
        }
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Run separation on").color(TEXT_DIM));
        let mut d = env.device();
        ui.selectable_value(&mut d, DevicePref::Auto, "Auto");
        ui.selectable_value(&mut d, DevicePref::Gpu, "GPU");
        ui.selectable_value(&mut d, DevicePref::Cpu, "CPU");
        env.set_device(d);
    });
    ui.label(
        RichText::new("Auto picks CUDA on NVIDIA, Metal (MPS) on Apple Silicon, otherwise CPU.")
            .size(11.0)
            .color(TEXT_FAINT),
    );

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let busy = env.is_busy() || !app.queue.is_idle();
        if ui.add_enabled(!busy, egui::Button::new("Reinstall Engine")).clicked() {
            env.install(ui.ctx().clone());
            app.show_settings = false;
        }
        if ui.add_enabled(!busy, egui::Button::new("Check Again")).clicked() {
            env.check(ui.ctx().clone());
        }
        if ui.add_enabled(!busy, egui::Button::new("Remove Engine")).clicked() {
            match env.uninstall() {
                Ok(()) => app.toast(ToastKind::Info, "Engine removed. Models and stems were kept."),
                Err(e) => app.toast(ToastKind::Error, e),
            }
        }
    });

    section_label(ui, "Storage");
    let paths = env.paths.clone();
    let sizes = cached_sizes(ui, &[&paths.venv, &paths.models, &paths.stem_cache]);
    folder_row(ui, "Engine", &paths.venv, sizes[0]);
    folder_row(ui, "Models", &paths.models, sizes[1]);
    if sizes[2] > 0 {
        folder_row(ui, "Old stem cache", &paths.stem_cache, sizes[2]);
        if ui
            .add_enabled(app.queue.is_idle(), egui::Button::new("Clear Old Stem Cache"))
            .on_hover_text("Stems from before projects existed. Project stems live in the workspace.")
            .clicked()
        {
            let _ = std::fs::remove_dir_all(&paths.stem_cache);
            ui.data_mut(|d| d.remove::<SizeCache>(size_cache_id(&paths.stem_cache)));
            app.toast(ToastKind::Info, "Old stem cache cleared");
        }
    }

    credits(ui);
}

/// API keys (private app-data file, or environment) for Whisper transcription.
fn ai_section(app: &mut StageWeaveApp, ui: &mut Ui) {
    use crate::ai::Provider;
    section_label(ui, "AI · lyric transcription");
    ui.label(
        RichText::new("Add a key for either service to transcribe lyrics with Whisper. Keys are kept in a private file in StageWeave's app-data folder (only your user account can read it).")
            .size(11.5)
            .color(TEXT_DIM),
    );
    for p in Provider::ALL {
        let id = egui::Id::new(("ai_key_input", p.label()));
        let status_id = egui::Id::new(("ai_key_status", p.label()));
        let source = app.ai.key_with_source(p).map(|(_, src)| src);
        let has = source.is_some();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized([60.0, 18.0], egui::Label::new(RichText::new(p.label()).strong().color(Color32::WHITE)));
            if has {
                let key = app.ai.key(p).unwrap_or_default();
                let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                let from = if source == Some(crate::ai::KeySource::Environment) { format!("From ${}", p.env_var()) } else { "Key saved".into() };
                ui.label(RichText::new(format!("{from} ····{tail}")).color(SUCCESS));
                if ui.small_button("Test").clicked() {
                    let r = crate::ai::test_key(p, &key);
                    ui.data_mut(|d| d.insert_temp(status_id, r.map(|_| "Key works".to_owned())));
                }
                if source == Some(crate::ai::KeySource::Saved) && ui.small_button("Remove").clicked() {
                    app.ai.remove_key(p);
                    ui.data_mut(|d| d.remove::<Result<String, String>>(status_id));
                }
            } else {
                let mut buf: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
                let r = ui.add(egui::TextEdit::singleline(&mut buf).password(true).hint_text("Paste API key").desired_width(200.0));
                if r.changed() {
                    ui.data_mut(|d| d.insert_temp(id, buf.clone()));
                }
                if ui.add_enabled(!buf.trim().is_empty(), egui::Button::new("Save").small()).clicked() {
                    // Check the key before keeping it, so typos surface immediately.
                    let result = crate::ai::test_key(p, &buf).and_then(|_| app.ai.set_key(p, &buf));
                    if result.is_ok() {
                        ui.data_mut(|d| d.remove::<String>(id));
                    }
                    ui.data_mut(|d| d.insert_temp(status_id, result.map(|_| "Key saved".to_owned())));
                }
                ui.hyperlink_to(RichText::new("Get a key").size(11.0), p.keys_url());
            }
        });
        if let Some(r) = ui.data(|d| d.get_temp::<Result<String, String>>(status_id)) {
            let (text, color) = match r {
                Ok(m) => (m, SUCCESS),
                Err(e) => (e, DANGER),
            };
            ui.label(RichText::new(text).size(11.0).color(color));
        }
    }
    if let Some(p) = app.ai.provider() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Default model").color(TEXT_DIM));
            let current = app.ai.model(p);
            egui::ComboBox::from_id_salt("settings_ai_model").selected_text(format!("{} · {}", p.label(), current.label)).show_ui(ui, |ui| {
                for prov in app.ai.configured() {
                    for m in prov.models() {
                        let on = prov == p && m.id == current.id;
                        if ui.selectable_label(on, format!("{} · {}", prov.label(), m.label)).clicked() {
                            app.ai.set_provider(prov);
                            app.ai.set_model(prov, m.id);
                        }
                    }
                }
            });
        });
    }
}

fn credits(ui: &mut Ui) {
    section_label(ui, "Credits");
    ui.label(RichText::new("Stem separation is powered by open-source research:").size(11.5).color(TEXT_DIM));
    ui.hyperlink_to("Demucs — Alexandre Défossez (MIT)", "https://github.com/adefossez/demucs");
    ui.hyperlink_to(
        "Ultimate Vocal Remover models — Anjok07 & aufr33",
        "https://github.com/Anjok07/ultimatevocalremovergui",
    );
    ui.hyperlink_to("python-audio-separator (MIT)", "https://github.com/nomadkaraoke/python-audio-separator");
}

fn folder_row(ui: &mut Ui, label: &str, path: &Path, size: u64) {
    ui.horizontal(|ui| {
        ui.add_sized([80.0, 18.0], egui::Label::new(RichText::new(label).color(TEXT_DIM)));
        ui.label(RichText::new(human_size(size)).monospace().color(TEXT));
        if ui.small_button("Show").on_hover_text(path.display().to_string()).clicked() {
            reveal(path);
        }
    });
}

#[derive(Clone)]
struct SizeCache {
    at: Instant,
    size: u64,
}

/// Folder sizes, recomputed at most every few seconds.
/// Folder sizes, each cached (per path) for a few seconds.
fn cached_sizes(ui: &Ui, dirs: &[&Path]) -> Vec<u64> {
    dirs.iter()
        .map(|dir| {
            let id = size_cache_id(dir);
            if let Some(c) = ui.data(|d| d.get_temp::<SizeCache>(id))
                && c.at.elapsed().as_secs() < 5
            {
                return c.size;
            }
            let size = dir_size(dir);
            ui.data_mut(|d| d.insert_temp(id, SizeCache { at: Instant::now(), size }));
            size
        })
        .collect()
}

fn size_cache_id(dir: &Path) -> egui::Id {
    egui::Id::new(("folder_size", dir))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b > 0.0 {
        format!("{:.0} KB", b / 1e3)
    } else {
        "empty".into()
    }
}

pub fn reveal(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = crate::engine::python_env::background_command(program).arg(path).spawn();
}

pub fn error_details(app: &mut StageWeaveApp, ctx: &egui::Context) {
    let Some(lines) = app.error_details.clone() else { return };
    let mut open = true;
    egui::Window::new("Separation error")
        .open(&mut open)
        .collapsible(false)
        .default_size([560.0, 320.0])
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            if let Some(e) = &app.last_split_error {
                ui.label(RichText::new(e).color(DANGER));
            }
            ui.add_space(4.0);
            egui::Frame::NONE.fill(Color32::from_rgb(12, 12, 13)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if lines.is_empty() {
                        ui.label(RichText::new("No output was captured.").color(TEXT_FAINT));
                    }
                    for l in &lines {
                        ui.add(egui::Label::new(RichText::new(l).monospace().size(10.5).color(TEXT_DIM)).wrap());
                    }
                });
            });
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(lines.join("\n"));
            }
        });
    if !open {
        app.error_details = None;
    }
}
