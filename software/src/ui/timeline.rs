//! Multi-track timeline: ruler, stem tracks, lyric caption track, playhead.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Shape, Stroke,
    Ui, UiBuilder, pos2, vec2,
};

use crate::app::{LyricDragOp, StageWeaveApp};
use crate::theme::*;

use super::lyric_actions::{self, Act};
use super::widgets::{
    Icon, gain_fader, horizontal_meter, icon_button, paint_waveform, pan_knob, panel_header, toggle_chip,
};

const HEAD_W: f32 = 236.0;
const RULER_H: f32 = 28.0;
const SECTION_H: f32 = 18.0;
const LYRIC_H: f32 = 40.0;
const HSCROLL_H: f32 = 12.0;
const MAX_PX_PER_SEC: f32 = 240.0;

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    let mut tab = app.timeline_tab;
    let has_media = app.session.source.is_some();
    let mut zoom_by = None;
    let mut reset_cue_mix = false;
    let mut exit_cue = false;
    let mut reset_section_mix = None;
    let cue_name = app.cue.active.and_then(|i| app.session.cues.get(i)).map(|c| (c.name.clone(), c.mix.is_some()));
    let section_name = app.section.current.and_then(|i| {
        let s = app.session.sections.get(i)?;
        Some((i, s.name.clone(), app.section_mix(i) != app.song_mix()))
    });
    panel_header(ui, &["Timeline"], &mut tab, |ui| {
        if !has_media {
            return;
        }
        if let Some((name, own_mix)) = &cue_name {
            if ui.button("Exit Cue").on_hover_text("Stop looping and go back to the song's mix (Esc)").clicked() {
                exit_cue = true;
            }
            if *own_mix && ui.small_button("Use song mix").on_hover_text("Drop this cue's own mute / volume settings").clicked() {
                reset_cue_mix = true;
            }
            ui.label(
                RichText::new(format!("{name} mix: mute, solo and volume changes apply only inside this cue"))
                    .size(11.0)
                    .color(CUE),
            );
            ui.add_space(10.0);
        } else if let Some((i, name, own_mix)) = &section_name {
            if *own_mix && ui.small_button("Use song mix").on_hover_text("Drop this section's own mute / volume settings").clicked() {
                reset_section_mix = Some(*i);
            }
            ui.label(
                RichText::new(format!("{name} mix: mute, solo and volume changes apply only inside this section"))
                    .size(11.0)
                    .color(SECTION),
            );
            ui.add_space(10.0);
        }
        if icon_button(ui, Icon::Follow, 22.0, app.timeline.follow, "Follow playhead").clicked() {
            app.timeline.follow = !app.timeline.follow;
        }
        ui.add_space(6.0);
        if icon_button(ui, Icon::ZoomIn, 22.0, false, "Zoom in (⌘ + scroll)").clicked() {
            zoom_by = Some(1.5);
        }
        if ui.small_button("Fit").on_hover_text("Fit the whole song").clicked() {
            app.timeline.fit_request = true;
        }
        if icon_button(ui, Icon::ZoomOut, 22.0, false, "Zoom out").clicked() {
            zoom_by = Some(1.0 / 1.5);
        }
    });
    app.timeline_tab = tab;
    if reset_cue_mix {
        app.reset_cue_mix();
    }
    if exit_cue {
        app.exit_cue();
    }
    if let Some(i) = reset_section_mix {
        app.reset_section_mix(i);
    }

    let body = ui.available_rect_before_wrap();
    ui.allocate_rect(body, Sense::hover());

    if !has_media {
        empty_state(ui, body);
        return;
    }

    let duration = app.session.duration();
    let lanes_left = body.left() + HEAD_W;
    let lane_w = (body.right() - lanes_left - 8.0).max(50.0);
    let min_pps = (lane_w / duration as f32).max(0.5);

    let tl = &mut app.timeline;
    if tl.fit_request {
        tl.px_per_sec = min_pps;
        tl.scroll = 0.0;
        tl.fit_request = false;
    }
    if let Some(z) = zoom_by {
        // Zoom around the playhead.
        let anchor = app.playback.position();
        let x = (anchor - tl.scroll) as f32 * tl.px_per_sec;
        tl.px_per_sec = (tl.px_per_sec * z).clamp(min_pps, MAX_PX_PER_SEC);
        tl.scroll = anchor - (x / tl.px_per_sec) as f64;
    }
    tl.px_per_sec = tl.px_per_sec.clamp(min_pps, MAX_PX_PER_SEC);

    // Regions.
    let ruler = Rect::from_min_max(pos2(lanes_left, body.top()), pos2(body.right(), body.top() + RULER_H));
    let corner = Rect::from_min_max(body.min, pos2(lanes_left, ruler.bottom()));
    let section_lane = Rect::from_min_max(pos2(body.left(), ruler.bottom()), pos2(body.right(), ruler.bottom() + SECTION_H));
    let tracks_area = Rect::from_min_max(pos2(body.left(), section_lane.bottom()), pos2(body.right(), body.bottom() - HSCROLL_H));
    let hscroll = Rect::from_min_max(pos2(lanes_left, body.bottom() - HSCROLL_H), body.max);

    let rows = rows_for(app);
    let track_h = app.timeline.track_height;
    let content_h: f32 = rows.iter().map(|r| r.height(track_h)).sum();
    let max_v = (content_h - tracks_area.height()).max(0.0);

    // Wheel: ⌘ zooms, shift / horizontal scrolls time, vertical scrolls tracks.
    let hovered = ui.rect_contains_pointer(body);
    if hovered {
        let (delta, zoom, mods, pointer) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers, i.pointer.hover_pos()));
        let tl = &mut app.timeline;
        if zoom != 1.0 || (mods.command && delta.y != 0.0) {
            let factor = if zoom != 1.0 { zoom } else { (delta.y * 0.01).exp() };
            let px = pointer.map_or(0.0, |p| (p.x - lanes_left).max(0.0));
            let t_at = tl.scroll + (px / tl.px_per_sec) as f64;
            tl.px_per_sec = (tl.px_per_sec * factor).clamp(min_pps, MAX_PX_PER_SEC);
            tl.scroll = t_at - (px / tl.px_per_sec) as f64;
        } else {
            let (dx, dy) = if mods.shift { (delta.y, 0.0) } else { (delta.x, delta.y) };
            if dx != 0.0 {
                tl.scroll -= (dx / tl.px_per_sec) as f64;
                tl.follow = tl.follow && !app.playback.is_playing();
            }
            if dy != 0.0 {
                if max_v > 0.0 {
                    tl.v_scroll = (tl.v_scroll - dy).clamp(0.0, max_v);
                } else {
                    tl.scroll -= (dy / tl.px_per_sec) as f64;
                }
            }
        }
    }

    // Follow playhead (page-style).
    let pos = app.playback.position();
    let tl = &mut app.timeline;
    let visible = (lane_w / tl.px_per_sec) as f64;
    if tl.follow && app.playback.is_playing() && (pos > tl.scroll + visible * 0.92 || pos < tl.scroll) {
        tl.scroll = pos - visible * 0.08;
    }
    tl.scroll = tl.scroll.clamp(0.0, (duration - visible).max(0.0));
    tl.v_scroll = tl.v_scroll.clamp(0.0, max_v);

    let view = View { left: lanes_left, scroll: tl.scroll, pps: tl.px_per_sec };

    // Track rows (clipped to the scrollable area).
    let painter = ui.painter().with_clip_rect(tracks_area);
    painter.rect_filled(tracks_area, 0.0, BG_TRACK_LANE);
    let mut y = tracks_area.top() - app.timeline.v_scroll;
    for (ri, row) in rows.iter().enumerate() {
        let h = row.height(track_h);
        let row_rect = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), h));
        if row_rect.intersects(tracks_area) {
            match *row {
                Row::Track(i) => track_row(app, ui, &painter, tracks_area, row_rect, i, &view, ri),
                Row::Original => original_row(app, ui, &painter, row_rect, &view),
                Row::Lyrics => lyric_row(app, ui, &painter, tracks_area, row_rect, &view),
            }
        }
        y += h;
    }
    // Header column divider.
    painter.line_segment(
        [pos2(lanes_left, tracks_area.top()), pos2(lanes_left, tracks_area.bottom())],
        Stroke::new(1.0, BORDER_STRONG),
    );

    draw_ruler(app, ui, ruler, corner, &view, duration);
    draw_cues(app, ui, ruler, tracks_area, &view);
    draw_sections(app, ui, section_lane, lanes_left, tracks_area, &view);
    draw_hscroll(app, ui, hscroll, lane_w, duration);

    // Playhead across ruler + tracks.
    let px = view.x(pos);
    if px >= lanes_left - 1.0 && px <= body.right() {
        let full = ui.painter().with_clip_rect(Rect::from_min_max(pos2(lanes_left - 6.0, body.top()), pos2(body.right(), tracks_area.bottom())));
        full.line_segment([pos2(px, ruler.top() + 8.0), pos2(px, tracks_area.bottom())], Stroke::new(1.5, PLAYHEAD));
        let top = ruler.top() + 4.0;
        full.add(Shape::convex_polygon(
            vec![pos2(px - 6.0, top), pos2(px + 6.0, top), pos2(px + 6.0, top + 9.0), pos2(px, top + 15.0), pos2(px - 6.0, top + 9.0)],
            PLAYHEAD,
            Stroke::NONE,
        ));
    }

    // Vertical scrollbar indicator.
    if max_v > 0.0 {
        let frac = tracks_area.height() / content_h;
        let bar_h = tracks_area.height() * frac;
        let bar_y = tracks_area.top() + (app.timeline.v_scroll / max_v) * (tracks_area.height() - bar_h);
        let bar = Rect::from_min_size(pos2(body.right() - 5.0, bar_y), vec2(3.0, bar_h));
        ui.painter().rect_filled(bar, 1.5, Color32::from_white_alpha(50));
    }
}

#[derive(Clone, Copy)]
enum Row {
    Track(usize),
    /// Un-split source shown as a single clip.
    Original,
    Lyrics,
}

impl Row {
    fn height(&self, track_h: f32) -> f32 {
        match self {
            Row::Lyrics => LYRIC_H,
            _ => track_h,
        }
    }
}

fn rows_for(app: &StageWeaveApp) -> Vec<Row> {
    // Lyrics sit on top, like a caption track, so they never scroll out of view.
    let mut rows = Vec::new();
    if app.session.lyrics.is_generated() {
        rows.push(Row::Lyrics);
    }
    if app.has_tracks() {
        rows.extend((0..app.session.tracks.len()).map(Row::Track));
    } else {
        rows.push(Row::Original);
    }
    rows
}

struct View {
    left: f32,
    scroll: f64,
    pps: f32,
}

impl View {
    fn x(&self, t: f64) -> f32 {
        self.left + ((t - self.scroll) as f32) * self.pps
    }
    fn t(&self, x: f32) -> f64 {
        self.scroll + ((x - self.left) / self.pps) as f64
    }
}

fn empty_state(ui: &mut Ui, body: Rect) {
    let p = ui.painter();
    // Faux empty track lanes for texture.
    for i in 0..4 {
        let r = Rect::from_min_size(pos2(body.left(), body.top() + RULER_H + i as f32 * 60.0), vec2(body.width(), 60.0));
        if r.bottom() > body.bottom() {
            break;
        }
        p.rect_filled(r, 0.0, if i % 2 == 0 { BG_TRACK_LANE } else { BG_TRACK_LANE_ALT });
    }
    p.text(body.center() - vec2(0.0, 8.0), Align2::CENTER_CENTER, "Timeline is empty", FontId::proportional(15.0), TEXT_DIM);
    p.text(
        body.center() + vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        "Drop an audio file anywhere to import it",
        FontId::proportional(12.0),
        TEXT_FAINT,
    );
}

fn nice_step(pps: f32) -> (f64, f64) {
    const STEPS: [f64; 10] = [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0];
    let major = STEPS.iter().copied().find(|s| *s as f32 * pps >= 80.0).unwrap_or(600.0);
    let minor = if major >= 60.0 { major / 4.0 } else { major / 5.0 };
    (major, minor)
}

fn draw_ruler(app: &mut StageWeaveApp, ui: &mut Ui, ruler: Rect, corner: Rect, view: &View, duration: f64) {
    let resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    if (resp.clicked() || resp.dragged())
        && let Some(p) = resp.interact_pointer_pos()
    {
        app.seek(view.t(p.x));
    }
    let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);

    let painter = ui.painter().with_clip_rect(ruler);
    painter.rect_filled(ruler, 0.0, BG_PANEL_HEADER);
    painter.line_segment([ruler.left_bottom(), ruler.right_bottom()], Stroke::new(1.0, BORDER_STRONG));

    let (major, minor) = nice_step(view.pps);
    let t0 = view.t(ruler.left()).max(0.0);
    let t1 = view.t(ruler.right()).min(duration);
    let mut t = (t0 / minor).floor() * minor;
    while t <= t1 {
        let x = view.x(t);
        let is_major = ((t / major).round() * major - t).abs() < 1e-6;
        let h = if is_major { 10.0 } else { 5.0 };
        painter.line_segment(
            [pos2(x, ruler.bottom() - h), pos2(x, ruler.bottom())],
            Stroke::new(1.0, if is_major { TEXT_DIM } else { TEXT_FAINT }),
        );
        if is_major {
            let m = (t / 60.0).floor() as u32;
            let s = t % 60.0;
            let label = if major < 1.0 { format!("{m}:{s:04.1}") } else { format!("{m}:{:02}", s as u32) };
            painter.text(pos2(x + 4.0, ruler.top() + 9.0), Align2::LEFT_CENTER, label, FontId::monospace(10.0), TEXT_DIM);
        }
        t += minor;
    }
    // Loop region shading.
    if app.loop_enabled {
        let r = Rect::from_min_max(pos2(view.x(0.0), ruler.bottom() - 3.0), pos2(view.x(duration), ruler.bottom()));
        painter.rect_filled(r, 0.0, SOLO_ON.gamma_multiply(0.6));
    }
    if resp.hovered()
        && let Some(p) = resp.hover_pos()
    {
        painter.line_segment([pos2(p.x, ruler.top()), pos2(p.x, ruler.bottom())], Stroke::new(1.0, Color32::from_white_alpha(60)));
    }

    // Corner: current timecode + track count.
    let cp = ui.painter();
    cp.rect_filled(corner, 0.0, BG_PANEL_HEADER);
    cp.line_segment([corner.left_bottom(), corner.right_bottom()], Stroke::new(1.0, BORDER_STRONG));
    cp.text(corner.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, smpte(app.position()), FontId::monospace(12.0), ACCENT_BRIGHT);
    let n = app.session.tracks.len();
    if n > 0 {
        cp.text(
            corner.right_center() - vec2(12.0, 0.0),
            Align2::RIGHT_CENTER,
            format!("{n} tracks"),
            FontId::proportional(10.5),
            TEXT_FAINT,
        );
    }
}

/// Cue loops as bands on the ruler: click to loop one, right-click for options.
fn draw_cues(app: &mut StageWeaveApp, ui: &mut Ui, ruler: Rect, tracks: Rect, view: &View) {
    let painter = ui.painter().with_clip_rect(Rect::from_min_max(ruler.min, tracks.max));
    let band_y = (ruler.bottom() - 12.0, ruler.bottom() - 1.0);
    let pos = app.position();
    if let Some(start) = app.cue.recording {
        // Being marked: from the start to the playhead.
        let (a, b) = if pos < start { (pos, start) } else { (start, pos) };
        let r = Rect::from_min_max(pos2(view.x(a), band_y.0), pos2(view.x(b).max(view.x(a) + 2.0), band_y.1));
        let pulse = 0.55 + 0.45 * (ui.input(|i| i.time) * 4.0).sin().abs() as f32;
        painter.rect_filled(r, 2.0, DANGER.gamma_multiply(pulse));
        ui.ctx().request_repaint();
    }
    let mut act: Option<(usize, CueAct)> = None;
    for i in 0..app.session.cues.len() {
        let c = app.session.cues[i].clone();
        let active = app.cue.active == Some(i);
        let (x0, x1) = (view.x(c.start), view.x(c.end));
        if x1 < ruler.left() || x0 > ruler.right() {
            continue;
        }
        let band = Rect::from_min_max(pos2(x0, band_y.0), pos2(x1.max(x0 + 4.0), band_y.1));
        if active {
            let region = Rect::from_min_max(pos2(x0, tracks.top()), pos2(x1, tracks.bottom()));
            painter.rect_filled(region, 0.0, CUE.gamma_multiply(0.08));
            for x in [x0, x1] {
                painter.line_segment([pos2(x, ruler.bottom()), pos2(x, tracks.bottom())], Stroke::new(1.0, CUE.gamma_multiply(0.7)));
            }
        }
        let resp = ui.interact(band, ui.id().with(("cue", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, format!("Timeline {}", c.name)));
        let fill = if active { CUE } else if resp.hovered() { CUE.gamma_multiply(0.8) } else { CUE.gamma_multiply(0.5) };
        painter.rect_filled(band, 3.0, fill);
        let label = if c.mix.is_some() { format!("{} ·mix", c.name) } else { c.name.clone() };
        let galley = painter.layout_no_wrap(label, FontId::proportional(9.5), Color32::BLACK);
        if galley.size().x + 8.0 < band.width() {
            painter.galley(pos2(band.left() + 4.0, band.center().y - galley.size().y / 2.0), galley, Color32::BLACK);
        }
        let tip = format!("{}  {} – {}\nClick to loop it · right-click for options", c.name, timecode(c.start), timecode(c.end));
        let resp = resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            act = Some((i, if active { CueAct::Exit } else { CueAct::Loop }));
        }
        resp.context_menu(|ui| {
            ui.label(RichText::new(format!("{} – {}", timecode(c.start), timecode(c.end))).size(11.0).color(TEXT_FAINT));
            let mut name = c.name.clone();
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(140.0)).changed() {
                app.session.cues[i].name = name;
            }
            ui.separator();
            if ui.button(if active { "Exit Cue" } else { "Loop This Cue" }).clicked() {
                act = Some((i, if active { CueAct::Exit } else { CueAct::Loop }));
                ui.close();
            }
            if ui.button("Set Start to Playhead").clicked() {
                act = Some((i, CueAct::SetStart(pos)));
                ui.close();
            }
            if ui.button("Set End to Playhead").clicked() {
                act = Some((i, CueAct::SetEnd(pos)));
                ui.close();
            }
            if c.mix.is_some() && ui.button("Use Song Mix").on_hover_text("Drop this cue's own stem settings").clicked() {
                act = Some((i, CueAct::ResetMix));
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Delete Cue").color(DANGER)).clicked() {
                act = Some((i, CueAct::Delete));
                ui.close();
            }
        });
    }
    if let Some((i, a)) = act {
        match a {
            CueAct::Loop => app.activate_cue(i),
            CueAct::Exit => app.exit_cue(),
            CueAct::SetStart(t) | CueAct::SetEnd(t) => app.set_cue_bound(i, matches!(a, CueAct::SetStart(_)), t),
            CueAct::ResetMix => {
                if app.cue.active == Some(i) {
                    app.reset_cue_mix();
                } else {
                    app.session.cues[i].mix = None;
                    app.save_song_state();
                }
            }
            CueAct::Delete => app.delete_cue(i),
        }
    }
}

/// Sections in their own lane under the ruler: click to jump to one,
/// right-click to rename it or set its mix.
fn draw_sections(app: &mut StageWeaveApp, ui: &mut Ui, lane: Rect, lanes_left: f32, tracks: Rect, view: &View) {
    let head = Rect::from_min_max(lane.min, pos2(lanes_left, lane.bottom()));
    let lane = Rect::from_min_max(pos2(lanes_left, lane.top()), lane.max);
    let p = ui.painter();
    p.rect_filled(head, 0.0, BG_PANEL_HEADER);
    p.rect_filled(lane, 0.0, BG_PANEL_HEADER.gamma_multiply(0.85));
    p.line_segment([head.left_bottom(), lane.right_bottom()], Stroke::new(1.0, BORDER_STRONG));
    p.line_segment([pos2(lanes_left, lane.top()), pos2(lanes_left, lane.bottom())], Stroke::new(1.0, BORDER_STRONG));
    p.text(head.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, "SECTIONS", FontId::proportional(9.5), TEXT_FAINT);

    // Clicking the empty lane seeks, like the ruler.
    let bg = ui.interact(lane, ui.id().with("section_lane"), Sense::click());
    if bg.clicked()
        && let Some(pt) = bg.interact_pointer_pos()
    {
        app.seek(view.t(pt.x));
    }
    if app.session.sections.is_empty() && app.section.recording.is_none() {
        bg.on_hover_text("While playing, press S where a part starts and S again where it ends. Then give it its own mutes and volumes.");
        p.text(lane.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, "S: mark a section with its own mix", FontId::proportional(10.0), TEXT_FAINT);
    }

    let painter = ui.painter().with_clip_rect(Rect::from_min_max(lane.min, tracks.max));
    let band_y = (lane.top() + 2.0, lane.bottom() - 2.0);
    let pos = app.position();
    if let Some(start) = app.section.recording {
        let (a, b) = if pos < start { (pos, start) } else { (start, pos) };
        let r = Rect::from_min_max(pos2(view.x(a), band_y.0), pos2(view.x(b).max(view.x(a) + 2.0), band_y.1));
        let pulse = 0.55 + 0.45 * (ui.input(|i| i.time) * 4.0).sin().abs() as f32;
        painter.rect_filled(r, 3.0, DANGER.gamma_multiply(pulse));
        ui.ctx().request_repaint();
    }

    let song = app.song_mix();
    let stems: Vec<(crate::model::StemKind, String, Color32)> = app.session.tracks.iter().map(|t| (t.kind, t.name.clone(), t.color)).collect();
    let mut act: Option<(usize, SectionAct)> = None;
    for i in 0..app.session.sections.len() {
        let s = app.session.sections[i].clone();
        let current = app.section.current == Some(i);
        let mix = app.section_mix(i);
        let own = mix != song;
        let (x0, x1) = (view.x(s.start), view.x(s.end));
        if x1 < lane.left() || x0 > lane.right() {
            continue;
        }
        let band = Rect::from_min_max(pos2(x0 + 1.0, band_y.0), pos2(x1.max(x0 + 5.0) - 1.0, band_y.1));
        if current {
            let region = Rect::from_min_max(pos2(x0, tracks.top()), pos2(x1, tracks.bottom()));
            painter.rect_filled(region, 0.0, SECTION.gamma_multiply(0.05));
        }
        let resp = ui.interact(band, ui.id().with(("section", i)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, current, format!("Section {}", s.name)));
        let fill = if current {
            SECTION
        } else if resp.hovered() {
            SECTION.gamma_multiply(0.75)
        } else {
            SECTION.gamma_multiply(if own { 0.55 } else { 0.35 })
        };
        painter.rect_filled(band, 3.0, fill);

        // Name, then the stems it silences (struck through).
        let silenced: Vec<crate::model::StemKind> = mix.iter().filter(|m| m.muted && !song.iter().any(|b| b.kind == m.kind && b.muted)).map(|m| m.kind).collect();
        let galley = painter.layout_no_wrap(s.name.clone(), FontId::proportional(10.0), Color32::BLACK);
        let mut x = band.left() + 5.0;
        if galley.size().x + 10.0 < band.width() {
            painter.galley(pos2(x, band.center().y - galley.size().y / 2.0), galley.clone(), Color32::BLACK);
            x += galley.size().x + 6.0;
            for kind in &silenced {
                if x + 12.0 > band.right() - 3.0 {
                    break;
                }
                let r = Rect::from_center_size(pos2(x + 5.5, band.center().y), vec2(11.0, 11.0));
                super::widgets::paint_stem_icon(&painter, r, *kind, Color32::from_black_alpha(200));
                painter.line_segment([r.left_bottom() + vec2(-1.0, 1.0), r.right_top() + vec2(1.0, -1.0)], Stroke::new(1.5, DANGER));
                x += 14.0;
            }
        }

        let tip = format!(
            "{}  {} – {}\n{}\nClick to jump here · right-click to set its mix",
            s.name,
            timecode(s.start),
            timecode(s.end),
            mix_summary(&mix, &song, &stems),
        );
        let resp = resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            act = Some((i, SectionAct::Jump));
        }
        resp.context_menu(|ui| {
            ui.set_min_width(250.0);
            ui.label(RichText::new(format!("{} – {}", timecode(s.start), timecode(s.end))).size(11.0).color(TEXT_FAINT));
            let mut name = s.name.clone();
            if ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY)).changed() {
                app.session.sections[i].name = name;
                app.save_song_state();
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for preset in ["Intro", "Verse", "Pre-Chorus", "Chorus", "Bridge", "Tag", "Outro"] {
                    if ui.small_button(preset).clicked() {
                        app.session.sections[i].name = preset.to_owned();
                        app.save_song_state();
                    }
                }
            });
            ui.separator();
            ui.label(RichText::new("Section mix").size(11.0).color(TEXT_DIM));
            if stems.is_empty() {
                ui.label(RichText::new("Split the song into stems to give this section its own mix.").size(11.0).color(TEXT_FAINT));
            }
            for (kind, label, color) in &stems {
                let Some(m) = mix.iter().find(|m| m.kind == *kind).cloned() else { continue };
                ui.horizontal(|ui| {
                    super::widgets::stem_badge(ui, 18.0, *kind, *color, !m.muted);
                    ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(label).size(12.0)).truncate());
                    let chip = toggle_chip(ui, "M", m.muted, MUTE_ON, "Mute in this section");
                    chip.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, m.muted, format!("Mute {label} in section")));
                    if chip.clicked() {
                        app.edit_section_stem(i, *kind, |t| t.muted = !t.muted);
                    }
                    let mut db = m.gain_db;
                    if gain_fader(ui, &mut db, 110.0, *color).changed() {
                        app.edit_section_stem(i, *kind, |t| t.gain_db = db);
                    }
                    ui.label(RichText::new(if db <= crate::model::MIN_DB { "-∞".to_owned() } else { format!("{db:+.0} dB") }).size(10.5).color(TEXT_FAINT));
                });
            }
            if own && ui.button("Use Song Mix").on_hover_text("Drop this section's own stem settings").clicked() {
                act = Some((i, SectionAct::ResetMix));
                ui.close();
            }
            ui.separator();
            if ui.button("Set Start to Playhead").clicked() {
                act = Some((i, SectionAct::SetStart(pos)));
                ui.close();
            }
            if ui.button("Set End to Playhead").clicked() {
                act = Some((i, SectionAct::SetEnd(pos)));
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Delete Section").color(DANGER)).clicked() {
                act = Some((i, SectionAct::Delete));
                ui.close();
            }
        });
    }
    if let Some((i, a)) = act {
        match a {
            SectionAct::Jump => {
                if let Some(s) = app.session.sections.get(i) {
                    let t = s.start;
                    app.seek(t);
                }
            }
            SectionAct::SetStart(t) | SectionAct::SetEnd(t) => app.set_section_bound(i, matches!(a, SectionAct::SetStart(_)), t),
            SectionAct::ResetMix => app.reset_section_mix(i),
            SectionAct::Delete => app.delete_section(i),
        }
    }
}

/// "Muted: Drums, Bass · Vocals −6 dB" — how a section differs from the song.
fn mix_summary(mix: &[crate::model::TrackMix], song: &[crate::model::TrackMix], stems: &[(crate::model::StemKind, String, Color32)]) -> String {
    let name = |k: crate::model::StemKind| stems.iter().find(|s| s.0 == k).map_or(k.label().to_owned(), |s| s.1.clone());
    let base = |k| song.iter().find(|b| b.kind == k);
    let muted: Vec<String> = mix.iter().filter(|m| m.muted && !base(m.kind).is_some_and(|b| b.muted)).map(|m| name(m.kind)).collect();
    let mut parts = Vec::new();
    if !muted.is_empty() {
        parts.push(format!("Muted: {}", muted.join(", ")));
    }
    for m in mix.iter().filter(|m| !m.muted) {
        if let Some(b) = base(m.kind)
            && (m.gain_db - b.gain_db).abs() >= 0.5
        {
            parts.push(format!("{} {:+.0} dB", name(m.kind), m.gain_db - b.gain_db));
        }
    }
    if parts.is_empty() { "Song mix".to_owned() } else { parts.join(" · ") }
}

#[derive(Clone, Copy)]
enum SectionAct {
    Jump,
    SetStart(f64),
    SetEnd(f64),
    ResetMix,
    Delete,
}

#[derive(Clone, Copy)]
enum CueAct {
    Loop,
    Exit,
    SetStart(f64),
    SetEnd(f64),
    ResetMix,
    Delete,
}

fn draw_hscroll(app: &mut StageWeaveApp, ui: &mut Ui, rect: Rect, lane_w: f32, duration: f64) {
    let p = ui.painter();
    p.rect_filled(rect, 0.0, BG_PANEL);
    let visible = (lane_w / app.timeline.px_per_sec) as f64;
    if visible >= duration - 0.01 {
        return;
    }
    let track = rect.shrink2(vec2(2.0, 3.0));
    let frac_w = (visible / duration) as f32;
    let frac_x = (app.timeline.scroll / duration) as f32;
    let thumb = Rect::from_min_size(
        pos2(track.left() + track.width() * frac_x, track.top()),
        vec2((track.width() * frac_w).max(20.0), track.height()),
    );
    let resp = ui.interact(thumb, ui.id().with("hscroll"), Sense::drag());
    if resp.dragged() {
        app.timeline.scroll += (resp.drag_delta().x / track.width()) as f64 * duration;
        app.timeline.follow = false;
    }
    let c = if resp.hovered() || resp.dragged() { Color32::from_white_alpha(90) } else { Color32::from_white_alpha(45) };
    ui.painter().rect_filled(thumb, 3.0, c);
}

/// Shared lane background + click-to-seek for a row. Returns the lane rect.
fn lane_bg(painter: &egui::Painter, row: Rect, lanes_left: f32, alt: bool, selected: bool) -> Rect {
    let lane = Rect::from_min_max(pos2(lanes_left, row.top()), row.max);
    let fill = if selected {
        Color32::from_rgb(33, 36, 44)
    } else if alt {
        BG_TRACK_LANE_ALT
    } else {
        BG_TRACK_LANE
    };
    painter.rect_filled(lane, 0.0, fill);
    painter.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, BG_APP));
    lane
}

#[allow(clippy::too_many_arguments)]
fn track_row(
    app: &mut StageWeaveApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    clip: Rect,
    row: Rect,
    i: usize,
    view: &View,
    row_index: usize,
) {
    let selected = app.selected_track == Some(i);
    let audible = app.session.is_audible(i);
    let duration = app.session.duration();
    let lane = lane_bg(painter, row, view.left, row_index % 2 == 1, selected);

    // ---------- clip in lane
    let lane_resp = ui.interact(lane.intersect(clip), ui.id().with(("lane", i)), Sense::click_and_drag());
    if (lane_resp.clicked() || lane_resp.dragged())
        && let Some(p) = lane_resp.interact_pointer_pos()
    {
        app.seek(view.t(p.x));
        app.selected_track = Some(i);
        app.lyric_edit.clip_selected = false;
    }

    let t = &app.session.tracks[i];
    let clip_rect = Rect::from_min_max(pos2(view.x(0.0), row.top() + 4.0), pos2(view.x(duration), row.bottom() - 5.0));
    let lane_painter = painter.with_clip_rect(lane.intersect(clip));
    let base = if audible { t.color } else { Color32::from_gray(110) };
    lane_painter.rect_filled(clip_rect, 4.0, base.gamma_multiply(if audible { 0.22 } else { 0.12 }));
    let label_strip = Rect::from_min_max(clip_rect.min, pos2(clip_rect.right(), clip_rect.top() + 15.0));
    lane_painter.rect_filled(label_strip, CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, base.gamma_multiply(if audible { 0.55 } else { 0.25 }));
    let label_x = clip_rect.left().max(lane.left()) + 6.0;
    lane_painter.text(
        pos2(label_x, label_strip.center().y),
        Align2::LEFT_CENTER,
        &t.name,
        FontId::proportional(10.5),
        if audible { Color32::from_black_alpha(220) } else { TEXT_FAINT },
    );
    let wave_rect = Rect::from_min_max(pos2(lane.left().max(clip_rect.left()), label_strip.bottom() + 2.0), pos2(lane.right().min(clip_rect.right()), clip_rect.bottom() - 2.0));
    let spp = 1.0 / view.pps as f64;
    let gain = t.gain_linear().min(1.6);
    paint_waveform(&lane_painter, wave_rect, &t.peaks, view.t(wave_rect.left()), spp, base.gamma_multiply(if audible { 0.95 } else { 0.4 }), gain);
    if selected {
        lane_painter.rect_stroke(clip_rect, 4.0, Stroke::new(1.0, Color32::from_white_alpha(120)), egui::StrokeKind::Inside);
    }

    // ---------- header
    let head = Rect::from_min_max(row.min, pos2(view.left, row.bottom()));
    let head_resp = ui.interact(head.intersect(clip), ui.id().with(("head", i)), Sense::click());
    let head_fill = if selected { Color32::from_rgb(44, 47, 56) } else { BG_TRACK_HEADER };
    painter.rect_filled(head, 0.0, head_fill);
    painter.line_segment([head.left_bottom(), head.right_bottom()], Stroke::new(1.0, BG_APP));
    painter.rect_filled(Rect::from_min_size(head.min, vec2(4.0, head.height())), 0.0, t.color.gamma_multiply(if audible { 1.0 } else { 0.35 }));
    if head_resp.clicked() {
        app.selected_track = Some(i);
        app.editor_tab = 1;
    }

    let inner = head.shrink2(vec2(12.0, 7.0)).translate(vec2(3.0, 0.0));
    let mut changed = false;
    let meter = t.meter;
    ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)), |ui| {
        ui.set_clip_rect(clip.intersect(head));
        let t = &mut app.session.tracks[i];
        ui.horizontal(|ui| {
            super::widgets::stem_badge(ui, 16.0, t.kind, t.color, audible).on_hover_text(t.kind.label());
            ui.add(egui::Label::new(RichText::new(&t.name).strong().color(if audible { Color32::WHITE } else { TEXT_DIM })).truncate().selectable(false));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if toggle_chip(ui, "S", t.solo, SOLO_ON, "Solo").clicked() {
                    t.solo = !t.solo;
                    changed = true;
                }
                if toggle_chip(ui, "M", t.muted, MUTE_ON, "Mute").clicked() {
                    t.muted = !t.muted;
                    changed = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            changed |= pan_knob(ui, &mut t.pan).changed();
            changed |= gain_fader(ui, &mut t.gain_db, 118.0, t.color).changed();
            ui.label(RichText::new(db_label(t.gain_db)).monospace().size(10.0).color(TEXT_DIM));
        });
        ui.add_space(1.0);
        let (m, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
        horizontal_meter(ui.painter(), m, meter);
    });
    if changed {
        app.sync_mix();
    }
}

fn original_row(app: &mut StageWeaveApp, ui: &mut Ui, painter: &egui::Painter, row: Rect, view: &View) {
    let Some(src) = &app.session.source else { return };
    let lane = lane_bg(painter, row, view.left, false, false);
    let duration = src.info.duration;
    let clip_rect = Rect::from_min_max(pos2(view.x(0.0), row.top() + 4.0), pos2(view.x(duration), row.bottom() - 5.0));
    let lp = painter.with_clip_rect(lane);
    let c = Color32::from_rgb(110, 130, 160);
    lp.rect_filled(clip_rect, 4.0, c.gamma_multiply(0.18));
    let wave = Rect::from_min_max(pos2(lane.left(), clip_rect.top() + 4.0), pos2(lane.right(), clip_rect.bottom() - 4.0));
    paint_waveform(&lp, wave, &src.peaks, view.t(wave.left()), 1.0 / view.pps as f64, c.gamma_multiply(0.6), 1.0);

    let busy = app.current_song_id().is_some_and(|id| app.queue.state(id).is_some());
    let msg = if busy {
        "Separating stems…"
    } else if app.is_loading() {
        "Loading stems…"
    } else {
        "Separate stems to unlock individual tracks"
    };
    let galley = lp.layout_no_wrap(msg.to_owned(), FontId::proportional(12.0), Color32::WHITE);
    let pill = Rect::from_center_size(lane.center(), galley.size() + vec2(24.0, 12.0));
    lp.rect_filled(pill, 12.0, Color32::from_black_alpha(170));
    lp.galley(pill.center() - galley.size() / 2.0, galley, Color32::WHITE);

    let head = Rect::from_min_max(row.min, pos2(view.left, row.bottom()));
    painter.rect_filled(head, 0.0, BG_TRACK_HEADER);
    painter.rect_filled(Rect::from_min_size(head.min, vec2(4.0, head.height())), 0.0, c);
    painter.text(head.left_top() + vec2(16.0, 16.0), Align2::LEFT_CENTER, "Original Mix", FontId::proportional(13.0), Color32::WHITE);
    painter.text(head.left_top() + vec2(16.0, 34.0), Align2::LEFT_CENTER, &src.info.file_name, FontId::proportional(10.5), TEXT_FAINT);
    let _ = ui;
}

/// Where on a lyric block the pointer is.
#[derive(Clone, Copy, PartialEq)]
enum Grip {
    Left,
    Right,
    Body,
}

/// Which part of block `r` is under `x`. Edge grips shrink on narrow
/// blocks (zoomed out) so the body can still be grabbed.
fn grip_at(r: Rect, x: f32) -> Grip {
    let h = (r.width() / 4.0).clamp(1.0, 5.0);
    if x <= r.left() + h {
        Grip::Left
    } else if x >= r.right() - h {
        Grip::Right
    } else {
        Grip::Body
    }
}

/// The drag a grip starts. Touching lines share a boundary handle that
/// moves both (hold Alt to move one side only); Alt on the body copies.
fn drag_op(lines: &[crate::model::LyricLine], i: usize, grip: Grip, alt: bool, pps: f32) -> LyricDragOp {
    let touching = |a: usize| (lines[a + 1].start - lines[a].end).abs() * pps as f64 <= 2.5;
    match grip {
        Grip::Left if !alt && i > 0 && touching(i - 1) => LyricDragOp::Split { line: i - 1 },
        Grip::Left => LyricDragOp::Edge { line: i, start: true },
        Grip::Right if !alt && i + 1 < lines.len() && touching(i) => LyricDragOp::Split { line: i },
        Grip::Right => LyricDragOp::Edge { line: i, start: false },
        Grip::Body if alt => LyricDragOp::Copy,
        Grip::Body => LyricDragOp::Move,
    }
}

/// Times a dragged edge or block sticks to: the playhead, other lines'
/// edges, sections and cues.
fn snap_points(app: &StageWeaveApp) -> Vec<f64> {
    let Some(d) = app.timeline.lyric_drag.as_ref() else { return Vec::new() };
    let skip: &[usize] = match d.op {
        LyricDragOp::Edge { line, .. } => &[line],
        LyricDragOp::Split { line } => &[line, line + 1],
        LyricDragOp::Move => &d.lines,
        LyricDragOp::Copy => &[],
    };
    let mut pts = vec![app.position()];
    for (k, l) in d.origin.iter().enumerate() {
        if !skip.contains(&k) {
            pts.extend([l.start, l.end]);
        }
    }
    pts.extend(app.session.sections.iter().flat_map(|s| [s.start, s.end]));
    pts.extend(app.session.cues.iter().flat_map(|c| [c.start, c.end]));
    pts
}

fn lyric_row(app: &mut StageWeaveApp, ui: &mut Ui, painter: &egui::Painter, clip: Rect, row: Rect, view: &View) {
    let lane = lane_bg(painter, row, view.left, true, false);
    let (alt, cmd, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.command, i.modifiers.shift));

    // A drag in progress follows the pointer, wherever it is.
    if app.timeline.lyric_drag.is_some() {
        let (down, ptr) = ui.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
        if let Some(p) = ptr {
            let pts = snap_points(app);
            let snap = (!cmd).then_some((pts.as_slice(), 8.0 / view.pps as f64));
            app.lyric_drag_update(view.t(p.x), snap);
        }
        if !down {
            app.lyric_drag_end();
        }
    }
    let drag = app.timeline.lyric_drag.as_ref().map(|d| (d.op, d.shown_t));

    // Clicking empty lane: clear the selection and move the playhead.
    let bg = ui.interact(lane.intersect(clip), ui.id().with("lyric_lane"), Sense::click());
    if bg.clicked()
        && let Some(p) = bg.interact_pointer_pos()
    {
        app.lyric_edit.clip_selected = false;
        app.lyric_edit.multi.clear();
        app.seek(view.t(p.x));
    }

    let pos = app.position();
    let current = app.session.lyrics.line_at(pos);
    let selected = app.lyric_selection();
    let editing = app.lyric_edit.editing.as_ref().filter(|_| !app.lyric_edit.edit_in_list).map(|(i, _)| *i);
    let lp = painter.with_clip_rect(lane.intersect(clip));
    let lines = &app.session.lyrics.lines;
    // Blocks keep a minimum width so short lines stay visible zoomed out.
    let rects: Vec<Rect> = lines
        .iter()
        .map(|l| {
            let x0 = view.x(l.start) + 1.0;
            Rect::from_min_max(pos2(x0, row.top() + 6.0), pos2((view.x(l.end) - 1.0).max(x0 + 3.0), row.bottom() - 6.0))
        })
        .collect();
    let mut act = None;
    let mut start_drag = None;
    let mut edit_rect = None;
    for (i, line) in lines.iter().enumerate() {
        let r = rects[i];
        if editing == Some(i) {
            edit_rect = Some(r);
        }
        if r.right() < lane.left() - 8.0 || r.left() > lane.right() + 8.0 {
            continue;
        }
        let active = current == Some(i);
        let tap = app.tap_sync == Some(i);
        let is_sel = selected.contains(&i);
        let fill = if tap {
            SOLO_ON.gamma_multiply(0.7)
        } else if active {
            Color32::from_rgb(215, 215, 225)
        } else if is_sel {
            Color32::from_rgb(52, 74, 110)
        } else {
            Color32::from_rgb(70, 72, 82)
        };
        lp.rect_filled(r, 3.0, fill);
        // Word timings (from Align): a tick where each word starts.
        if line.words.len() > 1 && line.words.len() == line.text.split_whitespace().count() {
            let tick = if active || tap { Color32::from_black_alpha(110) } else { Color32::from_white_alpha(70) };
            let mut last_x = f32::NEG_INFINITY;
            for w in &line.words[1..] {
                let x = view.x(w.start);
                if x > r.left() + 2.0 && x < r.right() - 2.0 && x - last_x >= 3.0 {
                    lp.line_segment([pos2(x, r.bottom() - 5.0), pos2(x, r.bottom() - 1.0)], Stroke::new(1.0, tick));
                    last_x = x;
                }
            }
        }
        if is_sel {
            lp.rect_stroke(r, 3.0, Stroke::new(2.0, ACCENT_BRIGHT), egui::StrokeKind::Inside);
        }
        let text_c = if active || tap { Color32::BLACK } else { TEXT };
        let shown = if line.text.trim().is_empty() { "(empty)" } else { line.text.as_str() };
        lp.with_clip_rect(r.shrink(3.0).intersect(lane)).text(
            pos2(r.left() + 6.0, r.center().y),
            Align2::LEFT_CENTER,
            shown,
            FontId::proportional(11.0),
            if line.text.trim().is_empty() { TEXT_FAINT } else { text_c },
        );

        // Hit area: the block plus up to 4 px of free space either side,
        // so edges are easy to catch even on thin blocks.
        let pad_l = if i == 0 { 4.0 } else { ((r.left() - rects[i - 1].right()) / 2.0).clamp(0.0, 4.0) };
        let pad_r = rects.get(i + 1).map_or(4.0, |n| ((n.left() - r.right()) / 2.0).clamp(0.0, 4.0));
        let hit = Rect::from_min_max(pos2(r.left() - pad_l, r.top()), pos2(r.right() + pad_r, r.bottom()));
        let resp = ui.interact(hit.intersect(lane).intersect(clip), ui.id().with(("lyric", i)), Sense::click_and_drag());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, is_sel, format!("Lyric {}", line.text)));
        if drag.is_none()
            && editing.is_none()
            && let Some(p) = resp.hover_pos()
        {
            let grip = grip_at(r, p.x);
            ui.ctx().set_cursor_icon(if grip == Grip::Body { egui::CursorIcon::Grab } else { egui::CursorIcon::ResizeHorizontal });
        }
        if resp.drag_started()
            && let Some(p) = ui.input(|inp| inp.pointer.press_origin())
        {
            start_drag = Some((drag_op(lines, i, grip_at(r, p.x), alt, view.pps), i, view.t(p.x)));
        } else if resp.double_clicked() {
            act = Some(Act::Edit(i, false));
        } else if resp.clicked() {
            act = Some(if shift {
                Act::SelectRange(i)
            } else if cmd {
                Act::Toggle(i)
            } else {
                Act::Select(i)
            });
        }
        let resp = resp.on_hover_text(format!(
            "{}  ·  {}\nClick to select (Shift / {} for more) · drag to move · drag edges to retime\nAlt-drag to copy · double-click to edit · right-click for more",
            timecode(line.start),
            line.text,
            if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" },
        ));
        resp.context_menu(|ui| {
            if let Some(a) = lyric_actions::context_menu(app, ui, i, false) {
                act = Some(a);
            }
        });
    }

    // While dragging: mark the edge being moved and show its time.
    if let Some((op, shown_t)) = drag {
        let x = view.x(shown_t);
        match op {
            LyricDragOp::Edge { .. } | LyricDragOp::Split { .. } => {
                lp.line_segment([pos2(x, row.top() + 2.0), pos2(x, row.bottom() - 2.0)], Stroke::new(2.0, ACCENT_BRIGHT));
            }
            LyricDragOp::Move | LyricDragOp::Copy => {}
        }
        ui.ctx().set_cursor_icon(match op {
            LyricDragOp::Move => egui::CursorIcon::Grabbing,
            LyricDragOp::Copy => egui::CursorIcon::Copy,
            _ => egui::CursorIcon::ResizeHorizontal,
        });
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            let tip = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("lyric_drag_tip")));
            let text = tip.layout_no_wrap(timecode(shown_t), FontId::monospace(11.0), Color32::WHITE);
            let at = pos2(p.x + 12.0, row.top() - text.size().y - 6.0);
            tip.rect_filled(Rect::from_min_size(at, text.size()).expand(4.0), 4.0, BG_PANEL_HEADER);
            tip.galley(at, text, Color32::WHITE);
        }
    }

    if let Some((op, i, t)) = start_drag {
        app.lyric_drag_begin(op, i, t);
    } else if let Some(a) = act {
        let ctx = ui.ctx().clone();
        lyric_actions::apply(app, &ctx, a, false);
    }
    if let Some(r) = edit_rect {
        inline_editor(app, ui, r, lane);
    }

    let head = Rect::from_min_max(row.min, pos2(view.left, row.bottom()));
    painter.rect_filled(head, 0.0, BG_TRACK_HEADER);
    painter.rect_filled(Rect::from_min_size(head.min, vec2(4.0, head.height())), 0.0, Color32::from_gray(200));
    painter.text(head.left_top() + vec2(16.0, 13.0), Align2::LEFT_CENTER, "Lyrics", FontId::proportional(12.5), Color32::WHITE);
    painter.text(head.left_top() + vec2(16.0, 28.0), Align2::LEFT_CENTER, "Right-click a line to edit", FontId::proportional(9.5), TEXT_FAINT);
    painter.text(
        head.right_center() - vec2(12.0, 0.0),
        Align2::RIGHT_CENTER,
        format!("{} lines", app.session.lyrics.lines.len()),
        FontId::proportional(10.5),
        TEXT_FAINT,
    );
}

/// Text box floating over a lyric clip. Enter saves, Esc cancels, clicking away saves.
fn inline_editor(app: &mut StageWeaveApp, ui: &mut Ui, clip_rect: Rect, lane: Rect) {
    let Some((line, mut draft)) = app.lyric_edit.editing.clone() else { return };
    let width = clip_rect.width().clamp(380.0, 560.0);
    let x = clip_rect.left().clamp(lane.left(), (lane.right() - width).max(lane.left()));
    let mut commit = false;
    let mut cancel = false;
    egui::Area::new(egui::Id::new(("lyric_inline_edit", line)))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(x, clip_rect.top() - 4.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(BG_PANEL_HEADER)
                .stroke(Stroke::new(1.5, ACCENT_BRIGHT))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(6, 4))
                .shadow(egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(140) })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}", line + 1)).monospace().size(11.0).color(TEXT_FAINT));
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut draft)
                                .hint_text("Type the line…")
                                .font(FontId::proportional(13.0))
                                .desired_width(width - 90.0),
                        );
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                        if esc {
                            cancel = true;
                        } else if r.lost_focus() && (enter || !r.has_focus()) {
                            commit = true;
                        }
                        if ui.small_button("Save").clicked() {
                            commit = true;
                        }
                    });
                });
        });
    if cancel {
        app.lyric_cancel_text();
    } else if commit {
        app.lyric_edit.editing = Some((line, draft));
        app.lyric_commit_text();
    } else {
        app.lyric_edit.editing = Some((line, draft));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn clicking_a_cue_band_loops_it() {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app.session.cues.push(crate::model::Cue { name: "Cue 1".into(), start: 20.0, end: 60.0, mix: None });
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 400.0)).build_ui_state(|ui, app: &mut StageWeaveApp| draw(app, ui), app);
        h.run_steps(3);
        h.get_by_label("Timeline Cue 1").click();
        h.run_steps(2);
        assert_eq!(h.state().cue.active, Some(0));
        h.get_by_label("Exit Cue").click(); // banner above the tracks
        h.run_steps(2);
        assert!(h.state().cue.active.is_none());
    }

    #[test]
    fn a_section_mutes_a_stem_from_its_menu() {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app.session.tracks = [crate::model::StemKind::Vocals, crate::model::StemKind::Drums]
            .into_iter()
            .map(|k| crate::model::Track::new(k, crate::model::Peaks::default(), None, None))
            .collect();
        app.session.sections.push(crate::model::Section { name: "Chorus".into(), start: 20.0, end: 60.0, mix: None });
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 400.0)).build_ui_state(|ui, app: &mut StageWeaveApp| draw(app, ui), app);
        h.run_steps(3);
        h.get_by_label("Section Chorus").click_secondary();
        h.run_steps(2);
        h.get_by_label("Mute Drums in section").click();
        h.run_steps(2);
        let a = h.state();
        let mix = a.session.sections[0].mix.as_ref().expect("section has its own mix");
        assert!(mix.iter().find(|m| m.kind == crate::model::StemKind::Drums).unwrap().muted);
        assert!(!a.session.tracks[1].muted, "the song's mix is untouched");
        // Clicking the band jumps there, without looping.
        h.get_by_label("Section Chorus").click();
        h.run_steps(2);
        assert!((h.state().position() - 20.0).abs() < 0.2);
        assert!(h.state().cue.active.is_none());
    }

    fn lyric_app() -> StageWeaveApp {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app.session.lyrics.lines = ["A", "B", "C"]
            .iter()
            .enumerate()
            .map(|(i, t)| crate::model::LyricLine::new(*t, i as f64 * 10.0, i as f64 * 10.0 + 4.0))
            .collect();
        app
    }

    fn harness(app: StageWeaveApp) -> Harness<'static, StageWeaveApp> {
        let mut h = Harness::builder().with_size(egui::vec2(1200.0, 400.0)).build_ui_state(|ui, app: &mut StageWeaveApp| draw(app, ui), app);
        h.run_steps(3);
        h
    }

    /// Press at `from`, move to `to` in steps, release.
    fn drag(h: &mut Harness<'static, StageWeaveApp>, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
        h.event(egui::Event::ModifiersChanged(modifiers));
        h.hover_at(from);
        h.run_steps(1);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers });
        h.run_steps(1);
        for k in 1..=4 {
            h.hover_at(from + (to - from) * (k as f32 / 4.0));
            h.run_steps(1);
        }
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers });
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        h.run_steps(2);
    }

    fn spans(app: &StageWeaveApp) -> Vec<(f64, f64)> {
        app.session.lyrics.lines.iter().map(|l| ((l.start * 100.0).round() / 100.0, (l.end * 100.0).round() / 100.0)).collect()
    }

    #[test]
    fn dragging_a_lyric_edge_when_zoomed_out_retimes_it() {
        let mut h = harness(lyric_app());
        let pps = h.state().timeline.px_per_sec;
        assert!(pps < 10.0, "fit zoom shows the whole song ({pps} px/s)");
        let r = h.get_by_label("Lyric B").rect();
        // Right edge, 3 s later.
        let from = egui::pos2(r.right() - 1.0, r.center().y);
        drag(&mut h, from, from + egui::vec2(3.0 * pps, 0.0), egui::Modifiers::NONE);
        let s = spans(h.state());
        assert_eq!(s[1].0, 10.0, "start unchanged");
        assert!((s[1].1 - 17.0).abs() < 0.3, "end moved to ~17 s: {s:?}");
        assert!(h.state().timeline.lyric_drag.is_none());
        assert_eq!(h.state().history.undo_label(), Some("Resize Line"));
    }

    #[test]
    fn dragging_a_lyric_block_moves_it_and_alt_drag_copies_it() {
        let mut h = harness(lyric_app());
        let pps = h.state().timeline.px_per_sec;
        let r = h.get_by_label("Lyric B").rect();
        drag(&mut h, r.center(), r.center() + egui::vec2(1.5 * pps, 0.0), egui::Modifiers::NONE);
        let s = spans(h.state());
        assert!((s[1].0 - 11.5).abs() < 0.3 && (s[1].1 - s[1].0 - 4.0).abs() < 1e-6, "moved, same length: {s:?}");
        assert_eq!(h.state().session.lyrics.lines.len(), 3);

        let r = h.get_by_label("Lyric A").rect();
        drag(&mut h, r.center(), r.center() + egui::vec2(30.0 * pps, 0.0), egui::Modifiers::ALT);
        let a = h.state();
        let texts: Vec<&str> = a.session.lyrics.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["A", "B", "C", "A"], "a copy lands after C");
        assert_eq!(a.lyric_selection(), vec![3], "the copy is selected");
        assert_eq!(a.history.undo_label(), Some("Copy Lines"));
    }

    #[test]
    fn copy_and_paste_lines_at_the_playhead() {
        let mut app = lyric_app();
        app.screen = crate::app::Screen::Song { project: 0, song: 0 };
        app.lyric_clip_select(0);
        app.lyric_select_range(1);
        assert_eq!(app.lyric_selection(), vec![0, 1]);
        let mut h = Harness::new_ui_state(|ui, app: &mut StageWeaveApp| app.handle_input(ui.ctx()), app);
        h.run_steps(1);
        h.event(egui::Event::Copy);
        h.run_steps(1);
        assert_eq!(h.state().lyric_clipboard.len(), 2);
        let json = serde_json::json!({ "stageweave_lyrics": h.state().lyric_clipboard }).to_string();
        h.state_mut().seek(40.0);
        h.event(egui::Event::Paste(json));
        h.run_steps(1);
        let s = spans(h.state());
        assert_eq!(s, vec![(0.0, 4.0), (10.0, 14.0), (20.0, 24.0), (40.0, 44.0), (50.0, 54.0)]);
        assert_eq!(h.state().lyric_selection(), vec![3, 4]);
        // Delete removes the whole selection in one undo step.
        h.key_press(egui::Key::Delete);
        h.run_steps(1);
        assert_eq!(h.state().session.lyrics.lines.len(), 3);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(1);
        assert_eq!(h.state().session.lyrics.lines.len(), 5);
    }
}
