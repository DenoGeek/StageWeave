//! Small pieces shared by the Home, Project and Setlist views.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, pos2, vec2};

use crate::app::SongStatus;
use crate::theme::*;

/// (label, colour) for a song status.
pub fn status_label(status: &SongStatus) -> (String, Color32) {
    match status {
        SongStatus::Importing(f) => (format!("Copying {:.0}%", f * 100.0), ACCENT_BRIGHT),
        SongStatus::Missing => ("File missing".into(), DANGER),
        SongStatus::Queued => ("Queued".into(), ACCENT_BRIGHT),
        SongStatus::Splitting { progress, stage } => {
            if stage.starts_with("Downloading model") {
                let pct = if *progress >= 0.0 { format!(" {:.0}%", progress * 100.0) } else { String::new() };
                (format!("Downloading model{pct}"), ACCENT_BRIGHT)
            } else if *progress >= 0.0 {
                (format!("Splitting {:.0}%", progress * 100.0), ACCENT_BRIGHT)
            } else {
                ("Splitting…".into(), ACCENT_BRIGHT)
            }
        }
        SongStatus::Split(n) => (format!("Split · {n} stems"), SUCCESS),
        SongStatus::NotSplit => ("Not split".into(), TEXT_FAINT),
    }
}

/// Pill anchored at its right-top corner. Returns its rect.
pub fn status_badge(painter: &Painter, right_top: Pos2, status: &SongStatus) -> Rect {
    let (text, color) = status_label(status);
    let g = painter.layout_no_wrap(text, FontId::proportional(10.0), color);
    let dot = matches!(status, SongStatus::Split(_) | SongStatus::Missing);
    let extra = if dot { 10.0 } else { 0.0 };
    let rect = Rect::from_min_size(pos2(right_top.x - g.size().x - 14.0 - extra, right_top.y - 9.0), g.size() + vec2(14.0 + extra, 6.0));
    painter.rect_filled(rect, 9.0, color.gamma_multiply(0.14));
    if dot {
        painter.circle_filled(pos2(rect.left() + 9.0, rect.center().y), 3.0, color);
    }
    painter.galley(rect.min + vec2(7.0 + extra, 3.0), g, color);
    rect
}

/// 2px bar; negative progress animates.
pub fn thin_progress(painter: &Painter, rect: Rect, progress: f32, time: f64) {
    painter.rect_filled(rect, 1.0, BG_WIDGET);
    if progress >= 0.0 {
        let mut fill = rect;
        fill.set_right(rect.left() + rect.width() * progress.clamp(0.0, 1.0));
        painter.rect_filled(fill, 1.0, ACCENT_BRIGHT);
    } else {
        let w = rect.width() * 0.3;
        let x = rect.left() + (((time as f32) * 0.6).fract() * 1.3 - 0.3) * rect.width();
        let seg = Rect::from_min_max(pos2(x.max(rect.left()), rect.top()), pos2((x + w).min(rect.right()), rect.bottom()));
        if seg.width() > 0.0 {
            painter.rect_filled(seg, 1.0, ACCENT_BRIGHT);
        }
    }
}

/// Mini waveform from a stored thumbnail (u8 peaks).
pub fn paint_thumb(painter: &Painter, rect: Rect, thumb: &[u8], color: Color32) {
    painter.rect_filled(rect, 3.0, BG_INSET);
    if thumb.is_empty() {
        painter.line_segment([rect.left_center(), rect.right_center()], Stroke::new(1.0, TEXT_FAINT.gamma_multiply(0.5)));
        return;
    }
    let bars = (rect.width() / 2.0) as usize;
    let mid = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    let mut mesh = egui::Mesh::default();
    for b in 0..bars {
        let i0 = b * thumb.len() / bars;
        let i1 = ((b + 1) * thumb.len() / bars).max(i0 + 1).min(thumb.len());
        let v = thumb[i0..i1].iter().copied().max().unwrap_or(0) as f32 / 255.0;
        let h = (v * half).max(0.5);
        let x = rect.left() + b as f32 * 2.0;
        mesh.add_colored_rect(Rect::from_min_max(pos2(x, mid - h), pos2(x + 1.4, mid + h)), color);
    }
    painter.add(mesh);
}

/// Empty-state illustration: stacked waveform bars.
pub fn paint_glyph(painter: &Painter, center: Pos2, color: Color32) {
    for (i, h) in [10.0, 22.0, 34.0, 18.0, 28.0, 12.0, 20.0].iter().enumerate() {
        let x = center.x - 27.0 + i as f32 * 9.0;
        painter.line_segment([pos2(x, center.y - h / 2.0), pos2(x, center.y + h / 2.0)], Stroke::new(4.0, color));
    }
}

pub fn centered_text(painter: &Painter, pos: Pos2, text: &str, size: f32, color: Color32) {
    painter.text(pos, Align2::CENTER_CENTER, text, FontId::proportional(size), color);
}

/// "12 Oct 2026" from a unix timestamp (UTC).
pub fn short_date(ts: i64) -> String {
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// Howard Hinnant's days → (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(short_date(1_791_936_000), "14 Oct 2026");
    }
}
