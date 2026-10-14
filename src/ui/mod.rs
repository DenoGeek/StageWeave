//! Top-level layout. Three screens: Home (projects), Project (song list)
//! and Song (the editor below).
//!
//! ```text
//! ┌──────────────────── header (menu · workspaces) ───────────────────┐
//! │ project / │  editor (lyrics │      program monitor            │ M │
//! │  split    │  · inspector)   │      (lyric video)              │ E │
//! │           ├──────────────── transport ─────────────────────── │ T │
//! │           │                 timeline (stems + lyrics)         │ E │
//! └──────────────────────────── status bar ──────────────────────────┘
//! ```

mod ai_panel;
mod dialogs;
mod editor;
mod export_ui;
mod header;
pub mod home;
mod library;
mod lyric_actions;
pub mod lyrics_view;
mod meters;
mod models_window;
mod monitor;
mod overlays;
mod project_panel;
mod project_view;
mod settings;
mod timeline;
mod transport;
pub mod widgets;

use egui::{Panel, Ui};

use crate::app::{EditorLayout, MusicKitApp, Screen};
use crate::theme;
use widgets::panel_frame;

pub fn draw(app: &mut MusicKitApp, ui: &mut Ui) {
    ui.painter().rect_filled(ui.max_rect(), 0.0, theme::BG_APP);

    Panel::top("header")
        .frame(egui::Frame::NONE.fill(theme::BG_APP).inner_margin(egui::Margin::symmetric(8, 0)))
        .exact_size(36.0)
        .show_separator_line(false)
        .show(ui, |ui| header::draw(app, ui));

    Panel::bottom("status")
        .frame(egui::Frame::NONE.fill(theme::BG_APP).inner_margin(egui::Margin::symmetric(10, 0)))
        .exact_size(24.0)
        .show_separator_line(false)
        .show(ui, |ui| header::status_bar(app, ui));

    match app.screen {
        Screen::Home => home::draw(app, ui),
        Screen::Project(_) => project_view::draw(app, ui),
        Screen::Song { .. } => editor_screen(app, ui),
    }

    dialogs::draw(app, ui.ctx());
    ai_panel::dialog(app, ui.ctx());
    export_ui::dialog(app, ui.ctx());
    settings::draw(app, ui.ctx());
    models_window::draw(app, ui.ctx());
    settings::error_details(app, ui.ctx());
    overlays::lyric_window(app, ui.ctx());
    overlays::drop_overlay(app, ui.ctx());
    overlays::toasts(app, ui.ctx());
}

fn editor_screen(app: &mut MusicKitApp, ui: &mut Ui) {
    Panel::right("meters")
        .frame(panel_frame())
        .resizable(false)
        .exact_size(78.0)
        .show_separator_line(false)
        .show(ui, |ui| meters::draw(app, ui));

    Panel::left("project")
        .frame(panel_frame())
        .default_size(290.0)
        .size_range(240.0..=460.0)
        .show_separator_line(false)
        .show(ui, |ui| project_panel::draw(app, ui));

    egui::CentralPanel::no_frame().show(ui, |ui| {
        let editor_visible = app.layout != EditorLayout::Perform;
        Panel::top("upper")
            .frame(egui::Frame::NONE)
            .resizable(true)
            .default_size(if app.layout == EditorLayout::Perform { 520.0 } else { 380.0 })
            .size_range(220.0..=900.0)
            .show_separator_line(false)
            .show(ui, |ui| {
                if editor_visible {
                    Panel::left("editor")
                        .frame(panel_frame())
                        .default_size(if app.layout == EditorLayout::Lyrics { 520.0 } else { 380.0 })
                        .size_range(280.0..=900.0)
                        .show_separator_line(false)
                        .show(ui, |ui| editor::draw(app, ui));
                }
                egui::CentralPanel::no_frame().show(ui, |ui| {
                    panel_frame().show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        monitor::draw(app, ui);
                    });
                });
            });

        Panel::top("transport")
            .frame(panel_frame())
            .exact_size(58.0)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| transport::draw(app, ui));

        egui::CentralPanel::no_frame().show(ui, |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                timeline::draw(app, ui);
            });
        });
    });

}
