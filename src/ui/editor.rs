//! Upper-left panel: lyrics editor and track inspector.

use egui::{Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::MusicKitApp;
use crate::model::LyricTheme;
use crate::theme::*;

use super::lyric_actions::{self, Act};
use super::widgets::{Icon, icon_button, gain_fader, pan_knob, panel_header, primary_button, section_label, toggle_chip};

pub fn draw(app: &mut MusicKitApp, ui: &mut Ui) {
    ui.set_min_size(ui.available_size());
    let mut tab = app.editor_tab;
    panel_header(ui, &["Lyrics", "Inspector"], &mut tab, |_| {});
    app.editor_tab = tab;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::same(10)).show(ui, |ui| match app.editor_tab {
            0 => lyrics(app, ui),
            _ => inspector(app, ui),
        });
    });
}

fn field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([52.0, 20.0], egui::Label::new(RichText::new(label).color(TEXT_DIM)));
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(f32::INFINITY));
    });
}

fn lyrics(app: &mut MusicKitApp, ui: &mut Ui) {
    if app.session.source.is_none() {
        placeholder(ui, "No song loaded", "Import a song, then paste its lyrics here to build a sing-along video.");
        return;
    }

    {
        let l = &mut app.session.lyrics;
        field(ui, "Title", &mut l.title, "Song title");
        field(ui, "Artist", &mut l.artist, "Artist");
    }
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        section_label(ui, "Lyrics");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if super::ai_panel::ai_button(ui, app.ai_panel.open, app.ai_panel.job.is_some()).clicked() {
                app.open_ai_dialog();
            }
        });
    });
    let l = &mut app.session.lyrics;
    ui.add(
        egui::TextEdit::multiline(&mut l.raw_text)
            .hint_text("Paste lyrics here — one line per row.\nBlank lines are skipped.")
            .desired_rows(8)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Body)
            .margin(egui::Margin::same(8)),
    );
    let line_count = l.raw_text.lines().filter(|s| !s.trim().is_empty()).count();
    ui.label(RichText::new(format!("{line_count} lines")).size(10.5).color(TEXT_FAINT));

    section_label(ui, "Style");
    ui.horizontal_wrapped(|ui| {
        for theme in LyricTheme::ALL {
            theme_swatch(ui, &mut l.style.theme, theme);
        }
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("Text size").color(TEXT_DIM));
        ui.add(egui::Slider::new(&mut l.style.scale, 0.6..=1.8).show_value(false));
    });
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut l.style.show_next_line, "Next line");
        ui.checkbox(&mut l.style.show_progress, "Progress bar");
        ui.checkbox(&mut l.style.animated_background, "Animated background");
    });
    background_picker(app, ui);
    ui.add_space(8.0);
    let l = &mut app.session.lyrics;

    let generated = l.is_generated();
    if !app.has_tracks() {
        ui.label(
            RichText::new("Tip: separate stems first so timing follows the vocals.")
                .size(11.0)
                .color(SOLO_ON),
        );
        ui.add_space(4.0);
    }
    if primary_button(ui, if generated { "Regenerate Lyric Video" } else { "Generate Lyric Video" }, line_count > 0).clicked() {
        app.generate_lyrics();
    }
    if generated && ui.add_sized([ui.available_width(), 26.0], egui::Button::new("Export Video (MP4)…")).clicked() {
        app.export.open = true;
    }

    if app.session.lyrics.is_generated() {
        ui.add_space(10.0);
        timing_list(app, ui);
    }
}

/// A 70×40 tile: thumbnail (or label) with a selection outline.
fn bg_tile(ui: &mut Ui, selected: bool, thumb: Option<(egui::TextureId, egui::Vec2)>, label: &str, sub: Option<&str>, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(70.0, 40.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, format!("Background {label}")));
    let p = ui.painter();
    match thumb {
        Some((id, size)) => {
            p.image(id, rect, super::lyrics_view::cover_uv(size, rect), Color32::WHITE);
            p.rect_filled(Rect::from_min_max(pos2(rect.left(), rect.bottom() - 14.0), rect.max), 0.0, Color32::from_black_alpha(150));
            p.text(rect.center_bottom() - vec2(0.0, 7.0), Align2::CENTER_CENTER, label, FontId::proportional(9.5), Color32::WHITE);
        }
        None => {
            p.rect_filled(rect, 5.0, BG_INSET);
            p.text(rect.center() - vec2(0.0, if sub.is_some() { 6.0 } else { 0.0 }), Align2::CENTER_CENTER, label, FontId::proportional(10.5), TEXT_DIM);
            if let Some(sub) = sub {
                p.text(rect.center() + vec2(0.0, 8.0), Align2::CENTER_CENTER, sub, FontId::proportional(9.0), TEXT_FAINT);
            }
        }
    }
    p.rect_stroke(
        rect,
        5.0,
        Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { ACCENT_BRIGHT } else if resp.hovered() { BORDER_STRONG } else { BORDER }),
        egui::StrokeKind::Outside,
    );
    resp.on_hover_text(tip)
}

/// Background: theme gradient, a starter loop, or the user's own image / video.
fn background_picker(app: &mut MusicKitApp, ui: &mut Ui) {
    use crate::backgrounds::{self, STARTERS};
    use crate::model::Background;

    section_label(ui, "Background");
    let ctx = ui.ctx().clone();
    let root = app.ws.root();
    let ffmpeg = app.ffmpeg.path();
    let current = app.session.lyrics.style.background.clone();
    let library = backgrounds::list(&root);
    let mut pick: Option<Background> = None;
    let mut choose_file = false;
    ui.horizontal_wrapped(|ui| {
        if bg_tile(ui, current == Background::Theme, None, "Theme", None, "The theme's colour gradient").clicked() {
            pick = Some(Background::Theme);
        }
        for s in &STARTERS {
            let thumb = app.backdrop.thumb(&ctx, &root, s.file, ffmpeg.as_deref());
            if bg_tile(ui, current.file() == Some(s.file), thumb, s.title, None, &format!("{} (looping video)", s.title)).clicked() {
                match backgrounds::ensure_starter(&root, s) {
                    Ok(()) => pick = Some(backgrounds::background_for(s.file)),
                    Err(e) => app.toast(crate::app::ToastKind::Error, e),
                }
            }
        }
        for f in library.iter().filter(|f| !STARTERS.iter().any(|s| s.file == f.as_str())) {
            let thumb = app.backdrop.thumb(&ctx, &root, f, ffmpeg.as_deref());
            let label: String = std::path::Path::new(f).file_stem().unwrap_or_default().to_string_lossy().chars().take(10).collect();
            if bg_tile(ui, current.file() == Some(f.as_str()), thumb, &label, None, f).clicked() {
                pick = Some(backgrounds::background_for(f));
            }
        }
        if bg_tile(ui, false, None, "+ Add", Some("image / video"), "Use your own image, or a short video that loops through the song").clicked() {
            choose_file = true;
        }
    });
    if let Some(bg) = pick
        && bg != current
    {
        app.set_background(bg);
    }
    if choose_file {
        app.choose_background_file();
    }
    if current != Background::Theme {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Darken").color(TEXT_DIM));
            ui.add(egui::Slider::new(&mut app.session.lyrics.style.background_dim, 0.0..=0.85).show_value(false))
                .on_hover_text("Darken the background so the words stand out");
        });
    }
    if let Some(e) = &app.backdrop.error {
        ui.label(RichText::new(e).size(11.0).color(DANGER));
    }
    if matches!(current, Background::Video { .. }) {
        ffmpeg_status(app, ui);
    }
}

/// One-time ffmpeg download status (needed for video backgrounds and export).
pub fn ffmpeg_status(app: &MusicKitApp, ui: &mut Ui) {
    use crate::engine::ffmpeg::FfmpegStatus;
    match app.ffmpeg.status() {
        FfmpegStatus::Downloading(s) => {
            let pct = s.fraction().map_or(String::new(), |f| format!(" {:.0}%", f * 100.0));
            ui.label(RichText::new(format!("Getting the video engine (ffmpeg, one time){pct}…")).size(11.0).color(TEXT_DIM));
        }
        FfmpegStatus::Failed(e) => {
            ui.label(RichText::new(format!("Video engine (ffmpeg) unavailable: {e}")).size(11.0).color(DANGER));
        }
        _ => {}
    }
}

fn theme_swatch(ui: &mut Ui, current: &mut LyricTheme, theme: LyricTheme) {
    let [top, bottom, hi, _] = theme.palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(70.0, 40.0), Sense::click());
    let p = ui.painter();
    let mut mesh = egui::Mesh::default();
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
    p.with_clip_rect(rect).add(mesh);
    p.text(rect.center() - vec2(0.0, 4.0), Align2::CENTER_CENTER, "Aa", FontId::proportional(13.0), hi);
    p.text(rect.center_bottom() - vec2(0.0, 7.0), Align2::CENTER_CENTER, theme.label(), FontId::proportional(9.5), Color32::from_white_alpha(200));
    let selected = *current == theme;
    p.rect_stroke(
        rect,
        5.0,
        Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { ACCENT_BRIGHT } else if resp.hovered() { BORDER_STRONG } else { BORDER }),
        egui::StrokeKind::Outside,
    );
    if resp.clicked() {
        *current = theme;
    }
}

fn timing_list(app: &mut MusicKitApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        section_label(ui, "Timing");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if app.tap_sync.is_some() {
                if ui.button("Stop Tap Sync").clicked() {
                    app.tap_sync = None;
                }
            } else if ui
                .button("⏺ Tap Sync")
                .on_hover_text("Plays from the start. Press Enter as each line begins.")
                .clicked()
            {
                app.tap_sync = Some(0);
                app.seek(0.0);
                if !app.is_playing() {
                    app.toggle_play();
                }
            }
        });
    });

    ui.label(
        RichText::new("Click a line to select · double-click to edit · right-click to insert or delete")
            .size(10.5)
            .color(TEXT_FAINT),
    );
    ui.add_space(2.0);

    let pos = app.position();
    let current = app.session.lyrics.line_at(pos);
    let selected = app.lyric_edit.sel.filter(|_| app.lyric_edit.clip_selected);
    let editing = app.lyric_edit.editing.clone().filter(|_| app.lyric_edit.edit_in_list);
    let dur = app.session.duration();
    let mut act: Option<Act> = None;
    let mut new_start: Option<(usize, f64)> = None;
    let mut commit = false;
    let mut cancel = false;
    let mut draft_update: Option<String> = None;

    for i in 0..app.session.lyrics.lines.len() {
        let line = app.session.lyrics.lines[i].clone();
        let is_current = current == Some(i);
        let is_sel = selected == Some(i);
        let is_tap_target = app.tap_sync == Some(i);
        let fill = if is_tap_target {
            SOLO_ON.gamma_multiply(0.15)
        } else if is_sel {
            ACCENT.gamma_multiply(0.28)
        } else if is_current {
            ACCENT.gamma_multiply(0.14)
        } else if i % 2 == 0 {
            BG_INSET
        } else {
            Color32::TRANSPARENT
        };
        egui::Frame::NONE
            .fill(fill)
            .stroke(if is_sel { egui::Stroke::new(1.0, ACCENT_BRIGHT) } else { egui::Stroke::NONE })
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 3))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let num = ui.add(egui::Label::new(RichText::new(format!("{:>2}", i + 1)).monospace().color(TEXT_FAINT)).sense(Sense::click()));
                    num.context_menu(|ui| {
                        if let Some(a) = lyric_actions::context_menu(app, ui, i, true) {
                            act = Some(a);
                        }
                    });
                    let mut start = line.start;
                    if ui
                        .add(
                            egui::DragValue::new(&mut start)
                                .range(0.0..=dur.max(line.end))
                                .speed(0.02)
                                .custom_formatter(|v, _| timecode(v))
                                .custom_parser(parse_timecode),
                        )
                        .on_hover_text("Line start · drag to adjust")
                        .changed()
                        && (start - line.start).abs() > 1e-6
                    {
                        new_start = Some((i, start));
                    }

                    // Quick actions on the right (shown on hover / selection).
                    let hovered = ui.rect_contains_pointer(ui.max_rect());
                    let is_editing = editing.as_ref().is_some_and(|(k, _)| *k == i);
                    let actions_w = if (hovered || is_sel) && !is_editing { 132.0 } else { 0.0 };
                    let text_w = (ui.available_width() - actions_w).max(60.0);

                    if let Some((_, draft)) = editing.as_ref().filter(|(k, _)| *k == i) {
                        let mut d = draft.clone();
                        let r = ui.add(egui::TextEdit::singleline(&mut d).hint_text("Type the line…").desired_width(text_w - 14.0));
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        let (enter, esc) = ui.input(|inp| (inp.key_pressed(egui::Key::Enter), inp.key_pressed(egui::Key::Escape)));
                        if esc {
                            cancel = true;
                        } else if r.lost_focus() && (enter || !r.has_focus()) {
                            commit = true;
                        }
                        draft_update = Some(d);
                    } else {
                        let (r, resp) = ui.allocate_exact_size(vec2(text_w, 18.0), Sense::click());
                        let (text, color) = if line.text.trim().is_empty() {
                            ("(empty)", TEXT_FAINT)
                        } else {
                            (line.text.as_str(), if is_current || is_sel { Color32::WHITE } else { TEXT })
                        };
                        ui.painter()
                            .with_clip_rect(r)
                            .text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.5), color);
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
                        if resp.double_clicked() {
                            act = Some(Act::Edit(i, true));
                        } else if resp.clicked() {
                            act = Some(Act::Select(i));
                        }
                        resp.context_menu(|ui| {
                            if let Some(a) = lyric_actions::context_menu(app, ui, i, true) {
                                act = Some(a);
                            }
                        });
                    }

                    if actions_w > 0.0 {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            if icon_button(ui, Icon::Close, 20.0, false, &format!("Delete line {}", i + 1)).clicked() {
                                act = Some(Act::Delete(i));
                            }
                            let at = app.lyric_insert_time().map(|t| format!(" at {}", timecode(t))).unwrap_or_default();
                            if ui.small_button("+ Below").on_hover_text(format!("Insert a line below{at}")).clicked() {
                                act = Some(Act::Below(i));
                            }
                            if ui.small_button("+ Above").on_hover_text(format!("Insert a line above{at}")).clicked() {
                                act = Some(Act::Above(i));
                            }
                        });
                    }
                });
            });
    }

    if let Some(d) = draft_update
        && let Some((k, _)) = app.lyric_edit.editing.clone()
    {
        app.lyric_edit.editing = Some((k, d));
    }
    if cancel {
        app.lyric_cancel_text();
    } else if commit {
        app.lyric_commit_text();
    }
    if let Some((i, t)) = new_start {
        app.lyric_set_start(i, t);
    }
    if let Some(a) = act {
        lyric_actions::apply(app, a, true);
    }
}

fn parse_timecode(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some((m, sec)) = s.split_once(':') {
        Some(m.trim().parse::<f64>().ok()? * 60.0 + sec.trim().parse::<f64>().ok()?)
    } else {
        s.parse().ok()
    }
}

fn inspector(app: &mut MusicKitApp, ui: &mut Ui) {
    let Some(i) = app.selected_track.filter(|&i| i < app.session.tracks.len()) else {
        placeholder(ui, "Nothing selected", "Select a stem in the timeline or project bin to adjust it.");
        return;
    };
    let audible = app.session.is_audible(i);
    let mut changed = false;
    let t = &mut app.session.tracks[i];

    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
        ui.painter().rect_filled(r, 7.0, t.color);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, t.kind.icon(), FontId::proportional(16.0), Color32::BLACK);
        ui.vertical(|ui| {
            ui.add(egui::TextEdit::singleline(&mut t.name).font(egui::TextStyle::Heading).frame(egui::Frame::NONE).desired_width(f32::INFINITY));
            ui.label(RichText::new(format!("{} stem  ·  {}", t.kind.label(), if audible { "audible" } else { "silent" })).size(11.0).color(TEXT_DIM));
        });
    });
    ui.add_space(6.0);

    section_label(ui, "Mix");
    egui::Grid::new("inspector_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(RichText::new("Gain").color(TEXT_DIM));
        ui.horizontal(|ui| {
            changed |= gain_fader(ui, &mut t.gain_db, 170.0, t.color).changed();
            ui.label(RichText::new(db_label(t.gain_db)).monospace().color(TEXT));
        });
        ui.end_row();

        ui.label(RichText::new("Pan").color(TEXT_DIM));
        ui.horizontal(|ui| {
            changed |= pan_knob(ui, &mut t.pan).changed();
            changed |= ui
                .add(egui::Slider::new(&mut t.pan, -1.0..=1.0).show_value(false))
                .changed();
            let pan_txt = if t.pan.abs() < 0.01 {
                "C".to_owned()
            } else if t.pan < 0.0 {
                format!("L{:.0}", -t.pan * 100.0)
            } else {
                format!("R{:.0}", t.pan * 100.0)
            };
            ui.label(RichText::new(pan_txt).monospace());
        });
        ui.end_row();

        ui.label(RichText::new("State").color(TEXT_DIM));
        ui.horizontal(|ui| {
            if toggle_chip(ui, "M", t.muted, MUTE_ON, "Mute").clicked() {
                t.muted = !t.muted;
                changed = true;
            }
            if toggle_chip(ui, "S", t.solo, SOLO_ON, "Solo").clicked() {
                t.solo = !t.solo;
                changed = true;
            }
        });
        ui.end_row();

        ui.label(RichText::new("Colour").color(TEXT_DIM));
        ui.color_edit_button_srgba(&mut t.color);
        ui.end_row();
    });

    section_label(ui, "Source");
    let file = t
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "Rendered in memory (splitter not connected)".to_owned());
    ui.label(RichText::new(file).size(11.0).color(TEXT_FAINT));

    if changed {
        app.sync_mix();
    }
}

fn placeholder(ui: &mut Ui, title: &str, body: &str) {
    ui.add_space(30.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(14.0).color(TEXT));
        ui.add_space(2.0);
        ui.label(RichText::new(body).size(12.0).color(TEXT_FAINT));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LyricLine;
    use egui_kittest::{Harness, kittest::Queryable};

    fn app_with_lines() -> MusicKitApp {
        let mut app = MusicKitApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        app.session.lyrics.lines = ["A", "B", "C"]
            .iter()
            .enumerate()
            .map(|(i, t)| LyricLine { text: (*t).into(), start: i as f64 * 10.0, end: i as f64 * 10.0 + 4.0 })
            .collect();
        app.session.lyrics.raw_text = "A\nB\nC".into();
        app.lyric_clip_select(1); // shows the row's quick actions
        app
    }

    fn harness(app: MusicKitApp) -> Harness<'static, MusicKitApp> {
        let mut h = Harness::builder().with_size(egui::vec2(700.0, 500.0)).build_ui_state(
            |ui, app: &mut MusicKitApp| {
                timing_list(app, ui);
            },
            app,
        );
        h.run_steps(2);
        h
    }

    fn texts(h: &Harness<'_, MusicKitApp>) -> Vec<String> {
        h.state().session.lyrics.lines.iter().map(|l| l.text.clone()).collect()
    }

    #[test]
    fn plus_below_button_inserts_after_the_row() {
        let mut h = harness(app_with_lines());
        h.get_by_label("+ Below").click();
        h.run_steps(2);
        assert_eq!(texts(&h), ["A", "B", "", "C"], "a new empty line after B");
        assert_eq!(h.state().lyric_edit.editing.as_ref().map(|e| e.0), Some(2), "and it opens for typing");
        assert!(h.state().lyric_edit.edit_in_list, "in the Timing list, not on the timeline");
    }

    #[test]
    fn plus_above_button_inserts_before_the_row() {
        let mut h = harness(app_with_lines());
        h.get_by_label("+ Above").click();
        h.run_steps(2);
        assert_eq!(texts(&h), ["A", "", "B", "C"]);
    }

    #[test]
    fn delete_x_removes_the_row_it_is_on_not_the_selected_one() {
        let mut h = harness(app_with_lines()); // B (line 2) is selected
        h.get_by_label("C").hover(); // reveal row 3's buttons
        h.run_steps(2);
        h.get_by_label("Delete line 3").click();
        h.run_steps(2);
        assert_eq!(texts(&h), ["A", "B"], "C went, the selected B stayed");
    }

    #[test]
    fn clicking_a_line_selects_it() {
        let mut h = harness(app_with_lines());
        h.get_by_label("C").click();
        h.run_steps(2);
        assert_eq!(h.state().lyric_edit.sel, Some(2));
        assert!(h.state().lyric_edit.clip_selected);
    }

    #[test]
    fn right_click_menu_deletes_a_line() {
        let mut h = harness(app_with_lines());
        h.get_by_label("A").click_secondary();
        h.run_steps(2);
        h.get_by_label("Delete Line ⌫").click(); // menu item label includes its shortcut hint
        h.run_steps(2);
        assert_eq!(texts(&h), ["B", "C"]);
    }

    #[test]
    fn delete_button_removes_the_row_and_updates_the_text() {
        let mut h = harness(app_with_lines());
        h.get_by_label("Delete line 2").click(); // the selected row (B)
        h.run_steps(2);
        assert_eq!(texts(&h), ["A", "C"]);
        assert_eq!(h.state().session.lyrics.raw_text, "A\nC", "Lyrics panel text follows");
    }

    #[test]
    fn background_picker_uses_library_files_and_theme() {
        let root = std::env::temp_dir().join(format!("mk-bgpick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(crate::backgrounds::dir(&root)).unwrap();
        image::RgbaImage::from_pixel(32, 18, image::Rgba([40, 90, 200, 255])).save(crate::backgrounds::dir(&root).join("Sky.png")).unwrap();
        let app = app_with_lines();
        app.ws.set_root(&root);
        let mut h = Harness::builder().with_size(egui::vec2(420.0, 300.0)).build_ui_state(|ui, app: &mut MusicKitApp| background_picker(app, ui), app);
        h.run_steps(2);
        for name in ["Background Theme", "Background Clouds", "Background Flowers", "Background Drapes", "Background + Add"] {
            h.get_by_label(name);
        }
        h.get_by_label("Background Sky").click();
        h.run_steps(2);
        assert_eq!(h.state().session.lyrics.style.background, crate::model::Background::Image { file: "Sky.png".into() });
        h.get_by_label("Darken"); // dim slider appears for image backgrounds
        h.get_by_label("Background Theme").click();
        h.run_steps(2);
        assert_eq!(h.state().session.lyrics.style.background, crate::model::Background::Theme);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn export_dialog_shows_options() {
        let mut app = app_with_lines();
        app.export.open = true;
        app.screen = crate::app::Screen::Song { project: 1, song: 1 };
        let mut h = Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(|ui, app: &mut MusicKitApp| super::super::export_ui::dialog(app, ui.ctx()), app);
        h.run_steps(2);
        assert!(h.state().export.open);
        h.get_by_label("720p").click();
        h.run_steps(2);
        assert_eq!(h.state().export.height, 720);
        h.get_by_label("Original song");
    }
}
