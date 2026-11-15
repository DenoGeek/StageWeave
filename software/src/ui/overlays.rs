//! Floating layers: detachable lyric window, drag-and-drop overlay, toasts.

use egui::{Align2, Color32, FontId, Id, Key, Order, Rect, Shape, Stroke, ViewportBuilder, ViewportCommand, ViewportId, vec2};

use crate::app::{StageWeaveApp, ToastKind};
use crate::theme::*;

use super::lyrics_view;

/// The lyric video in its own OS window, so it can be dragged onto a second
/// display (projector / TV) while the tracks stay on the main screen.
pub fn lyric_window(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if !app.lyric_window_open {
        return;
    }
    let title = if app.session.lyrics.title.is_empty() {
        "StageWeave — Lyric Video".to_owned()
    } else {
        format!("{} — Lyric Video", app.session.lyrics.title)
    };
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([480.0, 270.0]);

    ctx.show_viewport_immediate(ViewportId::from_hash_of("lyric_window"), builder, |ui, _class| {
        let (close, dbl, f_key, esc, space, fullscreen) = ui.input_mut(|i| {
            (
                i.viewport().close_requested(),
                i.pointer.button_double_clicked(egui::PointerButton::Primary),
                i.consume_key(egui::Modifiers::NONE, Key::F),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
                i.consume_key(egui::Modifiers::NONE, Key::Space),
                i.viewport().fullscreen.unwrap_or(false),
            )
        });
        if close {
            app.lyric_window_open = false;
            app.lyric_window_fullscreen = false;
            return;
        }
        if dbl || f_key {
            app.lyric_window_fullscreen = !app.lyric_window_fullscreen;
        }
        if esc && app.lyric_window_fullscreen {
            app.lyric_window_fullscreen = false;
        }
        if app.deck.is_some() {
            // Clickers send PageDown / PageUp (or arrows) to whichever window has focus.
            let (next, prev) = ui.input_mut(|i| {
                let k = |i: &mut egui::InputState, key| i.consume_key(egui::Modifiers::NONE, key);
                (
                    k(i, Key::ArrowRight) | k(i, Key::ArrowDown) | k(i, Key::PageDown),
                    k(i, Key::ArrowLeft) | k(i, Key::ArrowUp) | k(i, Key::PageUp),
                )
            });
            let ctx = ui.ctx().clone();
            if next && !app.deck_next() {
                app.next_song(&ctx);
            }
            if prev && !app.deck_prev() {
                app.prev_song(&ctx);
            }
            if space {
                app.deck_space(&ctx);
            }
        } else if space {
            app.toggle_play();
        }
        if fullscreen != app.lyric_window_fullscreen {
            ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(app.lyric_window_fullscreen));
        }

        let rect = ui.max_rect();
        let anim = ui.input(|i| i.time);
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        if app.deck.is_some() {
            let ctx = ui.ctx().clone();
            let painter = ui.painter().clone();
            let _ = super::deck_view::paint_slide(app, &ctx, &painter, rect);
            if app.is_playing() {
                ui.ctx().request_repaint();
            }
            return;
        }
        let (t, dur) = (app.position(), app.session.duration());
        let backdrop = app.lyric_backdrop(ui.ctx());
        lyrics_view::paint(ui.painter(), rect, &app.session.lyrics, t, dur, anim, backdrop);
        // Up-next card near the end of the song (or after the last lyric line).
        if let Some(next) = app.neighbour_song(1) {
            let after_lyrics = app.session.lyrics.lines.last().is_some_and(|l| t > l.end + 1.0);
            let near_end = dur > 0.0 && dur - t < 20.0;
            if after_lyrics || near_end {
                let alpha = if near_end { ((20.0 - (dur - t)) / 2.0).clamp(0.0, 1.0) as f32 } else { 1.0 };
                lyrics_view::paint_next_card(ui.painter(), rect, &next.title, &next.artist, alpha);
            }
        }

        // Hint bar while the mouse is moving over the window.
        let idle = ui.input(|i| i.pointer.time_since_last_movement());
        let hovering = ui.input(|i| i.pointer.has_pointer());
        if hovering && idle < 2.5 {
            let alpha = (1.0 - (idle - 2.0).max(0.0) * 2.0).clamp(0.0, 1.0);
            let hint = if app.lyric_window_fullscreen {
                "Double-click or F to exit fullscreen  ·  Esc  ·  Space play/pause"
            } else {
                "Drag this window to another display  ·  Double-click or F for fullscreen  ·  Space play/pause"
            };
            let galley = ui.painter().layout_no_wrap(hint.to_owned(), FontId::proportional(12.0), Color32::WHITE);
            let pill = Rect::from_center_size(rect.center_bottom() - vec2(0.0, 36.0), galley.size() + vec2(28.0, 14.0));
            ui.painter().rect_filled(pill, 14.0, Color32::from_black_alpha((180.0 * alpha) as u8));
            ui.painter().galley_with_override_text_color(
                pill.center() - galley.size() / 2.0,
                galley,
                Color32::from_white_alpha((230.0 * alpha) as u8),
            );
            ui.ctx().request_repaint();
        }
        if app.session.lyrics.style.animated_background || app.is_playing() {
            ui.ctx().request_repaint();
        }
    });
}

pub fn drop_overlay(app: &StageWeaveApp, ctx: &egui::Context) {
    let hovered: Vec<String> = ctx.input(|i| {
        i.raw
            .hovered_files
            .iter()
            .map(|f| {
                f.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect()
    });
    if hovered.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Foreground, Id::new("drop_overlay")));
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
    let box_rect = Rect::from_center_size(screen.center(), vec2(440.0, 220.0));
    painter.rect_filled(box_rect, 14.0, ACCENT.gamma_multiply(0.12));
    let r = box_rect;
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(Shape::dashed_line(&pts, Stroke::new(2.0, ACCENT_BRIGHT), 10.0, 6.0));
    let (headline, after) = match app.current_project() {
        Some(p) => (format!("Add to {}", p.name), "Songs are copied into the project's workspace folder".to_owned()),
        None => ("Start a new project".to_owned(), "The songs become this Sunday's setlist".to_owned()),
    };
    painter.text(r.center() - vec2(0.0, 22.0), Align2::CENTER_CENTER, headline, FontId::proportional(22.0), Color32::WHITE);
    let name = hovered.first().cloned().unwrap_or_default();
    let more = if hovered.len() > 1 { format!(" and {} more", hovered.len() - 1) } else { String::new() };
    let sub = if name.is_empty() { "Release to add".to_owned() } else { format!("{name}{more}") };
    painter.text(r.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, sub, FontId::proportional(13.0), TEXT_DIM);
    painter.text(
        r.center() + vec2(0.0, 44.0),
        Align2::CENTER_CENTER,
        &after,
        FontId::proportional(11.0),
        TEXT_FAINT,
    );
    ctx.request_repaint();
}

pub fn toasts(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if app.toasts.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, Id::new("toasts")));
    let mut y = screen.bottom() - 40.0;
    for toast in app.toasts.iter().rev() {
        let age = toast.born.elapsed().as_secs_f32();
        let alpha = (1.0 - (age - 3.4).max(0.0) / 0.6).clamp(0.0, 1.0) * (age / 0.15).min(1.0);
        let galley = painter.layout(toast.text.clone(), FontId::proportional(12.5), Color32::WHITE, 320.0);
        let size = galley.size() + vec2(34.0, 18.0);
        let rect = Rect::from_min_size(egui::pos2(screen.right() - size.x - 16.0, y - size.y), size);
        let accent = match toast.kind {
            ToastKind::Info => ACCENT_BRIGHT,
            ToastKind::Success => SUCCESS,
            ToastKind::Error => DANGER,
        };
        painter.rect_filled(rect.translate(vec2(0.0, 3.0)), 8.0, Color32::from_black_alpha((80.0 * alpha) as u8));
        painter.rect_filled(rect, 8.0, BG_PANEL_HEADER.gamma_multiply(alpha));
        painter.rect_stroke(rect, 8.0, Stroke::new(1.0, BORDER_STRONG.gamma_multiply(alpha)), egui::StrokeKind::Inside);
        painter.circle_filled(rect.left_center() + vec2(14.0, 0.0), 4.0, accent.gamma_multiply(alpha));
        painter.galley_with_override_text_color(
            rect.min + vec2(26.0, 9.0),
            galley,
            Color32::from_white_alpha((235.0 * alpha) as u8),
        );
        y = rect.top() - 8.0;
    }
}
