//! Lyric line actions shared by the timeline's Lyrics track and the Timing
//! list in the Lyrics panel, so both edit the same way.

use egui::{RichText, Ui};

use crate::app::MusicKitApp;
use crate::theme::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Select(usize),
    /// Open the text editor (`true` = in the Timing list, `false` = on the timeline).
    Edit(usize, bool),
    Above(usize),
    Below(usize),
    Delete(usize),
}

/// Right-click menu for line `i`. Returns the chosen action.
pub fn context_menu(app: &MusicKitApp, ui: &mut Ui, i: usize, in_list: bool) -> Option<Act> {
    let line = app.session.lyrics.lines.get(i)?;
    let at = app.lyric_insert_time().map(|t| format!(" (at {})", timecode(t))).unwrap_or_default();
    let mut act = None;
    ui.label(RichText::new(format!("Line {} · {}", i + 1, timecode(line.start))).size(11.0).color(TEXT_FAINT));
    if ui.add(egui::Button::new("Edit Text").shortcut_text("Enter")).clicked() {
        act = Some(Act::Edit(i, in_list));
        ui.close();
    }
    ui.separator();
    if ui.button(format!("Insert Line Above{at}")).clicked() {
        act = Some(Act::Above(i));
        ui.close();
    }
    if ui.button(format!("Insert Line Below{at}")).clicked() {
        act = Some(Act::Below(i));
        ui.close();
    }
    if at.is_empty() {
        ui.label(RichText::new("Pause to insert at the playhead").size(10.5).color(TEXT_FAINT));
    }
    ui.separator();
    if ui.add(egui::Button::new(RichText::new("Delete Line").color(DANGER)).shortcut_text("⌫")).clicked() {
        act = Some(Act::Delete(i));
        ui.close();
    }
    act
}

/// Apply an action. Inserts from the Timing list open their editor there.
pub fn apply(app: &mut MusicKitApp, act: Act, in_list: bool) {
    match act {
        Act::Select(i) => app.lyric_clip_select(i),
        Act::Edit(i, list) => {
            app.lyric_begin_text_edit(i);
            app.lyric_edit.edit_in_list = list;
        }
        Act::Above(i) => {
            app.lyric_insert_above(i);
            app.lyric_edit.edit_in_list = in_list;
        }
        Act::Below(i) => {
            app.lyric_insert_below(i);
            app.lyric_edit.edit_in_list = in_list;
        }
        Act::Delete(i) => app.lyric_delete_at(i),
    }
}
