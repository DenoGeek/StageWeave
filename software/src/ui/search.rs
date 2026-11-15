//! Finding songs anywhere: the ⌘K search window, the "Add from Library"
//! picker, and the result rows they (and the home screen) share.

use egui::{Align, Align2, Color32, FontId, Id, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::app::{LibraryPicker, StageWeaveApp};
use crate::song_library::{self, SongHit};
use crate::theme::*;

use super::widgets::{Icon, paint_icon};

const ROW_H: f32 = 50.0;

/// Search field with a magnifier. Returns its response.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> egui::Response {
    let r = egui::Frame::NONE.fill(BG_INSET).stroke(Stroke::new(1.0, BORDER)).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (icon, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            paint_icon(ui.painter(), icon, Icon::Search, TEXT_FAINT);
            let r = ui.add(egui::TextEdit::singleline(text).hint_text(hint).frame(egui::Frame::NONE).desired_width(width - 46.0));
            if !text.is_empty() && ui.add(egui::Button::new(RichText::new("×").color(TEXT_DIM)).frame(false)).on_hover_text("Clear").clicked() {
                text.clear();
            }
            r
        })
        .inner
    });
    r.inner
}

/// Small pill painted right-to-left from `right`; returns its left edge.
fn chip(p: &egui::Painter, right: f32, y: f32, text: &str, color: Color32) -> f32 {
    let g = p.layout_no_wrap(text.to_owned(), FontId::proportional(10.0), color);
    let rect = Rect::from_min_size(pos2(right - g.size().x - 12.0, y - 9.0), g.size() + vec2(12.0, 5.0));
    p.rect_filled(rect, 8.0, color.gamma_multiply(0.15));
    p.galley(rect.min + vec2(6.0, 2.5), g, color);
    rect.left() - 5.0
}

/// One song result: title, artist, which setlist, the matching lyric, and
/// what's already done for it.
pub fn song_row(ui: &mut Ui, hit: &SongHit, stems: Option<usize>, highlighted: bool, checked: Option<bool>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let s = &hit.song;
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Song {}", s.title)));
    let p = ui.painter();
    let fill = if checked == Some(true) {
        ACCENT.gamma_multiply(0.16)
    } else if highlighted || resp.hovered() {
        BG_WIDGET
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 6.0, fill);

    let mut left = rect.left() + 10.0;
    if let Some(on) = checked {
        let b = Rect::from_center_size(pos2(left + 7.0, rect.center().y), vec2(15.0, 15.0));
        p.rect_filled(b, 3.0, if on { ACCENT } else { BG_INSET });
        p.rect_stroke(b, 3.0, Stroke::new(1.0, if on { ACCENT_BRIGHT } else { BORDER_STRONG }), egui::StrokeKind::Inside);
        if on {
            p.line_segment([b.center() + vec2(-4.0, 0.0), b.center() + vec2(-1.0, 3.0)], Stroke::new(2.0, Color32::WHITE));
            p.line_segment([b.center() + vec2(-1.0, 3.0), b.center() + vec2(4.5, -3.5)], Stroke::new(2.0, Color32::WHITE));
        }
        left += 24.0;
    }

    // Chips (right side) first, so text can be clipped before them.
    let y = rect.top() + 16.0;
    let mut right = rect.right() - 10.0;
    if let Some(n) = stems {
        right = chip(p, right, y, &format!("{n} stems"), SUCCESS);
    }
    if hit.ready.timed > 0 {
        right = chip(p, right, y, "Lyrics timed", ACCENT_BRIGHT);
    } else if hit.ready.lyrics {
        right = chip(p, right, y, "Lyrics", TEXT_DIM);
    }
    if hit.ready.cues > 0 {
        right = chip(p, right, y, &format!("{} cue{}", hit.ready.cues, if hit.ready.cues == 1 { "" } else { "s" }), CUE);
    }
    let meta = [s.song_key.clone(), s.bpm.map(|b| format!("{b:.0} BPM")).unwrap_or_default(), super::project_panel::mmss(s.duration)]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    p.text(pos2(rect.right() - 10.0, rect.top() + 35.0), Align2::RIGHT_CENTER, &meta, FontId::proportional(10.5), TEXT_FAINT);

    let top = p.with_clip_rect(Rect::from_min_max(pos2(left, rect.top()), pos2(right - 8.0, rect.top() + 26.0)));
    let g = top.layout_no_wrap(s.title.clone(), FontId::proportional(13.5), Color32::WHITE);
    let tw = g.size().x;
    top.galley(pos2(left, rect.top() + 16.0 - g.size().y / 2.0), g, Color32::WHITE);
    if !s.artist.is_empty() {
        top.text(pos2(left + tw + 8.0, rect.top() + 16.0), Align2::LEFT_CENTER, &s.artist, FontId::proportional(11.5), TEXT_DIM);
    }
    let bottom = p.with_clip_rect(Rect::from_min_max(pos2(left, rect.top() + 24.0), pos2(rect.right() - 130.0, rect.bottom())));
    let mut where_ = format!("in {}", hit.project);
    if hit.copies > 0 {
        where_ += &format!(" +{} more", hit.copies);
    }
    let g = bottom.layout_no_wrap(where_, FontId::proportional(10.5), ACCENT_BRIGHT.gamma_multiply(0.8));
    let ww = g.size().x;
    bottom.galley(pos2(left, rect.top() + 35.0 - g.size().y / 2.0), g, Color32::WHITE);
    if let Some(line) = &hit.snippet {
        bottom.text(pos2(left + ww + 10.0, rect.top() + 35.0), Align2::LEFT_CENTER, format!("“{line}”"), FontId::proportional(10.5), TEXT_DIM);
    }
    resp
}

fn project_row(ui: &mut Ui, p: &crate::store::ProjectRow, highlighted: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Project {}", p.name)));
    let painter = ui.painter();
    if highlighted || resp.hovered() {
        painter.rect_filled(rect, 6.0, BG_WIDGET);
    }
    for (i, h) in [8.0, 14.0, 10.0, 16.0].iter().enumerate() {
        let x = rect.left() + 14.0 + i as f32 * 5.0;
        painter.line_segment([pos2(x, rect.center().y - h / 2.0), pos2(x, rect.center().y + h / 2.0)], Stroke::new(2.5, ACCENT_BRIGHT.gamma_multiply(0.8)));
    }
    painter.text(pos2(rect.left() + 44.0, rect.center().y), Align2::LEFT_CENTER, &p.name, FontId::proportional(13.5), Color32::WHITE);
    painter.text(
        pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{} song{} · {} min", p.song_count, if p.song_count == 1 { "" } else { "s" }, (p.total_duration / 60.0).round()),
        FontId::proportional(10.5),
        TEXT_FAINT,
    );
    resp
}

fn section(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(10.5).strong().color(TEXT_FAINT));
    ui.add_space(2.0);
}

/// Dim the app behind a floating window.
fn dim(ctx: &egui::Context, id: &str) {
    ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, Id::new(id))).rect_filled(ctx.content_rect(), 0.0, Color32::from_black_alpha(140));
}

enum Pick {
    Project(i64),
    Song(i64, i64),
}

/// ⌘K: search every project, song and lyric.
pub fn palette(app: &mut StageWeaveApp, ctx: &egui::Context) {
    if !app.search.open {
        return;
    }
    let songs = app.song_index();
    let projects = app.projects.clone();
    let query = app.search.query.clone();
    let found_projects: Vec<_> = if query.trim().is_empty() {
        projects.iter().take(6).cloned().collect()
    } else {
        projects.iter().filter(|p| song_library::matches_all(&song_library::terms(&query), &p.name)).take(6).cloned().collect()
    };
    let hits: Vec<SongHit> = if query.trim().is_empty() { Vec::new() } else { song_library::search(&songs, &projects, &query, false).into_iter().take(40).collect() };
    let total = found_projects.len() + hits.len();

    let (up, down, enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
        )
    });
    let s = &mut app.search;
    if down {
        s.cursor = (s.cursor + 1).min(total.saturating_sub(1));
    }
    if up {
        s.cursor = s.cursor.saturating_sub(1);
    }
    s.cursor = s.cursor.min(total.saturating_sub(1));
    let cursor = s.cursor;
    let mut pick: Option<Pick> = None;
    if enter && total > 0 {
        pick = Some(match found_projects.get(cursor) {
            Some(p) => Pick::Project(p.id),
            None => {
                let h = &hits[cursor - found_projects.len()];
                Pick::Song(h.song.project_id, h.song.id)
            }
        });
    }

    dim(ctx, "search_dim");
    let mut open = !esc;
    let width = (ctx.content_rect().width() - 80.0).clamp(360.0, 680.0);
    let shown = egui::Window::new("Search")
        .id(Id::new("global_search"))
        .title_bar(false)
        .resizable(false)
        .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0))
        .default_width(width)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(width);
            let r = search_field(ui, &mut app.search.query, "Search projects, songs, artists, lyrics…", width);
            r.request_focus();
            if r.changed() {
                app.search.cursor = 0;
            }
            ui.add_space(4.0);
            let list_h = (ctx.content_rect().height() - 220.0).max(200.0);
            egui::ScrollArea::vertical().max_height(list_h).min_scrolled_height(list_h.min(420.0)).auto_shrink([false, true]).show(ui, |ui| {
                if !found_projects.is_empty() {
                    section(ui, if query.trim().is_empty() { "RECENT PROJECTS" } else { "PROJECTS" });
                    for (i, p) in found_projects.iter().enumerate() {
                        let r = project_row(ui, p, i == cursor);
                        if i == cursor && (up || down) {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            pick = Some(Pick::Project(p.id));
                        }
                    }
                }
                if !hits.is_empty() {
                    section(ui, "SONGS");
                    for (j, h) in hits.iter().enumerate() {
                        let i = found_projects.len() + j;
                        let stems = app.song_split_stems(&h.song);
                        let r = song_row(ui, h, stems, i == cursor, None).on_hover_text("Open this song");
                        if i == cursor && (up || down) {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            pick = Some(Pick::Song(h.song.project_id, h.song.id));
                        }
                    }
                }
                if total == 0 {
                    ui.add_space(14.0);
                    ui.vertical_centered(|ui| ui.label(RichText::new(format!("Nothing matches “{}”", query.trim())).color(TEXT_DIM)));
                    ui.add_space(14.0);
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Up / Down to move · Enter to open · Esc to close").size(10.5).color(TEXT_FAINT));
        });
    // Clicking outside closes it.
    let inside = |p: egui::Pos2| shown.as_ref().is_some_and(|r| r.response.rect.contains(p));
    if ctx.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !inside(p))) {
        open = false;
    }
    match pick {
        Some(Pick::Project(id)) => {
            app.open_project(id);
            open = false;
        }
        Some(Pick::Song(project, song)) => {
            app.open_song_anywhere(project, song, ctx);
            open = false;
        }
        None => {}
    }
    if !open {
        app.search = Default::default();
    }
}

/// "Add from Library": reuse songs from other setlists, already split and
/// with their lyrics.
pub fn library_picker(app: &mut StageWeaveApp, ctx: &egui::Context) {
    let Some(mut picker) = app.library.take() else { return };
    let Some(project) = app.project_id() else { return };
    let songs = app.song_index();
    let projects = app.projects.clone();
    let others: Vec<_> = songs.iter().filter(|s| s.project_id != project && s.imported).cloned().collect();
    let here: Vec<String> = app.songs.iter().map(|s| s.original_path.to_lowercase()).collect();
    let hits = song_library::search(&others, &projects, &picker.query, true);

    dim(ctx, "library_dim");
    let mut open = !ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let (mut add, mut cancel) = (false, false);
    let width = (ctx.content_rect().width() - 80.0).clamp(400.0, 720.0);
    egui::Window::new("Add from Library")
        .id(Id::new("library_picker"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .default_width(width)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.label(
                RichText::new("Songs from your other setlists come with their stems, lyrics, timing, cues and mix. No need to split them again.")
                    .size(11.5)
                    .color(TEXT_DIM),
            );
            ui.add_space(6.0);
            search_field(ui, &mut picker.query, "Search by title, artist or lyrics…", width).request_focus();
            ui.add_space(4.0);
            let list_h = (ctx.content_rect().height() - 300.0).clamp(160.0, 460.0);
            egui::ScrollArea::vertical().max_height(list_h).min_scrolled_height(list_h).auto_shrink([false, true]).show(ui, |ui| {
                if hits.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if others.is_empty() {
                            "Your other setlists don't have any songs yet.".to_owned()
                        } else {
                            format!("No songs match “{}”", picker.query.trim())
                        };
                        ui.label(RichText::new(msg).color(TEXT_DIM));
                    });
                    ui.add_space(20.0);
                }
                for h in &hits {
                    let on = picker.selected.contains(&h.song.id);
                    let stems = app.song_split_stems(&h.song);
                    let mut r = song_row(ui, h, stems, false, Some(on));
                    if here.contains(&h.song.original_path.to_lowercase()) {
                        r = r.on_hover_text("This song is already in this setlist");
                        ui.painter().text(r.rect.right_bottom() + vec2(-10.0, -5.0), Align2::RIGHT_BOTTOM, "already in this setlist", FontId::proportional(9.5), TEXT_FAINT);
                    }
                    if r.clicked() {
                        if on {
                            picker.selected.retain(|&x| x != h.song.id);
                        } else {
                            picker.selected.push(h.song.id);
                        }
                    }
                    if r.double_clicked() {
                        picker.selected = vec![h.song.id];
                        add = true;
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let n = picker.selected.len();
                ui.label(RichText::new(if n == 0 { "Click songs to select them".to_owned() } else { format!("{n} selected") }).color(TEXT_FAINT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if n <= 1 { "Add Song".to_owned() } else { format!("Add {n} Songs") };
                    let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(ACCENT).min_size(vec2(110.0, 30.0));
                    add |= ui.add_enabled(n > 0, btn).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        });
    if add && !picker.selected.is_empty() {
        app.add_from_library(std::mem::take(&mut picker.selected), ctx);
        return;
    }
    if open && !cancel {
        app.library = Some(picker);
    }
}

/// Make a fresh picker (for the button / menu).
pub fn open_library(app: &mut StageWeaveApp) {
    app.library = Some(LibraryPicker::default());
}
