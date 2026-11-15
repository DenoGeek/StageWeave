//! Presenter view for a slide deck: the current slide (with its video or
//! sound), the next slide, media controls and a strip of every slide.

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::StageWeaveApp;
use crate::deck::Size;
use crate::slides::MediaKind;
use crate::theme::*;

use super::widgets::{Icon, icon_button, panel_frame, section_label};

const STRIP_H: f32 = 132.0;

/// Fit a `size` image inside `area`, centred (letterboxed).
fn fit(size: egui::Vec2, area: Rect) -> Rect {
    let s = (area.width() / size.x.max(1.0)).min(area.height() / size.y.max(1.0));
    Rect::from_center_size(area.center(), size * s)
}

/// Paint the current slide (and its playing video) into `area`. Shared with
/// the projector window. Returns the slide's rect, if it's loaded.
pub fn paint_slide(app: &mut StageWeaveApp, ctx: &egui::Context, painter: &egui::Painter, area: Rect) -> Option<Rect> {
    let t = app.position();
    let d = app.deck.as_mut()?;
    let idx = d.idx;
    let (tex, size) = d.texture(ctx, idx, Size::Full).or_else(|| d.texture(ctx, idx, Size::Thumb))?;
    let r = fit(size, area);
    painter.image(tex, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    // Load the next slide too, so advancing is instant.
    let _ = d.texture(ctx, idx + 1, Size::Full);
    let playing_here = d.media.as_ref().is_some_and(|m| m.slide == idx && m.video.is_some());
    if playing_here {
        let item = d.media.as_ref().map_or(0, |m| m.item);
        let rect = d.current_media().get(item).map(|m| m.rect);
        if let (Some(frame), Some([x, y, w, h])) = (d.media_frame(ctx, t), rect) {
            let mr = Rect::from_min_size(pos2(r.left() + x * r.width(), r.top() + y * r.height()), vec2(w * r.width(), h * r.height()));
            painter.rect_filled(mr, 0.0, Color32::BLACK);
            painter.image(frame, mr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
    Some(r)
}

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let Some(d) = app.deck.as_ref() else { return };
    let (idx, n) = (d.idx, d.len());

    // Strip of all slides along the bottom.
    egui::Panel::bottom("deck_strip").frame(panel_frame()).exact_size(STRIP_H).resizable(false).show_separator_line(false).show(ui, |ui| strip(app, ui));

    // Controls on the right: next slide, media.
    egui::Panel::right("deck_side").frame(panel_frame()).exact_size(300.0).resizable(false).show_separator_line(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Slide {} of {n}", idx + 1)).size(15.0).color(Color32::WHITE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::StepForward, 26.0, false, "Next slide (→, PageDown)").clicked() && !app.deck_next() {
                    app.next_song(&ctx);
                }
                if icon_button(ui, Icon::StepBack, 26.0, false, "Previous slide (←, PageUp)").clicked() && !app.deck_prev() {
                    app.prev_song(&ctx);
                }
            });
        });
        ui.add_space(8.0);
        section_label(ui, if idx + 1 < n { "Next" } else { "Next in setlist" });
        let (next_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ui.available_width() * 9.0 / 16.0), Sense::hover());
        ui.painter().rect_filled(next_rect, 4.0, Color32::BLACK);
        if idx + 1 < n {
            if let Some((tex, size)) = app.deck.as_mut().and_then(|d| d.texture(&ctx, idx + 1, Size::Thumb)) {
                ui.painter().image(tex, fit(size, next_rect), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        } else {
            let label = app.neighbour_song(1).map_or("End of the setlist".to_owned(), |s| s.title);
            ui.painter().text(next_rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), TEXT_DIM);
        }
        ui.add_space(6.0);
        media_controls(app, ui);
        ui.add_space(10.0);
        ui.label(RichText::new("→ / ↓ / PageDown: next · ← / ↑ / PageUp: back\nSpace: play the slide's video, or next slide\n⇧⌘P: show on the projector window").size(10.5).color(TEXT_FAINT));
    });

    // The slide itself.
    egui::CentralPanel::no_frame().show(ui, |ui| {
        panel_frame().show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            let area = ui.max_rect().shrink(8.0);
            let resp = ui.interact(area, ui.id().with("deck_slide"), Sense::click());
            let painter = ui.painter().clone();
            painter.rect_filled(area, 4.0, Color32::BLACK);
            match paint_slide(app, &ctx, &painter, area) {
                Some(r) => {
                    // A play badge over media that isn't playing.
                    let playing = app.is_playing();
                    if let Some(d) = app.deck.as_ref() {
                        for (k, m) in d.current_media().iter().enumerate() {
                            let [x, y, w, h] = m.rect;
                            let mr = Rect::from_min_size(pos2(r.left() + x * r.width(), r.top() + y * r.height()), vec2(w * r.width(), h * r.height()));
                            let active = d.media.as_ref().is_some_and(|p| p.item == k);
                            painter.rect_stroke(mr, 2.0, Stroke::new(1.0, ACCENT_BRIGHT.gamma_multiply(0.6)), egui::StrokeKind::Outside);
                            if !(active && playing) {
                                let c = mr.center();
                                painter.circle_filled(c, 22.0, Color32::from_black_alpha(160));
                                painter.add(egui::Shape::convex_polygon(vec![c + vec2(-7.0, -11.0), c + vec2(-7.0, 11.0), c + vec2(12.0, 0.0)], Color32::WHITE, Stroke::NONE));
                            }
                        }
                    }
                    // Click media to play / pause it; click elsewhere for the next slide.
                    if resp.clicked()
                        && let Some(p) = resp.interact_pointer_pos()
                    {
                        let hit = app.deck.as_ref().and_then(|d| {
                            d.current_media().iter().position(|m| {
                                let [x, y, w, h] = m.rect;
                                Rect::from_min_size(pos2(r.left() + x * r.width(), r.top() + y * r.height()), vec2(w * r.width(), h * r.height())).contains(p)
                            })
                        });
                        match hit {
                            Some(k) if app.deck.as_ref().and_then(|d| d.media.as_ref()).is_some_and(|m| m.item == k) => app.media_toggle(),
                            Some(k) => app.media_start(k, true),
                            None => {
                                if !app.deck_next() {
                                    app.next_song(&ctx);
                                }
                            }
                        }
                    }
                }
                None => {
                    painter.text(area.center(), Align2::CENTER_CENTER, "Loading slide…", FontId::proportional(13.0), TEXT_DIM);
                }
            }
            if app.is_playing() || app.deck.as_ref().is_some_and(|d| d.media.as_ref().is_some_and(|m| m.loading())) {
                ui.ctx().request_repaint();
            }
        });
    });
}

/// Play / pause and a seek bar for the slide's video or sound.
fn media_controls(app: &mut StageWeaveApp, ui: &mut Ui) {
    let Some(d) = app.deck.as_ref() else { return };
    let media = d.current_media().to_vec();
    if media.is_empty() {
        return;
    }
    section_label(ui, "Media on this slide");
    let active = d.media.as_ref().map(|m| (m.item, m.duration, m.loading()));
    for (k, m) in media.iter().enumerate() {
        let name = std::path::Path::new(&m.file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = if m.kind == MediaKind::Video { "Video" } else { "Sound" };
        let is_active = active.is_some_and(|a| a.0 == k);
        ui.horizontal(|ui| {
            let playing = is_active && app.is_playing();
            if icon_button(ui, if playing { Icon::Pause } else { Icon::Play }, 24.0, playing, "Play / pause (Space)").clicked() {
                if is_active {
                    app.media_toggle();
                } else {
                    app.media_start(k, true);
                }
            }
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(&name).color(TEXT)).truncate());
                let mut tags = vec![kind];
                if m.autoplay {
                    tags.push("plays by itself");
                }
                if m.looping {
                    tags.push("loops");
                }
                ui.label(RichText::new(tags.join(" · ")).size(10.5).color(TEXT_FAINT));
            });
        });
        if let Some((_, dur, loading)) = active.filter(|_| is_active) {
            if loading {
                ui.label(RichText::new("Loading…").size(11.0).color(TEXT_DIM));
            } else if dur > 0.0 {
                let pos = app.position().min(dur);
                let (bar, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::click_and_drag());
                let p = ui.painter();
                p.rect_filled(bar.shrink2(vec2(0.0, 5.0)), 2.0, BG_INSET);
                let fill = Rect::from_min_max(bar.shrink2(vec2(0.0, 5.0)).min, pos2(bar.left() + bar.width() * (pos / dur) as f32, bar.bottom() - 5.0));
                p.rect_filled(fill, 2.0, ACCENT_BRIGHT);
                if (resp.clicked() || resp.dragged())
                    && let Some(pt) = resp.interact_pointer_pos()
                {
                    app.seek(((pt.x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * dur);
                }
                ui.label(RichText::new(format!("{} / {}", super::project_panel::mmss(pos), super::project_panel::mmss(dur))).monospace().size(10.5).color(TEXT_DIM));
            }
        }
        ui.add_space(4.0);
    }
}

/// Every slide as a thumbnail; click one to show it.
fn strip(app: &mut StageWeaveApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let Some(n) = app.deck.as_ref().map(|d| d.len()) else { return };
    let idx = app.deck.as_ref().map_or(0, |d| d.idx);
    let h = STRIP_H - 34.0;
    let w = h * 16.0 / 9.0;
    let mut go = None;
    // Scroll the current slide into view when it changes.
    let shown_id = ui.id().with("deck_strip_shown");
    let shown: Option<usize> = ui.data(|d| d.get_temp(shown_id));
    ui.data_mut(|d| d.insert_temp(shown_id, idx));
    egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            for i in 0..n {
                let (r, resp) = ui.allocate_exact_size(vec2(w, h + 16.0), Sense::click());
                let img = Rect::from_min_size(r.min, vec2(w, h));
                let p = ui.painter();
                p.rect_filled(img, 3.0, Color32::BLACK);
                if let Some((tex, size)) = app.deck.as_mut().and_then(|d| d.texture(&ctx, i, Size::Thumb)) {
                    p.image(tex, fit(size, img), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                let has_media = app.deck.as_ref().is_some_and(|d| !d.deck.slides[i].media.is_empty());
                let stroke = if i == idx { Stroke::new(2.5, ACCENT_BRIGHT) } else if resp.hovered() { Stroke::new(1.0, TEXT_DIM) } else { Stroke::new(1.0, BORDER) };
                p.rect_stroke(img, 3.0, stroke, egui::StrokeKind::Outside);
                let label = if has_media { format!("{}  ▶", i + 1) } else { format!("{}", i + 1) };
                p.text(pos2(img.left() + 2.0, img.bottom() + 8.0), Align2::LEFT_CENTER, label, FontId::proportional(10.5), if i == idx { Color32::WHITE } else { TEXT_FAINT });
                if i == idx && shown != Some(idx) {
                    resp.scroll_to_me(Some(Align::Center));
                }
                if resp.clicked() {
                    go = Some(i);
                }
            }
        });
    });
    if let Some(i) = go {
        app.deck_go(i);
    }
}
