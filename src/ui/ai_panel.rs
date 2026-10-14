//! "AI" dialog: transcribe the open song's lyrics with Whisper.

use egui::{Align, Align2, Color32, FontId, Layout, Response, RichText, Sense, Stroke, Ui, vec2};

use crate::ai::{LANGUAGES, Provider};
use crate::app::{AiMethod, AiMode, AiResult, StageWeaveApp};
use crate::theme::*;

use super::project_panel::{mmss, progress_bar};
use super::widgets::{Icon, icon_button, paint_icon, primary_button, section_label};

const VIOLET: Color32 = Color32::from_rgb(150, 110, 255);
const VIOLET_LIGHT: Color32 = Color32::from_rgb(200, 180, 255);
const WIDTH: f32 = 620.0;

/// Small pill button with a sparkle; pulses while a job runs.
pub fn ai_button(ui: &mut Ui, open: bool, busy: bool) -> Response {
    let galley = ui.painter().layout_no_wrap("AI".into(), FontId::proportional(11.5), Color32::WHITE);
    let size = vec2(galley.size().x + 34.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = if open {
        VIOLET.gamma_multiply(0.35)
    } else if resp.hovered() {
        VIOLET.gamma_multiply(0.22)
    } else {
        VIOLET.gamma_multiply(0.12)
    };
    p.rect_filled(rect, 11.0, fill);
    let mut stroke = VIOLET.gamma_multiply(if open { 1.0 } else { 0.6 });
    if busy {
        let t = ui.input(|i| i.time) as f32;
        stroke = VIOLET.gamma_multiply(0.5 + 0.5 * (t * 4.0).sin().abs());
        ui.ctx().request_repaint();
    }
    p.rect_stroke(rect, 11.0, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let icon = egui::Rect::from_center_size(rect.left_center() + vec2(13.0, 0.0), vec2(13.0, 13.0));
    paint_icon(p, icon, Icon::Sparkle, VIOLET_LIGHT);
    p.galley(rect.left_center() + vec2(23.0, -galley.size().y / 2.0), galley, Color32::WHITE);
    let tip = if busy { "Transcribing… click to view" } else { "Transcribe this song's lyrics with AI" };
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The modal dialog. Drawn every frame while `app.ai_panel.open`.
pub fn dialog(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if !app.ai_panel.open {
        return;
    }
    if app.current_song_id().is_none() {
        app.ai_panel.open = false;
        return;
    }
    // Dim everything behind the dialog; clicking the backdrop closes it.
    let screen = ctx.content_rect();
    let backdrop = egui::Area::new(egui::Id::new("ai_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(150));
            ui.allocate_rect(screen, Sense::click())
        });
    if backdrop.inner.clicked() {
        app.ai_panel.open = false;
    }

    egui::Window::new("ai_dialog")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .frame(
            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0, VIOLET.gamma_multiply(0.5)))
                .corner_radius(12)
                .inner_margin(0)
                .shadow(egui::epaint::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(160) }),
        )
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            header(app, ui);
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 24, top: 18, bottom: 22 }).show(ui, |ui| {
                ui.set_width(WIDTH - 48.0);
                body(app, ui);
            });
        });
}

fn header(app: &mut StageWeaveApp, ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(WIDTH, 78.0), Sense::hover());
    let p = ui.painter();
    // Soft violet wash.
    let mut mesh = egui::Mesh::default();
    let (a, b) = (VIOLET.gamma_multiply(0.28), BG_PANEL);
    mesh.colored_vertex(rect.left_top(), a);
    mesh.colored_vertex(rect.right_top(), b);
    mesh.colored_vertex(rect.right_bottom(), b);
    mesh.colored_vertex(rect.left_bottom(), b);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.with_clip_rect(rect.shrink(1.0)).add(mesh);
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, BORDER));

    let badge = egui::Rect::from_center_size(rect.left_center() + vec2(46.0, 0.0), vec2(40.0, 40.0));
    p.rect_filled(badge, 10.0, VIOLET.gamma_multiply(0.3));
    paint_icon(p, badge.shrink(9.0), Icon::Sparkle, VIOLET_LIGHT);
    let aligning = match &app.ai_panel.result {
        Some((_, AiResult::Alignment(_))) => true,
        Some((_, AiResult::Transcript(_))) => false,
        None => app.ai_panel.mode == AiMode::Align,
    };
    let title = if aligning { "Align lyrics" } else { "Transcribe lyrics" };
    p.text(rect.left_center() + vec2(80.0, -10.0), Align2::LEFT_CENTER, title, FontId::proportional(18.0), Color32::WHITE);
    let song = app.current_song().map(|s| format!("{}  ·  {}", s.title, mmss(s.duration))).unwrap_or_default();
    p.text(rect.left_center() + vec2(80.0, 13.0), Align2::LEFT_CENTER, song, FontId::proportional(12.0), TEXT_DIM);

    let close = egui::Rect::from_center_size(rect.right_top() + vec2(-26.0, 26.0), vec2(26.0, 26.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(close));
    if icon_button(&mut child, Icon::Close, 26.0, false, "Close (Esc)").clicked() {
        app.ai_panel.open = false;
    }
}

fn body(app: &mut StageWeaveApp, ui: &mut Ui) {
    // A running job or finished result wins over setup, even if a key was removed meanwhile.
    if app.ai_panel.job.is_some() {
        running(app, ui);
        return;
    }
    let current = app.current_song_id();
    if let Some((song, r)) = &app.ai_panel.result
        && Some(*song) == current
    {
        match r {
            AiResult::Transcript(t) => {
                let t = t.clone();
                transcript_view(app, ui, t);
            }
            AiResult::Alignment(a) => {
                let a = a.clone();
                alignment_view(app, ui, a);
            }
        }
        return;
    }

    mode_switch(app, ui);
    ui.add_space(14.0);
    match app.ai_panel.mode {
        AiMode::Transcribe => transcribe_form(app, ui),
        AiMode::Align => align_form(app, ui),
    }
}

fn mode_switch(app: &mut StageWeaveApp, ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, BG_INSET);
    let modes = [
        (AiMode::Transcribe, "Transcribe audio", "AI writes out the lyrics"),
        (AiMode::Align, "Align my lyrics", "You have the words; AI finds the timing"),
    ];
    let w = rect.width() / 2.0;
    for (i, (mode, title, sub)) in modes.into_iter().enumerate() {
        let r = egui::Rect::from_min_size(rect.min + vec2(i as f32 * w, 0.0), vec2(w, rect.height())).shrink(4.0);
        let resp = ui.interact(r, ui.id().with(("ai_mode", i)), Sense::click());
        let on = app.ai_panel.mode == mode;
        let p = ui.painter();
        if on {
            p.rect_filled(r, 8.0, VIOLET.gamma_multiply(0.28));
            p.rect_stroke(r, 8.0, Stroke::new(1.0, VIOLET.gamma_multiply(0.8)), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect_filled(r, 8.0, BG_WIDGET);
        }
        p.text(r.center() - vec2(0.0, 8.0), Align2::CENTER_CENTER, title, FontId::proportional(13.5), if on { Color32::WHITE } else { TEXT });
        p.text(r.center() + vec2(0.0, 10.0), Align2::CENTER_CENTER, sub, FontId::proportional(10.5), TEXT_FAINT);
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.ai_panel.mode = mode;
            app.ai_panel.error = None;
        }
    }
}

fn no_key_card(app: &mut StageWeaveApp, ui: &mut Ui, why: &str) {
    egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(16).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Connect a transcription service").size(14.0).color(Color32::WHITE));
        ui.add_space(4.0);
        ui.label(RichText::new(why).color(TEXT_DIM));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            for p in Provider::ALL {
                ui.hyperlink_to(format!("{} API keys ↗", p.label()), p.keys_url());
                ui.add_space(12.0);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Open AI Settings").clicked() {
                    app.ai_panel.open = false;
                    app.show_settings = true;
                }
            });
        });
    });
}

fn error_box(app: &StageWeaveApp, ui: &mut Ui) {
    if let Some(e) = app.ai_panel.error.clone() {
        ui.add_space(10.0);
        egui::Frame::NONE.fill(DANGER.gamma_multiply(0.12)).corner_radius(6).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(e).color(Color32::from_rgb(255, 170, 170))).wrap());
        });
    }
}

fn transcribe_form(app: &mut StageWeaveApp, ui: &mut Ui) {
    let configured = app.ai.configured();
    if configured.is_empty() {
        no_key_card(
            app,
            ui,
            "Whisper (through OpenAI or Groq) writes out the song's lyrics with timing. Add an API key for either service in Settings.",
        );
        return;
    }
    options(app, ui, &configured, false);
    error_box(app, ui);
    ui.add_space(16.0);
    let ready = app.can_play() && !app.is_loading();
    if primary_button(ui, "Transcribe Song", ready).clicked() {
        let ctx = ui.ctx().clone();
        app.start_transcription(&ctx);
    }
    footnote(app, ui, true);
}

fn align_form(app: &mut StageWeaveApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Your lyrics").color(TEXT_DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let n = app.ai_panel.align_text.lines().filter(|l| !l.trim().is_empty()).count();
            ui.label(RichText::new(format!("{n} lines")).size(11.0).color(TEXT_FAINT));
            if !app.session.lyrics.raw_text.trim().is_empty()
                && app.ai_panel.align_text != app.session.lyrics.raw_text
                && ui.small_button("Use Lyrics panel text").clicked()
            {
                app.ai_panel.align_text = app.session.lyrics.raw_text.clone();
            }
        });
    });
    egui::ScrollArea::vertical().max_height(190.0).id_salt("align_text_scroll").show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.ai_panel.align_text)
                .hint_text("Paste the lyrics: one line per row, a blank line between verses.\nYour words are kept exactly; only the timing is worked out.")
                .desired_rows(8)
                .desired_width(f32::INFINITY)
                .margin(egui::Margin::same(10)),
        );
    });
    ui.add_space(12.0);

    // Timing source.
    let configured = app.ai.configured();
    let has_vocals = app.vocals_audio().is_some();
    if configured.is_empty() {
        app.ai_panel.align_with_whisper = false;
    }
    egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(14).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Find the timing with").color(TEXT_DIM));
        ui.add_space(4.0);
        ui.add_enabled_ui(!configured.is_empty(), |ui| {
            ui.radio_value(&mut app.ai_panel.align_with_whisper, true, "Whisper: listens for your words (most accurate)")
                .on_disabled_hover_text("Add an OpenAI or Groq key in Settings → AI");
        });
        ui.add_enabled_ui(has_vocals, |ui| {
            ui.radio_value(&mut app.ai_panel.align_with_whisper, false, "On-device: follows the singer's phrases (free, nothing uploaded)")
                .on_disabled_hover_text("Split the song first; this listens to the vocals stem");
        });
        if app.ai_panel.align_with_whisper && !configured.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            options(app, ui, &configured, true);
        }
    });
    if configured.is_empty() && !has_vocals {
        ui.add_space(8.0);
        ui.label(RichText::new("Split the song for on-device timing, or add an API key in Settings → AI to use Whisper.").size(11.0).color(SOLO_ON));
    }
    error_box(app, ui);
    ui.add_space(16.0);
    let ready = app.can_play() && !app.is_loading() && !app.ai_panel.align_text.trim().is_empty();
    if primary_button(ui, "Align Lyrics", ready).clicked() {
        let ctx = ui.ctx().clone();
        app.start_alignment(&ctx);
    }
    footnote(app, ui, app.ai_panel.align_with_whisper);
}

fn footnote(app: &StageWeaveApp, ui: &mut Ui, uploads: bool) {
    ui.add_space(8.0);
    let mut note = if uploads {
        let provider = app.ai.provider().map_or("the provider", Provider::label);
        format!("The audio is uploaded to {provider}.")
    } else {
        "Nothing leaves this computer.".to_owned()
    };
    if !app.session.lyrics.raw_text.trim().is_empty() {
        note.push_str(" You'll review the result before it replaces the current lyrics.");
    }
    ui.label(RichText::new(note).size(11.0).color(TEXT_FAINT));
}

fn options(app: &mut StageWeaveApp, ui: &mut Ui, configured: &[Provider], align: bool) {
    let Some(provider) = app.ai.provider() else { return };
    let frame = if align {
        egui::Frame::NONE
    } else {
        egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(16)
    };
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("ai_opts").num_columns(2).spacing([18.0, 14.0]).min_col_width(80.0).show(ui, |ui| {
            ui.label(RichText::new("Method").color(TEXT_DIM));
            let has_vocals = app.vocals_audio().is_some();
            ui.vertical(|ui| {
                ui.add_enabled_ui(has_vocals, |ui| {
                    ui.radio_value(&mut app.ai_panel.method, AiMethod::Phrases, "Phrase by phrase (vocals stem): best for non-English songs")
                        .on_disabled_hover_text("Split the song first; pieces are cut from the vocals stem");
                });
                ui.radio_value(&mut app.ai_panel.method, AiMethod::WholeSong, "Whole song in one request");
                if let Some(pieces) = app.ai_pieces() {
                    let secs = crate::chunking::sung_seconds(&pieces);
                    ui.label(
                        RichText::new(format!(
                            "≈ {} pieces · {}:{:02} of singing · 3 at a time · silence is skipped",
                            pieces.len(),
                            secs as u32 / 60,
                            secs as u32 % 60
                        ))
                        .size(11.0)
                        .color(TEXT_FAINT),
                    );
                }
            });
            ui.end_row();

            ui.label(RichText::new("Service").color(TEXT_DIM));
            ui.horizontal(|ui| {
                for p in Provider::ALL {
                    let has = configured.contains(&p);
                    let r = ui.add_enabled(has, egui::Button::selectable(provider == p, RichText::new(p.label()).size(13.0)).min_size(vec2(90.0, 28.0)));
                    let r = if has { r } else { r.on_disabled_hover_text("Add a key in Settings → AI") };
                    if r.clicked() {
                        app.ai.set_provider(p);
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new("Model").color(TEXT_DIM));
            let mut current = app.ai.model(provider);
            if align && !current.timestamps {
                current = provider.models().iter().find(|m| m.timestamps).unwrap_or(current);
            }
            egui::ComboBox::from_id_salt("ai_model").selected_text(current.label).width(320.0).show_ui(ui, |ui| {
                for m in provider.models().iter().filter(|m| !align || m.timestamps) {
                    if ui.selectable_label(m.id == current.id, m.label).clicked() {
                        app.ai.set_model(provider, m.id);
                    }
                }
            });
            ui.end_row();

            ui.label(RichText::new("Language").color(TEXT_DIM));
            let lang = app.ai.language();
            let lang_label = LANGUAGES.iter().find(|(c, _)| *c == lang).map_or(lang.as_str(), |(_, n)| n).to_owned();
            egui::ComboBox::from_id_salt("ai_lang").selected_text(lang_label).width(320.0).height(320.0).show_ui(ui, |ui| {
                for (code, name) in LANGUAGES {
                    if ui.selectable_label(*code == lang, *name).clicked() {
                        app.ai.set_language(code);
                    }
                }
            });
            ui.end_row();

            if !has_vocals {
                app.ai_panel.use_vocals = false;
            }
            if app.ai_pieces().is_none() {
                // Whole-song only: choose what to send.
                ui.label(RichText::new("Listen to").color(TEXT_DIM));
                ui.vertical(|ui| {
                    ui.add_enabled_ui(has_vocals, |ui| {
                        ui.radio_value(&mut app.ai_panel.use_vocals, true, "Vocals stem (recommended)")
                            .on_disabled_hover_text("Split the song first to send isolated vocals");
                    });
                    ui.radio_value(&mut app.ai_panel.use_vocals, false, "Full mix");
                });
                ui.end_row();
            }
        });
        if app.ai_pieces().is_some() && app.ai.language().is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("For non-English songs, choose the language. It's the biggest accuracy boost. (Auto-detect checks the longest piece, then uses that language for all.)")
                    .size(11.0)
                    .color(SOLO_ON),
            );
        }

        let model = app.ai.model(provider);
        let hint = if !model.timestamps && !align {
            Some(("This model returns text without timing; lines will be spaced evenly across the song.", SOLO_ON))
        } else if app.vocals_audio().is_none() {
            Some(("Tip: split the song first. Isolated vocals transcribe much more accurately than the full mix.", TEXT_FAINT))
        } else {
            None
        };
        if let Some((text, color)) = hint {
            ui.add_space(10.0);
            ui.label(RichText::new(text).size(11.0).color(color));
        }
    });
}

fn running(app: &mut StageWeaveApp, ui: &mut Ui) {
    let Some(job) = &app.ai_panel.job else { return };
    let (stage, secs) = (job.stage.clone(), job.started.elapsed().as_secs());
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.add(egui::Spinner::new().size(34.0).color(VIOLET_LIGHT));
        ui.add_space(14.0);
        ui.label(RichText::new("Transcribing…").size(16.0).color(Color32::WHITE));
        ui.add_space(4.0);
        ui.label(RichText::new(stage).color(TEXT_DIM));
        ui.label(RichText::new(format!("{}:{:02}", secs / 60, secs % 60)).monospace().color(TEXT_FAINT));
    });
    ui.add_space(14.0);
    progress_bar(ui, -1.0);
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        if ui.add(egui::Button::new("Cancel").min_size(vec2(90.0, 28.0))).clicked() {
            app.cancel_transcription();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new("You can close this window; transcription keeps going.").size(11.0).color(TEXT_FAINT));
        });
    });
}

fn transcript_view(app: &mut StageWeaveApp, ui: &mut Ui, t: crate::ai::Transcript) {
    let timed = !t.lines.is_empty();
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        super::widgets::paint_check(ui.painter(), r, SUCCESS);
        let summary = if timed {
            format!("{} lines, timed to the music", t.lines.len())
        } else {
            format!("{} words (no timing)", t.text.split_whitespace().count())
        };
        ui.label(RichText::new(summary).size(14.0).color(Color32::WHITE));
        if let Some(l) = &t.language {
            ui.label(RichText::new(format!("· {l}")).color(TEXT_FAINT));
        }
    });
    if let Some(note) = &t.note {
        ui.label(RichText::new(note).size(11.0).color(TEXT_FAINT));
    }
    ui.add_space(10.0);
    egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(4).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if timed {
                for (i, l) in t.lines.iter().enumerate() {
                    egui::Frame::NONE
                        .fill(if i % 2 == 0 { Color32::TRANSPARENT } else { BG_PANEL })
                        .inner_margin(egui::Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.add_sized([54.0, 18.0], egui::Label::new(RichText::new(timecode(l.start)).monospace().size(11.0).color(VIOLET_LIGHT)));
                                ui.add(egui::Label::new(RichText::new(&l.text).size(13.5).color(TEXT)).wrap());
                            });
                        });
                }
            } else {
                egui::Frame::NONE.inner_margin(10).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(&t.text).size(13.5).color(TEXT)).wrap());
                });
            }
        });
    });
    ui.add_space(14.0);
    if primary_button(ui, if timed { "Use Lyrics & Timing" } else { "Use Lyrics" }, true).clicked() {
        app.apply_ai_result(true);
        app.ai_panel.open = false;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if timed
            && ui
                .add(egui::Button::new("Text only").min_size(vec2(100.0, 28.0)))
                .on_hover_text("Keep the words, space the lines evenly")
                .clicked()
        {
            app.apply_ai_result(false);
            app.ai_panel.open = false;
        }
        if ui.add(egui::Button::new("Try Again").min_size(vec2(100.0, 28.0))).on_hover_text("Discard and change options").clicked() {
            app.ai_panel.result = None;
        }
        if ui.add(egui::Button::new("Discard").min_size(vec2(100.0, 28.0))).clicked() {
            app.ai_panel.result = None;
            app.ai_panel.open = false;
        }
    });
    ui.add_space(8.0);
    section_label(ui, "Before the service");
    ui.label(RichText::new("Whisper can mishear words, especially names and non-English lines. Read through the lyrics once.").size(11.0).color(TEXT_FAINT));
}

fn alignment_view(app: &mut StageWeaveApp, ui: &mut Ui, a: crate::align::Alignment) {
    use crate::align::{AlignSource, MATCHED};
    let estimated = a.total() - a.matched;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        super::widgets::paint_check(ui.painter(), r, SUCCESS);
        let summary = match a.source {
            AlignSource::Whisper => format!("{} of {} lines matched to the singing", a.matched, a.total()),
            AlignSource::OnDevice => format!("{} lines placed on the singer's phrases", a.total()),
        };
        ui.label(RichText::new(summary).size(14.0).color(Color32::WHITE));
        if a.source == AlignSource::Whisper && estimated > 0 {
            ui.label(RichText::new(format!("· {estimated} estimated")).color(SOLO_ON));
        }
    });
    if let Some(note) = &a.note {
        ui.label(RichText::new(note).size(11.0).color(TEXT_FAINT));
    }
    ui.add_space(10.0);
    egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(8).inner_margin(4).show(ui, |ui| {
        egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (i, l) in a.lines.iter().enumerate() {
                egui::Frame::NONE
                    .fill(if i % 2 == 0 { Color32::TRANSPARENT } else { BG_PANEL })
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (dot, tip) = if a.source == AlignSource::OnDevice {
                                (TEXT_FAINT, "Placed by vocal phrases")
                            } else if l.confidence >= MATCHED {
                                (SUCCESS, "Matched to the words Whisper heard")
                            } else {
                                (SOLO_ON, "Estimated: check this line in Edit mode")
                            };
                            let (r, resp) = ui.allocate_exact_size(vec2(10.0, 18.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.0, dot);
                            resp.on_hover_text(tip);
                            ui.add_sized(
                                [54.0, 18.0],
                                egui::Label::new(RichText::new(timecode(l.line.start)).monospace().size(11.0).color(VIOLET_LIGHT)),
                            );
                            ui.add(egui::Label::new(RichText::new(&l.line.text).size(13.5).color(TEXT)).wrap());
                        });
                    });
            }
        });
    });
    ui.add_space(14.0);
    if primary_button(ui, "Use Timing", true).clicked() {
        app.apply_ai_result(true);
        app.ai_panel.open = false;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new("Try Again").min_size(vec2(100.0, 28.0)))
            .on_hover_text("Edit the lyrics or options and run again")
            .clicked()
        {
            app.ai_panel.result = None;
        }
        if ui.add(egui::Button::new("Discard").min_size(vec2(100.0, 28.0))).clicked() {
            app.ai_panel.result = None;
            app.ai_panel.open = false;
        }
    });
    ui.add_space(8.0);
    let tip = if a.source == AlignSource::OnDevice {
        "On-device timing follows the singer's breaths. Fine-tune any line in Edit mode on the monitor (I / O)."
    } else {
        "Amber lines were estimated from the singing around them. Check them in Edit mode on the monitor (I / O)."
    };
    ui.label(RichText::new(tip).size(11.0).color(TEXT_FAINT));
}
