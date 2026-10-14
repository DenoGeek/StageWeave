use egui::{Align, Color32, Layout, Rect, RichText, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::app::StageWeaveApp;
use crate::theme::*;

use super::widgets::{Icon, gain_fader, icon_button, play_button};

pub fn draw(app: &mut StageWeaveApp, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap().shrink2(vec2(14.0, 0.0));
    ui.allocate_rect(rect, egui::Sense::hover());
    let enabled = app.can_play();

    // Left: timecode.
    let left = Rect::from_min_size(rect.min, vec2(250.0, rect.height()));
    ui.scope_builder(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.label(RichText::new(smpte(app.position())).monospace().size(20.0).color(if enabled { ACCENT_BRIGHT } else { TEXT_FAINT }));
        ui.label(RichText::new(format!("/ {}", smpte(app.session.duration()))).monospace().size(11.5).color(TEXT_FAINT));
    });

    // Setlist navigation (left of centre).
    let ctx = ui.ctx().clone();
    let (prev, next) = (app.neighbour_song(-1), app.neighbour_song(1));
    let nav = Rect::from_min_size(rect.center_top() - vec2(141.0 + 230.0, 0.0), vec2(220.0, rect.height()));
    ui.scope_builder(UiBuilder::new().max_rect(nav).layout(Layout::right_to_left(Align::Center)), |ui| {
        let next_tip = next.as_ref().map_or("Last song in the setlist".to_owned(), |s| format!("Next: {} (PageDown)", s.title));
        if ui.add_enabled(next.is_some(), egui::Button::new("Next ›")).on_hover_text(next_tip).clicked() {
            app.next_song(&ctx);
        }
        let prev_tip = prev.as_ref().map_or("First song in the setlist".to_owned(), |s| format!("Previous: {} (PageUp)", s.title));
        if ui.add_enabled(prev.is_some(), egui::Button::new("‹ Prev")).on_hover_text(prev_tip).clicked() {
            app.prev_song(&ctx);
        }
        if let (Some(cur), Some(_)) = (app.current_song_id(), app.current_project()) {
            let i = app.songs.iter().position(|s| s.id == cur).map_or(0, |i| i + 1);
            ui.label(RichText::new(format!("{i}/{}", app.songs.len())).monospace().size(11.0).color(TEXT_FAINT));
        }
    });

    // Centre: transport buttons.
    let centre = Rect::from_center_size(rect.center(), vec2(282.0, rect.height()));
    ui.scope_builder(UiBuilder::new().max_rect(centre).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icon_button(ui, Icon::ToStart, 28.0, false, "Go to start (Home)").clicked() {
                app.seek(0.0);
                app.timeline.scroll = 0.0;
            }
            if icon_button(ui, Icon::StepBack, 28.0, false, "Back 5 s (←)").clicked() {
                app.seek(app.position() - 5.0);
            }
            ui.add_space(4.0);
            if play_button(ui, app.is_playing(), enabled).clicked() {
                app.toggle_play();
            }
            ui.add_space(4.0);
            if icon_button(ui, Icon::Stop, 28.0, false, "Stop and return to start").clicked() {
                app.stop();
            }
            if icon_button(ui, Icon::StepForward, 28.0, false, "Forward 5 s (→)").clicked() {
                app.seek(app.position() + 5.0);
            }
            if icon_button(ui, Icon::ToEnd, 28.0, false, "Go to end (End)").clicked() {
                app.seek(app.session.duration());
            }
            if icon_button(ui, Icon::Loop, 28.0, app.loop_enabled, "Loop (L)").clicked() {
                app.loop_enabled = !app.loop_enabled;
            }
        });
    });

    // Between the transport and master: cues.
    let cue_area = Rect::from_min_max(pos2(centre.right() + 12.0, rect.top()), pos2(rect.right() - 272.0, rect.bottom()));
    if enabled && cue_area.width() > 60.0 {
        ui.scope_builder(UiBuilder::new().max_rect(cue_area).layout(Layout::left_to_right(Align::Center)), |ui| {
            ui.set_clip_rect(cue_area);
            cues(app, ui);
        });
    }

    // Right: master volume.
    let right = Rect::from_min_max(rect.right_top() - vec2(260.0, 0.0), rect.max);
    ui.scope_builder(UiBuilder::new().max_rect(right).layout(Layout::right_to_left(Align::Center)), |ui| {
        ui.label(RichText::new(db_label(app.master_db)).monospace().size(11.0).color(TEXT_DIM));
        if gain_fader(ui, &mut app.master_db, 120.0, ACCENT_BRIGHT).changed() {
            app.sync_mix();
        }
        ui.label(RichText::new("Master").size(11.5).color(TEXT_DIM));
    });
}

/// Cue controls: what's being marked, the loop that's playing (with a clear
/// way out), or the song's cues to jump into.
fn cues(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.x = 6.0;
    if let Some(start) = app.cue.recording {
        let pulse = 0.5 + 0.5 * (ui.input(|i| i.time) * 4.0).sin().abs() as f32;
        let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 5.0, DANGER.gamma_multiply(pulse));
        ui.label(RichText::new(format!("Cue from {}", timecode(start))).size(12.0).color(Color32::WHITE))
            .on_hover_text("Press Q again where the section ends. It will then loop. (Esc cancels)");
        ui.label(RichText::new("Q to close").size(11.0).color(TEXT_FAINT));
        ui.ctx().request_repaint();
        return;
    }
    if let Some(i) = app.cue.active {
        let Some(c) = app.session.cues.get(i).cloned() else { return };
        let exit = egui::Button::new(RichText::new("Exit Cue").strong().color(Color32::BLACK))
            .fill(CUE)
            .stroke(Stroke::NONE)
            .corner_radius(12)
            .min_size(vec2(78.0, 24.0));
        if ui.add(exit).on_hover_text("Stop looping, keep playing with the song's mix (Esc)").clicked() {
            app.exit_cue();
        }
        // Name it only when it fits whole; the timeline header names it too.
        let text = format!("Looping {}", c.name);
        let fits = |t: &str| ui.painter().layout_no_wrap(t.to_owned(), egui::FontId::proportional(12.0), CUE).size().x <= ui.available_width();
        let label = [text.as_str(), c.name.as_str()].into_iter().find(|t| fits(t));
        if let Some(label) = label {
            ui.label(RichText::new(label).size(12.0).color(CUE)).on_hover_text(format!("Looping {}: {} – {}{}", c.name, timecode(c.start), timecode(c.end), if c.mix.is_some() { " · own mix" } else { "" }));
        }
        return;
    }
    if app.session.cues.is_empty() {
        ui.label(RichText::new("Q: mark a cue").size(11.0).color(TEXT_FAINT))
            .on_hover_text("While playing, press Q where a section starts and Q again where it ends: it loops until you exit.");
        return;
    }
    ui.label(RichText::new("Cues").size(11.0).color(TEXT_FAINT));
    let mut go = None;
    for (i, c) in app.session.cues.iter().enumerate() {
        let b = egui::Button::new(RichText::new(c.name.trim_start_matches("Cue ")).size(11.5).color(Color32::BLACK))
            .fill(CUE.gamma_multiply(0.75))
            .corner_radius(10)
            .min_size(vec2(24.0, 20.0));
        if ui.add(b).on_hover_text(format!("Loop {} ({} – {})", c.name, timecode(c.start), timecode(c.end))).clicked() {
            go = Some(i);
        }
    }
    if let Some(i) = go {
        app.activate_cue(i);
        if !app.is_playing() {
            app.toggle_play();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn exit_cue_button_is_on_the_transport_and_works() {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app.session.cues.push(crate::model::Cue { name: "Cue 1".into(), start: 5.0, end: 9.0, mix: None });
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 48.0)).build_ui_state(|ui, app: &mut StageWeaveApp| draw(app, ui), app);
        h.run_steps(2);
        h.get_by_label("1").click(); // cue chip
        h.run_steps(2);
        assert_eq!(h.state().cue.active, Some(0));
        h.get_by_label("Exit Cue").click();
        h.run_steps(2);
        assert!(h.state().cue.active.is_none());
    }
}
