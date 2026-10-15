//! Dark, dense "pro editor" look, loosely modelled on Premiere Pro.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};

pub const BG_APP: Color32 = Color32::from_rgb(14, 14, 15);
pub const BG_PANEL: Color32 = Color32::from_rgb(29, 29, 31);
pub const BG_PANEL_HEADER: Color32 = Color32::from_rgb(35, 35, 38);
pub const BG_INSET: Color32 = Color32::from_rgb(21, 21, 23);
pub const BG_TRACK_HEADER: Color32 = Color32::from_rgb(38, 38, 41);
pub const BG_TRACK_LANE: Color32 = Color32::from_rgb(25, 25, 27);
pub const BG_TRACK_LANE_ALT: Color32 = Color32::from_rgb(28, 28, 30);
pub const BG_WIDGET: Color32 = Color32::from_rgb(46, 46, 50);
pub const BG_WIDGET_HOVER: Color32 = Color32::from_rgb(58, 58, 63);

pub const BORDER: Color32 = Color32::from_rgb(48, 48, 52);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(64, 64, 70);

pub const TEXT: Color32 = Color32::from_rgb(214, 214, 218);
pub const TEXT_DIM: Color32 = Color32::from_rgb(150, 150, 158);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(98, 98, 106);

pub const ACCENT: Color32 = Color32::from_rgb(38, 128, 235);
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(84, 160, 255);
pub const PLAYHEAD: Color32 = Color32::from_rgb(56, 146, 255);
pub const MUTE_ON: Color32 = Color32::from_rgb(44, 168, 140);
pub const SOLO_ON: Color32 = Color32::from_rgb(226, 186, 52);
/// Cue loops (timeline bands, transport).
pub const CUE: Color32 = Color32::from_rgb(64, 204, 172);
/// Sections with their own mix (timeline lane, transport).
pub const SECTION: Color32 = Color32::from_rgb(226, 178, 118);
pub const DANGER: Color32 = Color32::from_rgb(232, 72, 72);
pub const SUCCESS: Color32 = Color32::from_rgb(70, 190, 110);

pub const RADIUS: u8 = 6;

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Heading, FontId::new(17.0, Proportional)),
            (TextStyle::Body, FontId::new(13.0, Proportional)),
            (TextStyle::Button, FontId::new(12.5, Proportional)),
            (TextStyle::Small, FontId::new(10.5, Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(6.0, 5.0);
        s.button_padding = egui::vec2(9.0, 4.0);
        s.interact_size.y = 22.0;
        s.menu_margin = Margin::same(6);
        s.window_margin = Margin::same(10);
        s.slider_rail_height = 4.0;
        s.scroll.floating = true;
        s.scroll.bar_width = 8.0;

        let v: &mut Visuals = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = BG_APP;
        v.window_fill = BG_PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER_STRONG);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = BG_INSET;
        v.faint_bg_color = BG_PANEL_HEADER;
        v.code_bg_color = BG_INSET;
        v.hyperlink_color = ACCENT_BRIGHT;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.75);
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.warn_fg_color = SOLO_ON;
        v.error_fg_color = DANGER;

        let r = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG_PANEL;
        w.noninteractive.weak_bg_fill = BG_PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = BG_WIDGET;
        w.inactive.weak_bg_fill = BG_WIDGET;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = BG_WIDGET_HOVER;
        w.hovered.weak_bg_fill = BG_WIDGET_HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = ACCENT;
        w.active.weak_bg_fill = ACCENT;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT_BRIGHT);
        w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = BG_WIDGET_HOVER;
        w.open.weak_bg_fill = BG_WIDGET_HOVER;
        w.open.corner_radius = r;
    });
}

/// Format seconds as `MM:SS.cc`.
pub fn timecode(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor() as u32;
    let s = t % 60.0;
    format!("{m:02}:{s:05.2}")
}

/// Format seconds as `HH:MM:SS:FF` at 30 fps (Premiere-style).
pub fn smpte(t: f64) -> String {
    let t = t.max(0.0);
    let total_frames = (t * 30.0).floor() as u64;
    let f = total_frames % 30;
    let s = (total_frames / 30) % 60;
    let m = (total_frames / 1800) % 60;
    let h = total_frames / 108_000;
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

pub fn db_label(db: f32) -> String {
    if db <= crate::model::MIN_DB { "-∞ dB".into() } else { format!("{db:+.1} dB") }
}
