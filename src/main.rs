#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod align;
mod app;
mod backgrounds;
mod archive;
mod chunking;
mod dev;
mod download;
mod engine;
mod export;
mod model;
mod store;
mod theme;
mod ui;
mod workspace;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("StageWeave")
            .with_inner_size([1480.0, 920.0])
            .with_min_inner_size([1100.0, 680.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("StageWeave", options, Box::new(|cc| Ok(Box::new(app::StageWeaveApp::new(cc)))))
}
