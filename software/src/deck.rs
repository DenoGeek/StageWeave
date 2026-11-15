//! Presenting a slide deck from the setlist: which slide is showing, slide
//! textures (loaded in the background), and the slide's video or sound,
//! which plays through the normal audio engine so the master fader, meters
//! and transport all work, with the video kept in sync with the playhead.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{ColorImage, TextureHandle, TextureOptions, Vec2};

use crate::app::{StageWeaveApp, ToastKind};
use crate::backgrounds::SyncedVideo;
use crate::engine::LoadJob;
use crate::slides::{Deck, MediaKind, SlideMedia};

/// Slide image sizes kept as textures.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Size {
    Full,
    Thumb,
}

pub struct DeckState {
    pub song_id: i64,
    pub dir: PathBuf,
    pub deck: Deck,
    pub idx: usize,
    textures: HashMap<(usize, Size), TextureHandle>,
    loading: HashMap<(usize, Size), Receiver<Option<ColorImage>>>,
    pub media: Option<MediaPlayer>,
}

/// The slide media that's loaded (playing or paused).
pub struct MediaPlayer {
    pub slide: usize,
    pub item: usize,
    pub video: Option<SyncedVideo>,
    frame: Option<TextureHandle>,
    load: Option<LoadJob>,
    /// Start playing as soon as the audio is loaded.
    play_when_loaded: bool,
    pub duration: f64,
}

impl MediaPlayer {
    pub fn loading(&self) -> bool {
        self.load.is_some()
    }
}

impl DeckState {
    pub fn open(song_id: i64, dir: PathBuf) -> Result<Self, String> {
        let deck = Deck::load(&dir)?;
        Ok(Self { song_id, dir, deck, idx: 0, textures: HashMap::new(), loading: HashMap::new(), media: None })
    }

    pub fn len(&self) -> usize {
        self.deck.slides.len()
    }

    pub fn current_media(&self) -> &[SlideMedia] {
        self.deck.slides.get(self.idx).map_or(&[], |s| s.media.as_slice())
    }

    /// Slide `i` as a texture, if loaded; starts loading it otherwise.
    pub fn texture(&mut self, ctx: &egui::Context, i: usize, size: Size) -> Option<(egui::TextureId, Vec2)> {
        let key = (i, size);
        if let Some(t) = self.textures.get(&key) {
            return Some((t.id(), t.size_vec2()));
        }
        if let Some(rx) = self.loading.get(&key) {
            if let Ok(img) = rx.try_recv() {
                self.loading.remove(&key);
                let tex = ctx.load_texture(format!("slide-{}-{i}-{size:?}", self.song_id), img?, TextureOptions::LINEAR);
                let out = (tex.id(), tex.size_vec2());
                self.textures.insert(key, tex);
                return Some(out);
            }
            return None;
        }
        let path = self.dir.join(&self.deck.slides.get(i)?.image);
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let img = image::open(&path).ok().map(|img| {
                let img = if size == Size::Thumb { img.thumbnail(320, 320) } else { img };
                let rgba = img.to_rgba8();
                ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw())
            });
            let _ = tx.send(img);
            ctx2.request_repaint();
        });
        self.loading.insert(key, rx);
        None
    }

    /// Keep full-size textures only near the current slide.
    fn trim(&mut self) {
        let keep = self.idx.saturating_sub(1)..=self.idx + 2;
        self.textures.retain(|(i, s), _| *s == Size::Thumb || keep.contains(i));
    }

    /// The video frame for the playing media, as a texture.
    pub fn media_frame(&mut self, ctx: &egui::Context, t: f64) -> Option<egui::TextureId> {
        let m = self.media.as_mut()?;
        let v = m.video.as_mut()?;
        if let Some(frame) = v.frame_at(t) {
            let img = ColorImage::from_rgba_premultiplied(v.size(), &frame);
            match &mut m.frame {
                Some(tex) => tex.set(img, TextureOptions::LINEAR),
                None => m.frame = Some(ctx.load_texture("slide-media", img, TextureOptions::LINEAR)),
            }
        }
        m.frame.as_ref().map(|t| t.id())
    }
}

impl StageWeaveApp {
    /// Open a deck item (from `open_song`).
    pub fn open_deck(&mut self, song_id: i64, dir: PathBuf) {
        match DeckState::open(song_id, dir) {
            Ok(d) => self.deck = Some(d),
            Err(e) => {
                self.deck = None;
                self.toast(ToastKind::Error, e);
            }
        }
        self.deck_entered();
    }

    /// Show slide `i`. Media on the old slide stops; autoplay media starts.
    pub fn deck_go(&mut self, i: usize) {
        let Some(d) = self.deck.as_mut() else { return };
        let i = i.min(d.len().saturating_sub(1));
        if i == d.idx {
            return;
        }
        d.idx = i;
        d.trim();
        self.deck_entered();
    }

    fn deck_entered(&mut self) {
        self.media_stop();
        let Some(d) = self.deck.as_ref() else { return };
        if let Some(k) = d.current_media().iter().position(|m| m.autoplay) {
            self.media_start(k, true);
        } else if !d.current_media().is_empty() {
            // Load the first media paused, ready for Space / click.
            self.media_start(0, false);
        }
    }

    /// Next slide; `true` if there was one.
    pub fn deck_next(&mut self) -> bool {
        let Some(d) = self.deck.as_ref() else { return false };
        if d.idx + 1 >= d.len() {
            return false;
        }
        self.deck_go(d.idx + 1);
        true
    }

    /// Previous slide; `true` if there was one.
    pub fn deck_prev(&mut self) -> bool {
        let Some(d) = self.deck.as_ref() else { return false };
        if d.idx == 0 {
            return false;
        }
        self.deck_go(d.idx - 1);
        true
    }

    /// Space: play / pause the slide's media, or go to the next slide.
    pub fn deck_space(&mut self, ctx: &egui::Context) {
        let has_media = self.deck.as_ref().is_some_and(|d| !d.current_media().is_empty());
        if !has_media {
            if !self.deck_next() {
                self.next_song(ctx);
            }
            return;
        }
        self.media_toggle();
    }

    /// Load media `item` of the current slide; play it if `play`.
    pub fn media_start(&mut self, item: usize, play: bool) {
        self.media_stop();
        let Some(d) = self.deck.as_mut() else { return };
        let Some(m) = d.current_media().get(item).cloned() else { return };
        let video = match (m.kind, self.ffmpeg.path()) {
            (MediaKind::Video, Some(ff)) => Some(SyncedVideo::new(&ff, &d.dir.join(&m.file), 1280, 720, self.ctx.clone())),
            _ => None,
        };
        // Its audio: the prepared WAV, else the file itself (mp3, m4a, mp4…).
        let audio = d.dir.join(m.audio.as_ref().unwrap_or(&m.file));
        let load = Some(LoadJob::spawn(self.decoder.clone(), audio, d.song_id, self.ctx.clone()));
        d.media = Some(MediaPlayer { slide: d.idx, item, video, frame: None, load, play_when_loaded: play, duration: 0.0 });
    }

    pub fn media_stop(&mut self) {
        self.playback.pause();
        self.playback.seek(0.0);
        self.playback.set_tracks(Vec::new());
        self.session.source = None;
        if let Some(d) = self.deck.as_mut() {
            d.media = None;
        }
    }

    pub fn media_toggle(&mut self) {
        let Some(d) = self.deck.as_mut() else { return };
        match d.media.as_mut() {
            Some(m) if m.load.is_some() => m.play_when_loaded = !m.play_when_loaded,
            Some(m) if m.slide == d.idx => {
                if self.playback.is_playing() {
                    self.playback.pause();
                } else {
                    if m.duration > 0.0 && self.playback.position() >= m.duration - 0.05 {
                        self.playback.seek(0.0);
                    }
                    self.playback.play();
                }
            }
            _ => self.media_start(0, true),
        }
    }

    /// Per frame: finish loading media audio, loop or stop at the end.
    pub fn deck_tick(&mut self) {
        let Some(d) = self.deck.as_mut() else { return };
        let Some(m) = d.media.as_mut() else { return };
        if let Some(job) = &m.load
            && let Ok(result) = job.rx.try_recv()
        {
            m.load = None;
            match result {
                Ok(src) => {
                    m.duration = src.info.duration;
                    let play = m.play_when_loaded;
                    self.playback.set_tracks(src.audio.iter().cloned().collect());
                    self.session.source = Some(src);
                    self.sync_mix();
                    self.playback.seek(0.0);
                    if play {
                        self.playback.play();
                    }
                }
                Err(e) => {
                    let item = m.item;
                    let name = d.current_media().get(item).map(|x| x.file.clone()).unwrap_or_default();
                    d.media = None;
                    self.toast(ToastKind::Error, format!("Can't play {name}: {e}"));
                }
            }
            return;
        }
        let looping = d.deck.slides.get(m.slide).and_then(|s| s.media.get(m.item)).is_some_and(|x| x.looping);
        if m.duration > 0.0 && self.playback.position() >= m.duration - 0.03 {
            if looping {
                self.playback.seek(0.0);
            } else if self.playback.is_playing() {
                self.playback.pause();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{Screen, StageWeaveApp};
    use crate::model::{ItemKind, ItemSource};
    use crate::slides::{Deck, Slide};
    use egui_kittest::{Harness, kittest::Queryable};

    /// A setlist: a 3-slide deck, then a song.
    fn setlist() -> (StageWeaveApp, i64, i64) {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        let root = std::env::temp_dir().join(format!("sw-deck-ui-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        app.ws.set_root(&root);
        let pid = app.create_project("Sunday");
        app.open_project(pid);
        let folder = app.store.project(pid).unwrap().folder;
        let dir = format!("{folder}/decks/Welcome");
        let abs = app.ws.abs(&dir);
        std::fs::create_dir_all(&abs).unwrap();
        let deck = Deck { slides: (1..=3).map(|i| Slide { image: format!("slide-{i:03}.png"), media: Vec::new() }).collect() };
        for s in &deck.slides {
            image::RgbImage::new(16, 9).save(abs.join(&s.image)).unwrap();
        }
        std::fs::write(abs.join("deck.json"), serde_json::to_string(&deck).unwrap()).unwrap();
        let deck_id = app.store.add_item(pid, ItemKind::Deck, &format!("{dir}/deck.json"), "/w.pptx", "Welcome", &ItemSource::Deck { dir, slides: 3 }).unwrap();
        app.store.finish_import(deck_id, 0.0, 0, &[]);
        let song_path = format!("{folder}/songs/song.mp3");
        std::fs::create_dir_all(app.ws.abs(&song_path).parent().unwrap()).unwrap();
        std::fs::write(app.ws.abs(&song_path), b"x").unwrap();
        let song_id = app.store.add_song(pid, &song_path, "/song.mp3", "Song").unwrap();
        app.store.finish_import(song_id, 200.0, 44_100, &[]);
        app.reload_songs();
        (app, deck_id, song_id)
    }

    #[test]
    fn keys_move_through_slides_then_on_to_the_next_item() {
        let (mut app, deck_id, song_id) = setlist();
        let ctx = app.ctx.clone();
        app.open_song(deck_id, &ctx);
        assert!(matches!(app.screen, Screen::Song { .. }));
        assert_eq!(app.deck.as_ref().map(|d| (d.idx, d.len())), Some((0, 3)));
        assert!(app.can_project(), "slides can go on the projector");
        let mut h = Harness::new_ui_state(|ui, app: &mut StageWeaveApp| app.handle_input(ui.ctx()), app);
        h.run_steps(1);
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(1);
        h.key_press(egui::Key::Space); // no media: next slide
        h.run_steps(1);
        assert_eq!(h.state().deck.as_ref().unwrap().idx, 2);
        h.key_press(egui::Key::ArrowLeft);
        h.run_steps(1);
        assert_eq!(h.state().deck.as_ref().unwrap().idx, 1);
        h.key_press(egui::Key::End);
        h.run_steps(1);
        // Past the last slide: the next item in the setlist opens.
        h.key_press(egui::Key::PageDown);
        h.run_steps(1);
        assert_eq!(h.state().current_song_id(), Some(song_id));
        assert!(h.state().deck.is_none());
        // And back again.
        h.key_press(egui::Key::PageUp);
        h.run_steps(1);
        assert_eq!(h.state().current_song_id(), Some(deck_id));
        let _ = std::fs::remove_dir_all(h.state().ws.root());
    }

    #[test]
    fn the_deck_view_draws_the_slide() {
        let (mut app, deck_id, _) = setlist();
        let ctx = app.ctx.clone();
        app.open_song(deck_id, &ctx);
        let mut h = Harness::builder().with_size(egui::vec2(1400.0, 800.0)).build_ui_state(|ui, app: &mut StageWeaveApp| crate::ui::draw(app, ui), app);
        // Textures load in the background.
        for _ in 0..50 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = h.get_by_label("Slide 1 of 3");
        let _ = std::fs::remove_dir_all(h.state().ws.root());
    }
}
