//! "Export Lyric Video" dialog: resolution, audio source, progress.

use egui::{Color32, RichText, Ui};

use crate::app::StageWeaveApp;
use crate::engine::ffmpeg::FfmpegStatus;
use crate::export::ExportAudio;
use crate::model::Background;
use crate::theme::*;

use super::project_panel::progress_bar;
use super::widgets::primary_button;

pub fn dialog(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if !app.export.open {
        return;
    }
    if app.current_song_id().is_none() && app.export.job.is_none() {
        app.export.open = false;
        return;
    }
    let screen = ctx.content_rect();
    ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("export_dim")))
        .rect_filled(screen, 0.0, Color32::from_black_alpha(140));
    let mut open = true;
    egui::Window::new("Export Lyric Video")
        .id(egui::Id::new("export_dialog"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(440.0);
            body(app, ui);
        });
    if !open && app.export.job.is_none() {
        app.export.open = false;
        app.export.result = None;
    }
}

fn body(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 8.0;
    if let Some(job) = &app.export.job {
        let p = job.progress();
        ui.label(RichText::new(format!("Saving to {}", job.dest.display())).size(11.5).color(TEXT_FAINT));
        ui.label(RichText::new(&p.stage).color(TEXT_DIM));
        progress_bar(ui, p.fraction);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{:.0}%", p.fraction * 100.0)).monospace().color(ACCENT_BRIGHT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    job.cancel();
                }
            });
        });
        return;
    }
    if let Some(Ok(path)) = &app.export.result {
        let path = path.clone();
        ui.label(RichText::new("Export finished").color(SUCCESS));
        ui.label(RichText::new(path.display().to_string()).size(11.5).color(TEXT_DIM));
        ui.horizontal(|ui| {
            if ui.button("Show in Folder").clicked()
                && let Some(dir) = path.parent()
            {
                super::settings::reveal(dir);
            }
            if ui.button("Export Again").clicked() {
                app.export.result = None;
            }
        });
        return;
    }
    if let Some(Err(e)) = &app.export.result {
        ui.label(RichText::new(e).color(DANGER));
    }

    let lyrics = &app.session.lyrics;
    let title = if lyrics.title.is_empty() { app.current_song().map(|s| s.title.clone()).unwrap_or_default() } else { lyrics.title.clone() };
    ui.label(RichText::new(title).size(15.0).color(Color32::WHITE));
    let bg = match &lyrics.style.background {
        Background::Theme => format!("{} theme", lyrics.style.theme.label()),
        Background::Image { file } => format!("Image: {file}"),
        Background::Video { file } => format!("Looping video: {file}"),
    };
    let lines = lyrics.lines.len();
    ui.label(
        RichText::new(format!("{} · {lines} lyric lines · background: {bg}", crate::theme::timecode(app.session.duration())))
            .size(11.5)
            .color(TEXT_FAINT),
    );
    if lines == 0 {
        ui.label(RichText::new("No lyric lines yet: the video will only show the title card.").size(11.5).color(SOLO_ON));
    }

    egui::Grid::new("export_opts").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
        ui.label(RichText::new("Resolution").color(TEXT_DIM));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut app.export.height, 720, "720p");
            ui.selectable_value(&mut app.export.height, 1080, "1080p");
        });
        ui.end_row();
        ui.label(RichText::new("Frame rate").color(TEXT_DIM));
        ui.horizontal(|ui| {
            ui.selectable_value(&mut app.export.fps, 25, "25");
            ui.selectable_value(&mut app.export.fps, 30, "30");
        });
        ui.end_row();
        ui.label(RichText::new("Audio").color(TEXT_DIM));
        let has_stems = app.session.tracks.iter().any(|t| t.audio.is_some());
        if !has_stems {
            app.export.audio = ExportAudio::Original;
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut app.export.audio, ExportAudio::Original, "Original song");
            ui.add_enabled_ui(has_stems, |ui| {
                ui.selectable_value(&mut app.export.audio, ExportAudio::Mix, "Current mix")
                    .on_hover_text("Your mixer settings: e.g. mute the vocals for a sing-along track")
                    .on_disabled_hover_text("Split the song into stems to export your own mix");
            });
        });
        ui.end_row();
    });

    let status = app.ffmpeg.status();
    let ready = matches!(status, FfmpegStatus::Ready(_));
    match &status {
        FfmpegStatus::Missing => {
            ui.label(
                RichText::new("Exporting needs ffmpeg, the free video encoder. StageWeave will download it once (~30–150 MB).")
                    .size(11.5)
                    .color(TEXT_DIM),
            );
        }
        _ => super::editor::ffmpeg_status(app, ui),
    }
    ui.add_space(4.0);
    let label = match status {
        FfmpegStatus::Ready(_) => "Export MP4…",
        FfmpegStatus::Downloading(_) => "Getting ffmpeg…",
        _ => "Download ffmpeg",
    };
    if primary_button(ui, label, !matches!(status, FfmpegStatus::Downloading(_))).clicked() {
        if ready {
            app.export_lyric_video(ui.ctx());
        } else {
            app.ffmpeg.install(ui.ctx());
        }
    }
}
