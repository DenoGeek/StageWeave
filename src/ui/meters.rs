use egui::{Align2, FontId, Rect, Stroke, Ui, pos2, vec2};

use crate::app::MusicKitApp;
use crate::theme::*;

use super::widgets::{HEADER_H, meter_pos, vertical_meter};

pub fn draw(app: &mut MusicKitApp, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, egui::Sense::hover());
    let p = ui.painter();

    let head = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
    p.rect_filled(head, egui::CornerRadius { nw: RADIUS, ne: RADIUS, sw: 0, se: 0 }, BG_PANEL_HEADER);
    p.line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0, BORDER));
    p.text(head.center(), Align2::CENTER_CENTER, "Master", FontId::proportional(12.0), TEXT);

    let body = Rect::from_min_max(pos2(rect.left() + 10.0, head.bottom() + 14.0), pos2(rect.right() - 8.0, rect.bottom() - 30.0));
    let meter_w = 10.0;
    let gap = 3.0;
    let l = Rect::from_min_size(body.min, vec2(meter_w, body.height()));
    let r = l.translate(vec2(meter_w + gap, 0.0));
    vertical_meter(p, l, app.master_meter[0]);
    vertical_meter(p, r, app.master_meter[1]);
    p.text(l.center_top() - vec2(0.0, 7.0), Align2::CENTER_CENTER, "L", FontId::proportional(9.0), TEXT_FAINT);
    p.text(r.center_top() - vec2(0.0, 7.0), Align2::CENTER_CENTER, "R", FontId::proportional(9.0), TEXT_FAINT);

    // dB scale.
    let scale_x = r.right() + 6.0;
    for db in [0.0f32, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0] {
        let f = meter_pos(10f32.powf(db / 20.0));
        let y = body.bottom() - body.height() * f;
        p.line_segment([pos2(scale_x, y), pos2(scale_x + 3.0, y)], Stroke::new(1.0, TEXT_FAINT));
        p.text(pos2(scale_x + 6.0, y), Align2::LEFT_CENTER, format!("{}", db as i32), FontId::monospace(8.5), TEXT_FAINT);
    }

    let peak = app.master_meter[0].max(app.master_meter[1]);
    let label = if peak <= 0.0005 { "-∞".to_owned() } else { format!("{:.1}", 20.0 * peak.log10()) };
    let color = if peak > 0.89 { DANGER } else { TEXT_DIM };
    p.text(pos2(rect.center().x, rect.bottom() - 15.0), Align2::CENTER_CENTER, label, FontId::monospace(11.0), color);
}
