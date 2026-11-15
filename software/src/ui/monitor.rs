//! Program monitor: live preview of the lyric video, with an edit mode for
//! fixing lines and timing. (The pop-out lyric window is display-only.)

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::app::StageWeaveApp;
use crate::theme::*;

use super::lyrics_view;
use super::widgets::{Icon, icon_button, panel_header};

const EDIT_BAR_H: f32 = 118.0;
const EDIT_COLOR: Color32 = Color32::from_rgb(255, 196, 70);

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    let generated = app.can_project();
    let can_edit = app.session.source.is_some();
    let mut tab = app.monitor_tab;
    let mut toggle_edit = false;
    panel_header(ui, &["Program: Lyric Video"], &mut tab, |ui| {
        if generated {
            if icon_button(ui, Icon::Fullscreen, 22.0, app.lyric_window_fullscreen, "Open fullscreen on the lyric window").clicked() {
                app.lyric_window_open = true;
                app.lyric_window_fullscreen = true;
            }
            if icon_button(
                ui,
                Icon::PopOut,
                22.0,
                app.lyric_window_open,
                if app.lyric_window_open { "Close lyric window" } else { "Pop out to a window you can drag to another screen" },
            )
            .clicked()
            {
                app.toggle_lyric_window();
            }
        }
        if can_edit {
            let on = app.lyric_edit.on;
            let label = RichText::new(if on { "Done" } else { "Edit" }).color(if on { Color32::BLACK } else { TEXT });
            let btn = egui::Button::new(label).fill(if on { EDIT_COLOR } else { BG_WIDGET }).min_size(vec2(54.0, 20.0));
            let tip = if on {
                "Leave edit mode"
            } else {
                "Edit lines and timing here. If the song is playing, it pauses and marks the start of the current line."
            };
            if ui.add(btn).on_hover_text(tip).clicked() {
                toggle_edit = true;
            }
        }
    });
    app.monitor_tab = tab;
    if toggle_edit {
        app.toggle_lyric_edit();
    }
    let editing = app.lyric_edit.on && can_edit;

    let mut area = ui.available_rect_before_wrap().shrink(10.0);
    if editing {
        area.set_bottom(area.bottom() - EDIT_BAR_H - 8.0);
    }
    let resp = ui.interact(area, ui.id().with("monitor_frame"), Sense::click());
    let frame = lyrics_view::fit_16_9(area);
    let painter = ui.painter().clone();

    if !generated && !editing {
        ui.allocate_rect(ui.available_rect_before_wrap(), Sense::hover());
        painter.rect_filled(frame, 4.0, Color32::from_rgb(8, 8, 9));
        painter.rect_stroke(frame, 4.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
        let (title, body) = if app.session.source.is_none() {
            ("No media", "Import a song to get started")
        } else {
            ("No lyric video yet", "Generate from the Lyrics panel, transcribe with AI, or click Edit to build lines while it plays")
        };
        painter.text(frame.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, title, FontId::proportional(15.0), TEXT_DIM);
        painter.text(frame.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, body, FontId::proportional(12.0), TEXT_FAINT);
        return;
    }

    let t = app.position();
    let anim = ui.input(|i| i.time);
    let backdrop = app.lyric_backdrop(ui.ctx());
    lyrics_view::paint(&painter, frame, &app.session.lyrics, t, app.session.duration(), anim, backdrop);
    if app.session.lyrics.style.animated_background {
        ui.ctx().request_repaint();
    }

    if app.lyric_window_open {
        let badge = Rect::from_min_size(frame.right_top() + vec2(-150.0, 10.0), vec2(140.0, 22.0));
        painter.rect_filled(badge, 11.0, Color32::from_black_alpha(170));
        painter.circle_filled(badge.left_center() + vec2(13.0, 0.0), 4.0, DANGER);
        painter.text(badge.left_center() + vec2(24.0, 0.0), Align2::LEFT_CENTER, "LIVE IN LYRIC WINDOW", FontId::proportional(10.0), Color32::WHITE);
    }

    if editing {
        painter.rect_stroke(frame, 4.0, Stroke::new(2.0, EDIT_COLOR), egui::StrokeKind::Outside);
        let tag = Rect::from_min_size(frame.left_top() + vec2(10.0, 10.0), vec2(74.0, 20.0));
        painter.rect_filled(tag, 10.0, EDIT_COLOR);
        painter.text(tag.center(), Align2::CENTER_CENTER, "EDITING", FontId::proportional(10.5), Color32::BLACK);
        if resp.clicked() {
            // Click the preview to pick the line under the playhead.
            app.lyric_edit.sel = app.session.lyrics.line_for_edit(t);
        }
        let bar = Rect::from_min_max(pos2(area.left(), area.bottom() + 8.0), pos2(area.right(), area.bottom() + 8.0 + EDIT_BAR_H));
        ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| edit_bar(app, ui));
    } else {
        if resp.double_clicked() {
            app.toggle_lyric_window();
        }
        resp.on_hover_text("Double-click to pop out");
    }
    ui.allocate_rect(ui.available_rect_before_wrap(), Sense::hover());
}

fn time_chip(ui: &mut Ui, label: &str, t: f64, color: Color32) {
    ui.label(RichText::new(label).size(11.0).color(TEXT_FAINT));
    ui.label(RichText::new(timecode(t)).monospace().size(13.0).color(color));
}

fn edit_bar(app: &mut StageWeaveApp, ui: &mut Ui) {
    let sel = app.lyric_sel();
    let n = app.session.lyrics.lines.len();
    egui::Frame::NONE
        .fill(BG_INSET)
        .stroke(Stroke::new(1.0, EDIT_COLOR.gamma_multiply(0.5)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing = vec2(6.0, 8.0);

            // Row 1: which line + its text.
            ui.horizontal(|ui| {
                if icon_button(ui, Icon::StepBack, 24.0, false, "Previous line (↑)").clicked()
                    && let Some(i) = sel
                {
                    app.lyric_select(i.saturating_sub(1));
                }
                let label = match sel {
                    Some(i) => format!("Line {} of {n}", i + 1),
                    None => "No lines yet".into(),
                };
                ui.add_sized([86.0, 22.0], egui::Label::new(RichText::new(label).size(11.5).color(TEXT_DIM)));
                if icon_button(ui, Icon::StepForward, 24.0, false, "Next line (↓)").clicked()
                    && let Some(i) = sel
                {
                    app.lyric_select((i + 1).min(n.saturating_sub(1)));
                }
                match sel {
                    Some(i) => {
                        let focus = std::mem::take(&mut app.lyric_edit.focus_text);
                        // Edit a copy so the undo step can be recorded before the first keystroke.
                        let mut text = app.session.lyrics.lines[i].text.clone();
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .hint_text("Type the words for this line")
                                .font(FontId::proportional(14.0))
                                .desired_width(ui.available_width() - 70.0),
                        );
                        if focus {
                            r.request_focus();
                        }
                        if r.gained_focus() {
                            app.lyric_checkpoint("Edit Text", None);
                        }
                        if r.changed() {
                            app.session.lyrics.lines[i].text = text;
                            app.session.lyrics.sync_raw_from_lines();
                        }
                        if ui.add(egui::Button::new(RichText::new("Delete").color(DANGER))).on_hover_text("Delete this line").clicked() {
                            app.lyric_delete();
                        }
                    }
                    None => {
                        ui.label(RichText::new("Play the song and pause where a line starts, or press N to add one at the playhead (E stretches the previous line to it).").color(TEXT_FAINT));
                    }
                }
            });

            // Row 2: timing.
            if let Some(i) = sel {
                let line = app.session.lyrics.lines[i].clone();
                let playhead = app.position();
                let inside = playhead >= line.start && playhead < line.end;
                ui.horizontal(|ui| {
                    time_chip(ui, "Start", line.start, if inside { EDIT_COLOR } else { TEXT });
                    if ui.small_button("−").on_hover_text("Start 0.1 s earlier").clicked() {
                        app.lyric_nudge(true, -0.1);
                    }
                    if ui.small_button("+").on_hover_text("Start 0.1 s later").clicked() {
                        app.lyric_nudge(true, 0.1);
                    }
                    if ui.button("Mark Start (I)").on_hover_text("Set the start to the playhead").clicked() {
                        app.lyric_mark_start();
                    }
                    ui.add_space(10.0);
                    time_chip(ui, "End", line.end, TEXT);
                    if ui.small_button("−").on_hover_text("End 0.1 s earlier").clicked() {
                        app.lyric_nudge(false, -0.1);
                    }
                    if ui.small_button("+").on_hover_text("End 0.1 s later").clicked() {
                        app.lyric_nudge(false, 0.1);
                    }
                    let end_btn = egui::Button::new(RichText::new("Mark End (O)").color(Color32::BLACK)).fill(EDIT_COLOR);
                    if ui.add(end_btn).on_hover_text("Set the end to the playhead and move to the next line").clicked() {
                        app.lyric_mark_end();
                    }
                    ui.label(RichText::new(format!("{:.1} s", line.end - line.start)).monospace().size(11.0).color(TEXT_FAINT));
                });
            }

            // Row 3: structure + options.
            ui.horizontal(|ui| {
                if ui.button("Insert at Playhead (N)").clicked() {
                    app.lyric_new_at_playhead();
                }
                if ui.add_enabled(sel.is_some(), egui::Button::new("Split at Playhead")).clicked() {
                    app.lyric_split();
                }
                if ui.add_enabled(sel.is_some_and(|i| i + 1 < n), egui::Button::new("Merge with Next")).clicked() {
                    app.lyric_merge();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.checkbox(&mut app.lyric_edit.pause_marks_start, RichText::new("Pause marks start").size(11.5))
                        .on_hover_text("When on, pausing playback sets the selected line's start to the playhead");
                });
            });
        });
}
