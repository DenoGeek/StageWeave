//! Lyric-video renderer. Used by both the embedded program monitor and the
//! detachable lyric window, so they always show the same frame.

use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Align, Align2, Color32, FontId, Galley, Mesh, Painter, Pos2, Rect, TextureId, Vec2, pos2, vec2};

use crate::model::{Lyrics, split_translation};

fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

fn gradient(painter: &Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

/// Cheap soft glow: a radial gradient fan.
fn glow(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    let n = 48;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + vec2(a.cos(), a.sin()) * radius, Color32::TRANSPARENT);
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    painter.add(mesh);
}

fn centered_galley(painter: &Painter, text: &str, size: f32, color: Color32, wrap: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), FontId::proportional(size), color, wrap);
    job.halign = Align::Center;
    painter.layout_job(job)
}

/// Paint a centred galley with a karaoke fill from left to right across rows.
fn karaoke(painter: &Painter, galley: &Arc<Galley>, center_top: Pos2, base: Color32, hi: Color32, progress: f32) {
    painter.galley_with_override_text_color(center_top, galley.clone(), base);
    if progress <= 0.0 {
        return;
    }
    let total: f32 = galley.rows.iter().map(|r| r.rect().width()).sum();
    let mut remaining = total * progress.clamp(0.0, 1.0);
    for row in &galley.rows {
        if remaining <= 0.0 {
            break;
        }
        let r = row.rect().translate(center_top.to_vec2());
        let w = remaining.min(r.width());
        remaining -= w;
        let clip = Rect::from_min_max(pos2(r.left() - 2.0, r.top() - 4.0), pos2(r.left() + w, r.bottom() + 4.0));
        painter
            .with_clip_rect(clip.intersect(painter.clip_rect()))
            .galley_with_override_text_color(center_top, galley.clone(), hi);
    }
}

/// What to draw behind the lyrics.
#[derive(Clone, Copy, Debug)]
pub enum Backdrop {
    /// The theme gradient.
    Theme,
    /// An image or the current video frame (`size` in pixels, cropped to fill).
    Texture { id: TextureId, size: Vec2 },
    /// Nothing but the dimming layer: composited over the background later (video export).
    Transparent,
}

/// UV rect that crops an image of `size` to fill `rect` (like CSS `cover`).
pub fn cover_uv(size: Vec2, rect: Rect) -> Rect {
    let (ia, ra) = (size.x / size.y.max(1.0), rect.width() / rect.height().max(1.0));
    if ia > ra {
        let w = ra / ia;
        Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
    } else {
        let h = ia / ra;
        Rect::from_min_max(pos2(0.0, 0.5 - h / 2.0), pos2(1.0, 0.5 + h / 2.0))
    }
}

/// Render one frame of the lyric video into `rect`.
pub fn paint(painter: &Painter, rect: Rect, lyrics: &Lyrics, t: f64, duration: f64, anim_time: f64, backdrop: Backdrop) {
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let style = &lyrics.style;
    let [top, bottom, hi, accent] = style.theme.palette();
    let h = rect.height();
    let w = rect.width();

    let dim = Color32::from_black_alpha((style.background_dim.clamp(0.0, 1.0) * 255.0) as u8);
    match backdrop {
        Backdrop::Theme => gradient(&painter, rect, top, bottom),
        Backdrop::Texture { id, size } => {
            painter.image(id, rect, cover_uv(size, rect), Color32::WHITE);
            painter.rect_filled(rect, 0.0, dim);
        }
        Backdrop::Transparent => {
            painter.rect_filled(rect, 0.0, dim);
        }
    }
    if style.animated_background && matches!(backdrop, Backdrop::Theme) {
        let a = anim_time as f32;
        let blobs = [
            (0.25, 0.35, 0.21, 0.13, accent.gamma_multiply(0.30)),
            (0.75, 0.65, 0.17, 0.11, hi.gamma_multiply(0.16)),
            (0.55, 0.20, 0.13, 0.19, accent.gamma_multiply(0.18)),
        ];
        for (i, (x, y, sx, sy, c)) in blobs.into_iter().enumerate() {
            let fi = i as f32;
            let p = pos2(
                rect.left() + w * (x + 0.12 * (a * sx + fi).sin()),
                rect.top() + h * (y + 0.10 * (a * sy + fi * 2.0).cos()),
            );
            glow(&painter, p, w.max(h) * (0.35 + 0.05 * (a * 0.3 + fi).sin()), c);
        }
    }
    // Vignette.
    gradient(
        &painter,
        Rect::from_min_max(pos2(rect.left(), rect.bottom() - h * 0.35), rect.max),
        Color32::TRANSPARENT,
        Color32::from_black_alpha(110),
    );

    let base_size = h * 0.085 * style.scale;
    let wrap = w * 0.84;
    let dim = Color32::from_white_alpha(120);
    let faint = Color32::from_white_alpha(55);

    if lyrics.lines.is_empty() {
        // No lyrics for this song: hold on a title card.
        let title = if lyrics.title.is_empty() { "MusicKit" } else { &lyrics.title };
        let g = centered_galley(&painter, title, h * 0.1, Color32::WHITE, wrap);
        let y = rect.center().y - g.size().y * 0.6;
        painter.galley(pos2(rect.center().x, y), g.clone(), Color32::WHITE);
        if !lyrics.artist.is_empty() {
            painter.text(pos2(rect.center().x, y + g.size().y + h * 0.04), Align2::CENTER_CENTER, &lyrics.artist, FontId::proportional(h * 0.045), hi);
        }
        progress_bar(&painter, rect, lyrics, t, duration, hi);
        return;
    }

    let current = lyrics.line_at(t);
    let next = lyrics.next_line_after(t);
    let first_start = lyrics.lines[0].start;

    // Title card before the first line.
    if t < first_start && current.is_none() {
        let title = if lyrics.title.is_empty() { "Untitled" } else { &lyrics.title };
        let g = centered_galley(&painter, title, h * 0.1, Color32::WHITE, wrap);
        let y = rect.center().y - g.size().y * 0.75;
        painter.galley(pos2(rect.center().x, y), g.clone(), Color32::WHITE);
        if !lyrics.artist.is_empty() {
            painter.text(
                pos2(rect.center().x, y + g.size().y + h * 0.04),
                Align2::CENTER_CENTER,
                &lyrics.artist,
                FontId::proportional(h * 0.045),
                hi,
            );
        }
        countdown(&painter, rect, first_start - t, hi);
        progress_bar(&painter, rect, lyrics, t, duration, hi);
        return;
    }

    match current {
        Some(i) => {
            let line = &lyrics.lines[i];
            let since = (t - line.start) as f32;
            let enter = ease_out(since / 0.35);
            let progress = ((t - line.start) / (line.end - line.start).max(0.01)) as f32;
            // Sing a bit ahead of the end so the fill finishes on the last word.
            let progress = (progress / 0.88).min(1.0);

            // Only the sung words are highlighted; a (translation) sits below, unhighlighted.
            let (raw_sung, translation) = split_translation(&line.text);
            let has_sung = !raw_sung.is_empty();
            let sung = if has_sung { raw_sung } else { translation.clone().unwrap_or_default() };
            let g = centered_galley(&painter, &sung, base_size, Color32::WHITE, wrap);
            let alpha = (enter * 255.0) as u8;
            let tr_color = Color32::from_rgba_unmultiplied(220, 220, 230, (alpha as f32 * 0.75) as u8);
            let tg = translation
                .filter(|_| has_sung)
                .map(|t| centered_galley(&painter, &t, base_size * 0.5, tr_color, wrap));
            let tr_gap = h * 0.02;
            let block_h = g.size().y + tg.as_ref().map_or(0.0, |t| t.size().y + tr_gap);
            let y = rect.center().y - block_h / 2.0 + (1.0 - enter) * h * 0.04;
            let base = Color32::from_white_alpha(alpha);
            let hi_a = Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), alpha);
            // Soft shadow for legibility.
            painter.galley_with_override_text_color(pos2(rect.center().x + 2.0, y + 3.0), g.clone(), Color32::from_black_alpha(90));
            karaoke(&painter, &g, pos2(rect.center().x, y), base, hi_a, progress);
            if let Some(tg) = &tg {
                let ty = y + g.size().y + tr_gap;
                painter.galley_with_override_text_color(pos2(rect.center().x + 1.5, ty + 2.0), tg.clone(), Color32::from_black_alpha(80));
                painter.galley(pos2(rect.center().x, ty), tg.clone(), tr_color);
            }

            if i > 0 {
                let prev = &lyrics.lines[i - 1];
                let pg = centered_galley(&painter, &sung_only(&prev.text), base_size * 0.55, faint, wrap);
                painter.galley(pos2(rect.center().x, y - pg.size().y - h * 0.05), pg, faint);
            }
            if style.show_next_line
                && let Some(n) = lyrics.lines.get(i + 1)
            {
                let ng = centered_galley(&painter, &sung_only(&n.text), base_size * 0.6, dim, wrap);
                painter.galley(pos2(rect.center().x, y + block_h + h * 0.06), ng, dim);
            }
        }
        None => {
            // Instrumental break (or outro).
            if let Some(n) = next {
                let line = &lyrics.lines[n];
                let pulse = 0.6 + 0.4 * ((anim_time * 2.5).sin() as f32 * 0.5 + 0.5);
                painter.text(
                    rect.center() - vec2(0.0, h * 0.06),
                    Align2::CENTER_CENTER,
                    "♪   ♪   ♪",
                    FontId::proportional(h * 0.06),
                    hi.gamma_multiply(pulse),
                );
                if style.show_next_line {
                    let ng = centered_galley(&painter, &sung_only(&line.text), base_size * 0.6, dim, wrap);
                    painter.galley(pos2(rect.center().x, rect.center().y + h * 0.06), ng, dim);
                }
                countdown(&painter, rect, line.start - t, hi);
            } else {
                let title = if lyrics.title.is_empty() { "Thanks for singing!" } else { &lyrics.title };
                painter.text(rect.center(), Align2::CENTER_CENTER, title, FontId::proportional(h * 0.07), Color32::from_white_alpha(200));
            }
        }
    }

    // Watermark.
    if !lyrics.title.is_empty() {
        let label = if lyrics.artist.is_empty() {
            lyrics.title.clone()
        } else {
            format!("{}  —  {}", lyrics.title, lyrics.artist)
        };
        painter.text(
            rect.left_top() + vec2(h * 0.04, h * 0.04),
            Align2::LEFT_TOP,
            label,
            FontId::proportional((h * 0.03).max(9.0)),
            Color32::from_white_alpha(90),
        );
    }
    progress_bar(&painter, rect, lyrics, t, duration, hi);
}

/// Three dots that disappear one per second before a line starts.
fn countdown(painter: &Painter, rect: Rect, remaining: f64, color: Color32) {
    if !(0.0..=3.0).contains(&remaining) {
        return;
    }
    let h = rect.height();
    let n = remaining.ceil() as usize;
    let r = h * 0.012;
    let y = rect.center().y + h * 0.22;
    for i in 0..3 {
        let x = rect.center().x + (i as f32 - 1.0) * r * 4.0;
        let c = if i < n { color } else { color.gamma_multiply(0.15) };
        painter.circle_filled(pos2(x, y), r, c);
    }
}

fn progress_bar(painter: &Painter, rect: Rect, lyrics: &Lyrics, t: f64, duration: f64, color: Color32) {
    if !lyrics.style.show_progress || duration <= 0.0 {
        return;
    }
    let h = (rect.height() * 0.006).max(2.0);
    let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - h), rect.max);
    painter.rect_filled(bar, 0.0, Color32::from_white_alpha(25));
    let mut fill = bar;
    fill.set_right(bar.left() + bar.width() * (t / duration).clamp(0.0, 1.0) as f32);
    painter.rect_filled(fill, 0.0, color.gamma_multiply(0.85));
}

/// "Up next" card in the bottom-right corner (setlist mode).
pub fn paint_next_card(painter: &Painter, rect: Rect, title: &str, artist: &str, alpha: f32) {
    if alpha <= 0.01 {
        return;
    }
    let h = rect.height();
    let pad = h * 0.03;
    let label_font = FontId::proportional((h * 0.022).max(9.0));
    let title_font = FontId::proportional((h * 0.04).max(12.0));
    let label = painter.layout_no_wrap("UP NEXT".into(), label_font, Color32::from_white_alpha((150.0 * alpha) as u8));
    let mut text = title.to_owned();
    if !artist.is_empty() {
        text = format!("{title}  —  {artist}");
    }
    let tg = painter.layout(text, title_font, Color32::from_white_alpha((240.0 * alpha) as u8), rect.width() * 0.4);
    let w = tg.size().x.max(label.size().x) + pad * 2.0;
    let card = Rect::from_min_size(
        rect.right_bottom() - vec2(w + pad, label.size().y + tg.size().y + pad * 3.0 + h * 0.02),
        vec2(w, label.size().y + tg.size().y + pad * 2.2),
    );
    painter.rect_filled(card, h * 0.015, Color32::from_black_alpha((150.0 * alpha) as u8));
    painter.galley(card.min + vec2(pad, pad * 0.8), label.clone(), Color32::WHITE);
    painter.galley(card.min + vec2(pad, pad * 0.8 + label.size().y + pad * 0.3), tg, Color32::WHITE);
}

/// The sung part of a line (falls back to the translation for "(Chorus)"-style lines).
fn sung_only(text: &str) -> String {
    let (sung, tr) = split_translation(text);
    if sung.is_empty() { tr.unwrap_or_default() } else { sung }
}

/// Fit a 16:9 frame inside `avail`.
pub fn fit_16_9(avail: Rect) -> Rect {
    let aspect = 16.0 / 9.0;
    let (w, h) = if avail.width() / avail.height() > aspect {
        (avail.height() * aspect, avail.height())
    } else {
        (avail.width(), avail.width() / aspect)
    };
    Rect::from_center_size(avail.center(), vec2(w, h))
}
