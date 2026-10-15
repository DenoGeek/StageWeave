//! Reusable custom-painted widgets.

use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::model::{MAX_DB, MIN_DB};
use crate::theme::*;

/// A rounded "card" frame used for every docked panel.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(RADIUS))
        .outer_margin(Margin::same(3))
}

pub const HEADER_H: f32 = 30.0;

/// Premiere-style tab strip at the top of a panel. Returns true if the tab changed.
pub fn panel_header(
    ui: &mut Ui,
    tabs: &[&str],
    active: &mut usize,
    right: impl FnOnce(&mut Ui),
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 },
        BG_PANEL_HEADER,
    );
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, BORDER),
    );

    let mut changed = false;
    let mut x = rect.left() + 10.0;
    let font = FontId::proportional(12.5);
    for (i, tab) in tabs.iter().enumerate() {
        let galley = ui.painter().layout_no_wrap(tab.to_string(), font.clone(), TEXT);
        let w = galley.size().x + 16.0;
        let tab_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(w, HEADER_H));
        let resp = ui.interact(tab_rect, ui.id().with(("tab", tab)), Sense::click());
        let is_active = *active == i;
        let color = if is_active {
            Color32::WHITE
        } else if resp.hovered() {
            TEXT
        } else {
            TEXT_DIM
        };
        ui.painter().galley_with_override_text_color(
            tab_rect.center() - galley.size() / 2.0,
            galley,
            color,
        );
        if is_active {
            let underline = Rect::from_min_max(
                pos2(tab_rect.left() + 6.0, tab_rect.bottom() - 2.0),
                pos2(tab_rect.right() - 6.0, tab_rect.bottom()),
            );
            ui.painter().rect_filled(underline, 1.0, ACCENT_BRIGHT);
        }
        if resp.clicked() && !is_active {
            *active = i;
            changed = true;
        }
        x += w + 2.0;
    }

    let right_rect = Rect::from_min_max(pos2(x + 8.0, rect.top()), rect.max - vec2(8.0, 0.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    right(&mut child);
    changed
}

/// Small faint uppercase section label.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(10.0)
            .color(TEXT_FAINT)
            .strong(),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    ToStart,
    ToEnd,
    StepBack,
    StepForward,
    Loop,
    PopOut,
    Fullscreen,
    Import,
    Close,
    ZoomIn,
    ZoomOut,
    Follow,
    Sparkle,
    Search,
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(1.6, color);
    let tri = |pts: Vec<Pos2>| Shape::convex_polygon(pts, color, Stroke::NONE);
    match icon {
        Icon::Play => {
            painter.add(tri(vec![
                c + vec2(-0.45 * s, -0.6 * s),
                c + vec2(0.65 * s, 0.0),
                c + vec2(-0.45 * s, 0.6 * s),
            ]));
        }
        Icon::Pause => {
            let w = 0.28 * s;
            for dx in [-0.32 * s, 0.32 * s] {
                painter.rect_filled(
                    Rect::from_center_size(c + vec2(dx, 0.0), vec2(w, 1.2 * s)),
                    1.0,
                    color,
                );
            }
        }
        Icon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, vec2(1.0 * s, 1.0 * s)), 2.0, color);
        }
        Icon::ToStart | Icon::ToEnd => {
            let dir = if icon == Icon::ToEnd { 1.0 } else { -1.0 };
            painter.add(tri(vec![
                c + vec2(-0.45 * s * dir, -0.55 * s),
                c + vec2(0.35 * s * dir, 0.0),
                c + vec2(-0.45 * s * dir, 0.55 * s),
            ]));
            painter.rect_filled(
                Rect::from_center_size(c + vec2(0.5 * s * dir, 0.0), vec2(0.18 * s, 1.1 * s)),
                0.5,
                color,
            );
        }
        Icon::StepBack | Icon::StepForward => {
            let dir = if icon == Icon::StepForward { 1.0 } else { -1.0 };
            for off in [-0.38, 0.32] {
                let o = c + vec2(off * s * dir, 0.0);
                painter.add(tri(vec![
                    o + vec2(-0.3 * s * dir, -0.5 * s),
                    o + vec2(0.35 * s * dir, 0.0),
                    o + vec2(-0.3 * s * dir, 0.5 * s),
                ]));
            }
        }
        Icon::Loop => {
            let r = Rect::from_center_size(c, vec2(1.4 * s, 0.9 * s));
            painter.rect_stroke(r, 0.45 * s, stroke, StrokeKind::Middle);
            let a = pos2(r.right(), c.y);
            painter.add(tri(vec![a + vec2(-0.25 * s, -0.05 * s), a + vec2(0.25 * s, -0.05 * s), a + vec2(0.0, 0.3 * s)]));
        }
        Icon::PopOut => {
            let r = Rect::from_center_size(c + vec2(-0.1 * s, 0.1 * s), vec2(1.2 * s, 1.2 * s));
            painter.rect_stroke(r, 2.0, stroke, StrokeKind::Middle);
            let from = c + vec2(0.0, 0.0);
            let to = c + vec2(0.75 * s, -0.75 * s);
            painter.line_segment([from, to], stroke);
            painter.line_segment([to, to + vec2(-0.45 * s, 0.0)], stroke);
            painter.line_segment([to, to + vec2(0.0, 0.45 * s)], stroke);
        }
        Icon::Fullscreen => {
            let r = Rect::from_center_size(c, vec2(1.3 * s, 1.3 * s));
            let l = 0.42 * s;
            for (corner, dx, dy) in [
                (r.left_top(), 1.0, 1.0),
                (r.right_top(), -1.0, 1.0),
                (r.left_bottom(), 1.0, -1.0),
                (r.right_bottom(), -1.0, -1.0),
            ] {
                painter.line_segment([corner, corner + vec2(l * dx, 0.0)], stroke);
                painter.line_segment([corner, corner + vec2(0.0, l * dy)], stroke);
            }
        }
        Icon::Import => {
            painter.line_segment([c + vec2(0.0, -0.7 * s), c + vec2(0.0, 0.25 * s)], stroke);
            painter.line_segment([c + vec2(-0.35 * s, -0.1 * s), c + vec2(0.0, 0.25 * s)], stroke);
            painter.line_segment([c + vec2(0.35 * s, -0.1 * s), c + vec2(0.0, 0.25 * s)], stroke);
            painter.line_segment([c + vec2(-0.7 * s, 0.7 * s), c + vec2(0.7 * s, 0.7 * s)], stroke);
        }
        Icon::Close => {
            let d = 0.45 * s;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(d, -d), c + vec2(-d, d)], stroke);
        }
        Icon::ZoomIn | Icon::ZoomOut => {
            let d = 0.55 * s;
            painter.line_segment([c + vec2(-d, 0.0), c + vec2(d, 0.0)], stroke);
            if icon == Icon::ZoomIn {
                painter.line_segment([c + vec2(0.0, -d), c + vec2(0.0, d)], stroke);
            }
        }
        Icon::Sparkle => {
            // Big four-point star + a small one.
            let star = |c: Pos2, r: f32| {
                let k = r * 0.28;
                Shape::convex_polygon(
                    vec![c + vec2(0.0, -r), c + vec2(k, -k), c + vec2(r, 0.0), c + vec2(k, k), c + vec2(0.0, r), c + vec2(-k, k), c + vec2(-r, 0.0), c + vec2(-k, -k)],
                    color,
                    Stroke::NONE,
                )
            };
            painter.add(star(c + vec2(-0.15 * s, 0.15 * s), 0.8 * s));
            painter.add(star(c + vec2(0.62 * s, -0.62 * s), 0.35 * s));
        }
        Icon::Search => {
            let r = 0.5 * s;
            let lens = c + vec2(-0.18 * s, -0.18 * s);
            painter.circle_stroke(lens, r, stroke);
            let d = std::f32::consts::FRAC_1_SQRT_2;
            painter.line_segment([lens + vec2(r * d, r * d), c + vec2(0.75 * s, 0.75 * s)], Stroke::new(2.0, color));
        }
        Icon::Follow => {
            painter.line_segment([c + vec2(0.0, -0.75 * s), c + vec2(0.0, 0.75 * s)], stroke);
            painter.add(tri(vec![
                c + vec2(-0.35 * s, -0.75 * s),
                c + vec2(0.35 * s, -0.75 * s),
                c + vec2(0.0, -0.3 * s),
            ]));
        }
    }
}

/// An instrument icon for a stem (microphone, drum, keys…), drawn to fit
/// `rect` in `color`.
pub fn paint_stem_icon(painter: &egui::Painter, rect: Rect, kind: crate::model::StemKind, color: Color32) {
    use crate::model::StemKind;
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let w = (0.16 * s).max(1.0);
    let stroke = Stroke::new(w, color);
    let at = |x: f32, y: f32| c + vec2(x * s, y * s);
    // Points on an ellipse arc (angles in turns of a circle, y down).
    let arc = |cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32| -> Vec<Pos2> {
        (0..=16)
            .map(|k| {
                let a = (from + (to - from) * k as f32 / 16.0) * std::f32::consts::TAU;
                at(cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    };
    match kind {
        StemKind::Vocals => {
            // Microphone: capsule, cradle, stand.
            painter.rect_filled(Rect::from_min_max(at(-0.3, -0.9), at(0.3, 0.15)), 0.3 * s, color);
            painter.add(Shape::line(arc(0.0, -0.2, 0.55, 0.6, 0.0, 0.5), stroke));
            painter.line_segment([at(0.0, 0.4), at(0.0, 0.78)], stroke);
            painter.line_segment([at(-0.35, 0.82), at(0.35, 0.82)], stroke);
        }
        StemKind::Drums => {
            // Drum shell with two sticks crossing above it.
            painter.add(Shape::closed_line(arc(0.0, 0.0, 0.75, 0.24, 0.0, 1.0), stroke));
            painter.line_segment([at(-0.75, 0.0), at(-0.75, 0.55)], stroke);
            painter.line_segment([at(0.75, 0.0), at(0.75, 0.55)], stroke);
            painter.add(Shape::line(arc(0.0, 0.55, 0.75, 0.24, 0.0, 0.5), stroke));
            for d in [-1.0, 1.0] {
                painter.line_segment([at(0.1 * d, -0.1), at(0.7 * d, -0.85)], stroke);
                painter.circle_filled(at(0.7 * d, -0.85), 0.13 * s, color);
            }
        }
        StemKind::Bass => {
            // Bass clef.
            painter.circle_filled(at(-0.5, -0.25), 0.17 * s, color);
            let curve = [(-0.5, -0.25), (-0.4, -0.55), (-0.1, -0.72), (0.22, -0.62), (0.4, -0.3), (0.32, 0.08), (0.0, 0.42), (-0.5, 0.75)];
            painter.add(Shape::line(curve.iter().map(|&(x, y)| at(x, y)).collect(), Stroke::new(w * 1.2, color)));
            painter.circle_filled(at(0.68, -0.38), 0.11 * s, color);
            painter.circle_filled(at(0.68, 0.02), 0.11 * s, color);
        }
        StemKind::Guitar => {
            // Body (two lobes), neck and headstock, on a diagonal.
            painter.circle_filled(at(-0.38, 0.38), 0.44 * s, color);
            painter.circle_filled(at(-0.05, 0.05), 0.32 * s, color);
            painter.line_segment([at(-0.05, 0.05), at(0.62, -0.62)], Stroke::new(0.2 * s, color));
            painter.line_segment([at(0.58, -0.58), at(0.85, -0.85)], Stroke::new(0.36 * s, color));
        }
        StemKind::Piano => {
            // Four white keys and two black ones.
            let body = Rect::from_min_max(at(-0.85, -0.6), at(0.85, 0.6));
            painter.rect_stroke(body, 0.12 * s, stroke, egui::StrokeKind::Inside);
            for x in [-0.425, 0.0, 0.425] {
                painter.line_segment([at(x, -0.6), at(x, 0.6)], Stroke::new(w * 0.8, color));
            }
            for x in [-0.425, 0.0] {
                painter.rect_filled(Rect::from_min_max(at(x - 0.13, -0.6), at(x + 0.13, 0.12)), 0.0, color);
            }
        }
        StemKind::Other => {
            // A sparkle: two thin diamonds.
            painter.add(Shape::convex_polygon(vec![at(0.0, -0.9), at(0.2, 0.0), at(0.0, 0.9), at(-0.2, 0.0)], color, Stroke::NONE));
            painter.add(Shape::convex_polygon(vec![at(-0.9, 0.0), at(0.0, -0.2), at(0.9, 0.0), at(0.0, 0.2)], color, Stroke::NONE));
        }
        StemKind::Instrumental => {
            // Two beamed eighth notes.
            painter.circle_filled(at(-0.48, 0.55), 0.24 * s, color);
            painter.circle_filled(at(0.42, 0.4), 0.24 * s, color);
            painter.line_segment([at(-0.27, 0.52), at(-0.27, -0.55)], stroke);
            painter.line_segment([at(0.63, 0.37), at(0.63, -0.72)], stroke);
            painter.line_segment([at(-0.27, -0.55), at(0.63, -0.72)], Stroke::new(0.24 * s, color));
        }
    }
}

/// A stem's icon on a rounded badge in its colour (dimmed when silent).
pub fn stem_badge(ui: &mut Ui, size: f32, kind: crate::model::StemKind, color: Color32, audible: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(r, (size * 0.2).round(), color.gamma_multiply(if audible { 1.0 } else { 0.35 }));
    paint_stem_icon(ui.painter(), r.shrink(size * 0.18), kind, Color32::BLACK);
    resp
}

/// A small check mark.
pub fn paint_check(painter: &egui::Painter, rect: Rect, color: Color32) {
    let s = rect.width().min(rect.height());
    let o = rect.center() - vec2(s / 2.0, s / 2.0);
    let pts = vec![o + vec2(0.18 * s, 0.52 * s), o + vec2(0.42 * s, 0.76 * s), o + vec2(0.84 * s, 0.26 * s)];
    painter.add(Shape::line(pts, Stroke::new(2.0, color)));
}

/// Flat icon button. `active` paints it in the accent colour.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, active: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let painter = ui.painter();
    if resp.hovered() || active {
        let fill = if active { ACCENT.gamma_multiply(0.22) } else { BG_WIDGET_HOVER };
        painter.rect_filled(rect, 4.0, fill);
    }
    let color = if active {
        ACCENT_BRIGHT
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        TEXT_DIM
    };
    paint_icon(painter, rect.shrink(size * 0.22), icon, color);
    // Label for screen readers (and UI tests).
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip));
    resp.on_hover_text(tooltip)
}

/// The big round play button.
pub fn play_button(ui: &mut Ui, playing: bool, enabled: bool) -> Response {
    let size = 38.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let fill = if !enabled {
        BG_WIDGET
    } else if resp.is_pointer_button_down_on() {
        ACCENT.gamma_multiply(0.8)
    } else if resp.hovered() {
        ACCENT_BRIGHT
    } else {
        ACCENT
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    let icon = if playing { Icon::Pause } else { Icon::Play };
    let color = if enabled { Color32::WHITE } else { TEXT_FAINT };
    let mut icon_rect = rect.shrink(11.0);
    if !playing {
        icon_rect = icon_rect.translate(vec2(1.5, 0.0));
    }
    paint_icon(ui.painter(), icon_rect, icon, color);
    resp.on_hover_text(if playing { "Pause (Space)" } else { "Play (Space)" })
}

/// Blue call-to-action button.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let size = vec2(ui.available_width().max(galley.size().x + 24.0), 32.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, text));
    let fill = if !enabled {
        BG_WIDGET
    } else if resp.is_pointer_button_down_on() {
        ACCENT.gamma_multiply(0.85)
    } else if resp.hovered() {
        ACCENT_BRIGHT
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, 5.0, fill);
    let color = if enabled { Color32::WHITE } else { TEXT_FAINT };
    ui.painter().galley_with_override_text_color(rect.center() - galley.size() / 2.0, galley, color);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// Square toggle chip, used for Mute / Solo.
pub fn toggle_chip(ui: &mut Ui, label: &str, on: bool, on_color: Color32, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
    let (fill, text) = if on {
        (on_color, Color32::BLACK)
    } else if resp.hovered() {
        (BG_WIDGET_HOVER, Color32::WHITE)
    } else {
        (BG_WIDGET, TEXT_DIM)
    };
    ui.painter().rect_filled(rect, 3.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), text);
    resp.on_hover_text(tooltip)
}

/// Horizontal gain fader in dB. Double-click resets to 0 dB.
pub fn gain_fader(ui: &mut Ui, db: &mut f32, width: f32, color: Color32) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(width, 16.0), Sense::click_and_drag());
    let to_frac = |db: f32| {
        // Perceptual-ish: map dB through a gentle curve so 0 dB sits at ~80%.
        let lin = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0);
        lin.powf(1.6)
    };
    let from_frac = |f: f32| MIN_DB + f.clamp(0.0, 1.0).powf(1.0 / 1.6) * (MAX_DB - MIN_DB);

    if resp.double_clicked() {
        *db = 0.0;
        resp.mark_changed();
    } else if (resp.dragged() || resp.clicked())
        && let Some(p) = resp.interact_pointer_pos()
    {
        let f = (p.x - rect.left()) / rect.width();
        let new = (from_frac(f) * 10.0).round() / 10.0;
        if (new - *db).abs() > f32::EPSILON {
            *db = new;
            resp.mark_changed();
        }
    }

    let painter = ui.painter();
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
    painter.rect_filled(track, 2.0, BG_INSET);
    let f = to_frac(*db);
    let filled = Rect::from_min_max(track.min, pos2(track.left() + track.width() * f, track.bottom()));
    painter.rect_filled(filled, 2.0, color.gamma_multiply(0.85));
    // Unity mark.
    let unity_x = track.left() + track.width() * to_frac(0.0);
    painter.line_segment(
        [pos2(unity_x, rect.top() + 2.0), pos2(unity_x, rect.bottom() - 2.0)],
        Stroke::new(1.0, TEXT_FAINT),
    );
    let knob = pos2(track.left() + track.width() * f, rect.center().y);
    let hot = resp.hovered() || resp.dragged();
    painter.circle_filled(knob, if hot { 6.0 } else { 5.0 }, Color32::from_gray(235));
    painter.circle_stroke(knob, if hot { 6.0 } else { 5.0 }, Stroke::new(1.0, Color32::from_gray(40)));

    resp.on_hover_text(format!("{}  ·  double-click to reset", db_label(*db)))
}

/// Small rotary pan knob. Drag up/down; double-click centres.
pub fn pan_knob(ui: &mut Ui, pan: &mut f32) -> Response {
    let size = 20.0;
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
    if resp.double_clicked() {
        *pan = 0.0;
        resp.mark_changed();
    } else if resp.dragged() {
        let d = resp.drag_delta();
        *pan = (*pan + (d.x - d.y) * 0.01).clamp(-1.0, 1.0);
        resp.mark_changed();
    }
    let painter = ui.painter();
    let c = rect.center();
    let r = size / 2.0 - 1.0;
    painter.circle_filled(c, r, BG_INSET);
    painter.circle_stroke(c, r, Stroke::new(1.0, BORDER_STRONG));
    // Arc from centre to value.
    let start = -std::f32::consts::FRAC_PI_2;
    let end = start + *pan * 0.75 * std::f32::consts::PI;
    let steps = 16;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = start + (end - start) * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * (r - 2.0)
        })
        .collect();
    painter.add(Shape::line(pts, Stroke::new(2.0, ACCENT_BRIGHT)));
    let a = end;
    painter.line_segment([c, c + vec2(a.cos(), a.sin()) * (r - 3.0)], Stroke::new(1.5, Color32::WHITE));
    let label = if pan.abs() < 0.01 {
        "Pan: C".to_owned()
    } else if *pan < 0.0 {
        format!("Pan: L{:.0}", -*pan * 100.0)
    } else {
        format!("Pan: R{:.0}", *pan * 100.0)
    };
    resp.on_hover_text(format!("{label}  ·  drag to adjust, double-click to centre"))
}

fn meter_color(f: f32) -> Color32 {
    if f > 0.9 {
        Color32::from_rgb(235, 70, 60)
    } else if f > 0.72 {
        Color32::from_rgb(235, 200, 60)
    } else {
        Color32::from_rgb(60, 200, 110)
    }
}

/// Convert linear amplitude to a 0..1 meter position on a -48..0 dB scale.
pub fn meter_pos(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * level.log10();
    ((db + 48.0) / 48.0).clamp(0.0, 1.0)
}

/// Segmented vertical LED meter.
pub fn vertical_meter(painter: &egui::Painter, rect: Rect, level: f32) {
    painter.rect_filled(rect, 1.0, Color32::from_rgb(12, 12, 13));
    let segs = ((rect.height() / 4.0) as usize).max(4);
    let lit = (meter_pos(level) * segs as f32).round() as usize;
    let seg_h = rect.height() / segs as f32;
    for i in 0..segs {
        let f = i as f32 / segs as f32;
        let y1 = rect.bottom() - i as f32 * seg_h;
        let r = Rect::from_min_max(pos2(rect.left() + 1.0, y1 - seg_h + 1.0), pos2(rect.right() - 1.0, y1));
        let c = meter_color(f);
        painter.rect_filled(r, 0.0, if i < lit { c } else { c.gamma_multiply(0.09) });
    }
}

/// Thin horizontal meter (track headers).
pub fn horizontal_meter(painter: &egui::Painter, rect: Rect, level: f32) {
    painter.rect_filled(rect, 1.0, Color32::from_rgb(12, 12, 13));
    let f = meter_pos(level);
    if f <= 0.0 {
        return;
    }
    let w = rect.width() * f;
    // Draw in three colour zones so the bar reads like an LED meter.
    for (from, to) in [(0.0, 0.72), (0.72, 0.9), (0.9, 1.0)] {
        let a = rect.left() + rect.width() * from;
        let b = (rect.left() + rect.width() * to).min(rect.left() + w);
        if b > a {
            painter.rect_filled(
                Rect::from_min_max(pos2(a, rect.top()), pos2(b, rect.bottom())),
                0.0,
                meter_color(from + 0.01),
            );
        }
    }
}

/// Mirrored bar waveform from a peak envelope.
#[allow(clippy::too_many_arguments)]
pub fn paint_waveform(
    painter: &egui::Painter,
    rect: Rect,
    peaks: &crate::model::Peaks,
    t0: f64,
    secs_per_px: f64,
    color: Color32,
    gain: f32,
) {
    let mid = rect.center().y;
    let half = rect.height() / 2.0 - 1.0;
    let step = 2.0f32; // px per bar
    let mut x = rect.left();
    let mut mesh = egui::Mesh::default();
    while x < rect.right() {
        let ta = t0 + (x - rect.left()) as f64 * secs_per_px;
        let tb = ta + step as f64 * secs_per_px;
        let a = (peaks.max_in(ta, tb) * gain).min(1.0);
        if a > 0.003 {
            let h = (a * half).max(0.5);
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, mid - h), pos2((x + step - 0.6).min(rect.right()), mid + h)),
                color,
            );
        }
        x += step;
    }
    painter.add(mesh);
}
