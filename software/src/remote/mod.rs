//! Phone remote: control StageWeave from the `mobileapp/` over the local
//! Wi-Fi. Off until turned on in Settings.
//!
//! The server threads (`server.rs`) hand commands to the app through a
//! channel; `poll` applies them on the UI thread, through the same methods
//! the keyboard uses. `publish` turns the app's state into a `RemoteState`
//! each frame and shares it when it changes.

pub mod protocol;
pub mod server;

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::Duration;

use protocol::*;
pub use server::PairedDevice;
use server::{Server, Shared};

use crate::app::{Screen, StageWeaveApp};
use crate::store::Store;

const ENABLED_KEY: &str = "remote.enabled";
const ID_KEY: &str = "remote.id";
/// How long a "pair this phone" request stays on screen.
const PAIR_REQUEST_SHOWN: Duration = Duration::from_secs(120);

pub struct Remote {
    shared: Arc<Shared>,
    server: Option<Server>,
    tx: Sender<ClientMsg>,
    rx: Receiver<ClientMsg>,
    ctx: egui::Context,
    last: Option<RemoteState>,
    seq: u64,
    /// Why the server couldn't start.
    pub error: Option<String>,
}

impl Remote {
    pub fn new(store: Arc<Store>, ctx: egui::Context) -> Self {
        let id = store.setting(ID_KEY).filter(|s| !s.is_empty()).unwrap_or_else(|| {
            let id = server::random_hex(8);
            store.set_setting(ID_KEY, &id);
            id
        });
        let info = ServerInfo {
            name: format!("StageWeave on {}", os_name()),
            host: host_name(),
            id,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: VERSION,
        };
        let (tx, rx) = channel();
        Self { shared: Shared::new(store, info), server: None, tx, rx, ctx, last: None, seq: 0, error: None }
    }

    /// Start the server if it was left on.
    pub fn start_if_enabled(&mut self, store: &Store) {
        if store.setting(ENABLED_KEY).as_deref() == Some("1") {
            self.start();
        }
    }

    pub fn set_enabled(&mut self, store: &Store, on: bool) {
        store.set_setting(ENABLED_KEY, if on { "1" } else { "0" });
        if on { self.start() } else { self.stop() }
    }

    pub fn start(&mut self) {
        if self.server.is_some() {
            return;
        }
        match Server::start(self.shared.clone(), self.tx.clone(), self.ctx.clone()) {
            Ok(s) => {
                self.server = Some(s);
                self.error = None;
                self.last = None; // publish again for new phones
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn stop(&mut self) {
        self.server = None;
        self.shared.clients.lock().unwrap().clear();
        self.shared.pairing.lock().unwrap().request = None;
    }

    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    pub fn port(&self) -> Option<u16> {
        self.server.as_ref().map(|s| s.port)
    }

    pub fn info(&self) -> &ServerInfo {
        &self.shared.info
    }

    /// The code a phone enters to pair.
    pub fn pin(&self) -> String {
        self.shared.pairing.lock().unwrap().pin.clone()
    }

    /// Names of the connected phones.
    pub fn clients(&self) -> Vec<String> {
        if !self.running() {
            return Vec::new();
        }
        self.shared.clients.lock().unwrap().clone()
    }

    /// A phone waiting for the code (its name).
    pub fn pair_request(&self) -> Option<String> {
        if !self.running() {
            return None;
        }
        let p = self.shared.pairing.lock().unwrap();
        p.request.as_ref().filter(|(_, at)| at.elapsed() < PAIR_REQUEST_SHOWN).map(|(n, _)| n.clone())
    }

    /// Fix the pairing code (dev screenshots).
    pub fn set_pin(&self, pin: &str) {
        self.shared.pairing.lock().unwrap().pin = pin.to_owned();
    }

    pub fn dismiss_pair_request(&self) {
        self.shared.pairing.lock().unwrap().request = None;
    }

    pub fn devices(&self) -> Vec<PairedDevice> {
        self.shared.pairing.lock().unwrap().devices.clone()
    }

    /// Unpair a phone (it's disconnected and has to enter a code again).
    pub fn forget(&self, token: &str) {
        self.shared.forget(token);
    }

    /// "192.168.1.20:47800" for each local network interface.
    pub fn addresses(&self) -> Vec<String> {
        let Some(port) = self.port() else { return Vec::new() };
        server::local_addresses().into_iter().map(|(_, ip)| format!("{ip}:{port}")).collect()
    }

    fn drain(&self) -> Vec<ClientMsg> {
        self.rx.try_iter().collect()
    }
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

fn host_name() -> String {
    let h = gethostname::gethostname().to_string_lossy().into_owned();
    let h = h.trim_end_matches('.').trim_end_matches(".local").trim_end_matches(".lan").to_owned();
    if h.is_empty() { "computer".into() } else { h }
}

/// Apply the phones' commands (UI thread, once per frame).
pub fn poll(app: &mut StageWeaveApp) {
    for msg in app.remote.drain() {
        apply(app, msg);
    }
}

/// Share the current state with the phones when it changed.
pub fn publish(app: &mut StageWeaveApp) {
    if !app.remote.running() || app.remote.shared.clients.lock().unwrap().is_empty() {
        return;
    }
    let state = state(app);
    if app.remote.last.as_ref() == Some(&state) {
        return;
    }
    app.remote.seq += 1;
    let frame = serde_json::to_string(&ServerMsg::State { state: state.clone() }).unwrap_or_default();
    app.remote.shared.state.store(Arc::new((app.remote.seq, frame)));
    app.remote.last = Some(state);
}

/// What the phone shows.
pub fn state(app: &StageWeaveApp) -> RemoteState {
    let projects: Vec<ProjectInfo> = app.projects.iter().map(|p| ProjectInfo { id: p.id, name: p.name.clone(), items: p.song_count as usize }).collect();
    let project = app.current_project().map(|p| ProjectInfo { id: p.id, name: p.name.clone(), items: app.songs.len() });
    let items = if project.is_some() {
        app.songs
            .iter()
            .map(|s| ItemInfo {
                id: s.id,
                title: s.title.clone(),
                artist: s.artist.clone(),
                kind: s.kind.as_str().to_owned(),
                duration: round(s.duration),
                ready: s.imported && !app.importer.progress.contains_key(&s.id),
            })
            .collect()
    } else {
        Vec::new()
    };
    let current = app.current_song().map(|s| {
        let marker = |name: &str, start: f64, end: f64| Marker { name: name.to_owned(), start: round(start), end: round(end) };
        Current {
            id: s.id,
            title: s.title.clone(),
            artist: s.artist.clone(),
            kind: s.kind.as_str().to_owned(),
            playing: app.is_playing(),
            loading: app.is_loading(),
            position: round(app.position()),
            duration: round(app.session.duration()),
            looping: app.loop_enabled,
            cues: app.session.cues.iter().map(|c| marker(&c.name, c.start, c.end)).collect(),
            active_cue: app.cue.active,
            sections: app.session.sections.iter().map(|c| marker(&c.name, c.start, c.end)).collect(),
            current_section: app.section.current,
            tracks: app
                .session
                .tracks
                .iter()
                .map(|t| TrackInfo {
                    name: t.name.clone(),
                    kind: t.kind.label().to_lowercase(),
                    muted: t.muted,
                    solo: t.solo,
                    gain_db: (t.gain_db * 10.0).round() / 10.0,
                })
                .collect(),
            master_db: (app.master_db * 10.0).round() / 10.0,
            deck: app.deck.as_ref().map(|d| DeckInfo { index: d.idx, count: d.len() }),
        }
    });
    RemoteState { projects, project, items, current }
}

/// Tenths of a second: enough for the phone, and fewer updates.
fn round(t: f64) -> f64 {
    (t * 10.0).round() / 10.0
}

/// Carry out one command, the same way the keyboard and mouse do.
pub fn apply(app: &mut StageWeaveApp, msg: ClientMsg) {
    let ctx = app.ctx.clone();
    match msg {
        ClientMsg::OpenProject { id } => {
            if app.projects.iter().any(|p| p.id == id) && app.project_id() != Some(id) {
                app.open_project(id);
            }
        }
        ClientMsg::OpenItem { id } => {
            if app.current_song_id() != Some(id) && app.songs.iter().any(|s| s.id == id && s.imported) {
                app.open_song(id, &ctx);
            }
        }
        ClientMsg::Back => match app.screen {
            Screen::Song { .. } => app.leave_song(),
            Screen::Project(_) => app.go_home(),
            Screen::Home => {}
        },
        ClientMsg::Play => {
            if app.deck.is_some() {
                if !app.is_playing() && app.deck.as_ref().is_some_and(|d| !d.current_media().is_empty()) {
                    app.media_toggle();
                }
            } else if !app.is_playing() {
                app.toggle_play();
            }
        }
        ClientMsg::Pause => {
            if app.deck.is_some() {
                if app.is_playing() {
                    app.media_toggle();
                }
            } else if app.is_playing() {
                app.toggle_play();
            }
        }
        ClientMsg::Toggle => {
            if app.deck.is_some() {
                if app.deck.as_ref().is_some_and(|d| !d.current_media().is_empty()) {
                    app.media_toggle();
                }
            } else {
                app.toggle_play();
            }
        }
        ClientMsg::Stop => app.stop(),
        ClientMsg::Seek { t } => {
            if t.is_finite() {
                app.seek(t);
            }
        }
        ClientMsg::Next => {
            if app.current_song_id().is_none() {
                // From the setlist: open the first item.
                if let Some(first) = app.songs.iter().find(|s| s.imported).map(|s| s.id) {
                    app.open_song(first, &ctx);
                }
            } else if !app.deck_next() {
                app.next_song(&ctx);
            }
        }
        ClientMsg::Prev => {
            if app.current_song_id().is_some() && !app.deck_prev() {
                app.prev_song(&ctx);
            }
        }
        ClientMsg::Loop { on } => app.loop_enabled = on,
        ClientMsg::Cue { i } => {
            if i < app.session.cues.len() {
                app.activate_cue(i);
            }
        }
        ClientMsg::ExitCue => app.exit_cue(),
        ClientMsg::Section { i } => {
            if let Some(start) = app.session.sections.get(i).map(|s| s.start) {
                if app.cue.active.is_some() {
                    app.exit_cue();
                }
                app.seek(start);
            }
        }
        ClientMsg::Track { i, muted, solo, gain_db } => {
            let Some(t) = app.session.tracks.get_mut(i) else { return };
            if let Some(m) = muted {
                t.muted = m;
            }
            if let Some(s) = solo {
                t.solo = s;
            }
            if let Some(g) = gain_db.filter(|g| g.is_finite()) {
                t.gain_db = g.clamp(crate::model::MIN_DB, crate::model::MAX_DB);
            }
            app.sync_mix();
        }
        ClientMsg::Master { gain_db } => {
            if gain_db.is_finite() {
                app.master_db = gain_db.clamp(crate::model::MIN_DB, crate::model::MAX_DB);
                app.sync_mix();
            }
        }
        ClientMsg::Hello { .. } | ClientMsg::Pair { .. } | ClientMsg::Ping => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cue, Section, StemKind, Track};
    use std::net::TcpStream;
    use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

    /// A demo app with a setlist of two songs; returns their ids.
    fn setlist() -> (StageWeaveApp, i64, i64) {
        let mut app = StageWeaveApp::with_context(&egui::Context::default(), crate::dev::Dev::demo());
        let root = std::env::temp_dir().join(format!("sw-remote-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        app.ws.set_root(&root);
        let pid = app.create_project("Sunday");
        app.open_project(pid);
        let folder = app.store.project(pid).unwrap().folder;
        let mut ids = Vec::new();
        for name in ["one", "two"] {
            let path = format!("{folder}/songs/{name}.mp3");
            std::fs::create_dir_all(app.ws.abs(&path).parent().unwrap()).unwrap();
            std::fs::write(app.ws.abs(&path), b"x").unwrap();
            let id = app.store.add_song(pid, &path, "/x.mp3", name).unwrap();
            app.store.finish_import(id, 120.0, 44_100, &[]);
            ids.push(id);
        }
        app.reload_songs();
        (app, ids[0], ids[1])
    }

    /// Pretend the open song finished loading, with stems, cues and sections.
    fn loaded(app: &mut StageWeaveApp) {
        app.session.source = app.decoder.load(std::path::Path::new("song.mp3")).ok();
        app.load_job = None;
        app.session.tracks = [StemKind::Vocals, StemKind::Drums].into_iter().map(|k| Track::new(k, Default::default(), None, None)).collect();
        app.session.cues = vec![Cue { name: "Chorus".into(), start: 30.0, end: 40.0, mix: None }];
        app.session.sections = vec![Section { name: "Bridge".into(), start: 50.0, end: 60.0, mix: None }];
    }

    #[test]
    fn commands_drive_the_app_like_the_keyboard() {
        let (mut app, one, two) = setlist();
        let s = state(&app);
        assert_eq!(s.project.as_ref().map(|p| p.name.as_str()), Some("Sunday"));
        assert_eq!(s.items.iter().map(|i| (i.title.as_str(), i.kind.as_str(), i.ready)).collect::<Vec<_>>(), [("one", "song", true), ("two", "song", true)]);
        assert!(s.current.is_none());

        apply(&mut app, ClientMsg::OpenItem { id: one });
        assert_eq!(app.current_song_id(), Some(one));
        loaded(&mut app);
        apply(&mut app, ClientMsg::Play);
        assert!(app.is_playing());
        apply(&mut app, ClientMsg::Toggle);
        assert!(!app.is_playing());
        apply(&mut app, ClientMsg::Seek { t: 12.0 });
        assert!((app.position() - 12.0).abs() < 0.05);

        apply(&mut app, ClientMsg::Cue { i: 0 });
        assert_eq!(app.cue.active, Some(0));
        apply(&mut app, ClientMsg::Section { i: 0 });
        assert_eq!(app.cue.active, None, "jumping to a section leaves the cue");
        assert!((app.position() - 50.0).abs() < 0.05);
        apply(&mut app, ClientMsg::Cue { i: 9 }); // out of range: ignored

        apply(&mut app, ClientMsg::Track { i: 1, muted: Some(true), solo: None, gain_db: Some(-6.0) });
        assert!(app.session.tracks[1].muted && !app.session.tracks[1].solo);
        assert_eq!(app.session.tracks[1].gain_db, -6.0);
        apply(&mut app, ClientMsg::Master { gain_db: 99.0 });
        assert_eq!(app.master_db, crate::model::MAX_DB, "clamped");
        apply(&mut app, ClientMsg::Loop { on: true });

        let c = state(&app).current.unwrap();
        assert_eq!((c.id, c.title.as_str(), c.looping), (one, "one", true));
        assert_eq!(c.cues[0].name, "Chorus");
        assert_eq!(c.sections[0].start, 50.0);
        assert_eq!(c.tracks.iter().map(|t| (t.kind.as_str(), t.muted)).collect::<Vec<_>>(), [("vocals", false), ("drums", true)]);

        apply(&mut app, ClientMsg::Next);
        assert_eq!(app.current_song_id(), Some(two));
        apply(&mut app, ClientMsg::Prev);
        assert_eq!(app.current_song_id(), Some(one));
        apply(&mut app, ClientMsg::Back);
        assert!(matches!(app.screen, Screen::Project(_)));
        apply(&mut app, ClientMsg::Next);
        assert_eq!(app.current_song_id(), Some(one), "Next from the setlist opens the first item");
        apply(&mut app, ClientMsg::Back);
        apply(&mut app, ClientMsg::Back);
        assert!(matches!(app.screen, Screen::Home));
        let s = state(&app);
        assert!(s.project.is_none() && s.items.is_empty());
        let sunday = s.projects.iter().find(|p| p.name == "Sunday").map(|p| p.id).unwrap();
        apply(&mut app, ClientMsg::OpenProject { id: sunday });
        assert_eq!(app.project_id(), Some(sunday));
        let _ = std::fs::remove_dir_all(app.ws.root());
    }

    /// The phone app parses the same file (`mobileapp/test/fixtures/state.json`).
    #[test]
    fn the_phone_fixture_matches_the_protocol() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../mobileapp/test/fixtures/state.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let msg: ServerMsg = serde_json::from_str(&text).unwrap();
        let ServerMsg::State { state } = &msg else { panic!("not a state message") };
        let c = state.current.as_ref().unwrap();
        assert_eq!((c.tracks.len(), c.cues.len(), c.sections.len(), c.looping), (2, 1, 2, true));
        // Nothing is lost going back to JSON.
        let again: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(again, serde_json::from_str::<serde_json::Value>(&text).unwrap());
        // Commands use the same names as the phone sends.
        let cmd: ClientMsg = serde_json::from_str(r#"{"type":"track","i":1,"muted":true}"#).unwrap();
        assert_eq!(cmd, ClientMsg::Track { i: 1, muted: Some(true), solo: None, gain_db: None });
        assert_eq!(serde_json::to_string(&ClientMsg::ExitCue).unwrap(), r#"{"type":"exit_cue"}"#);
    }

    #[test]
    fn only_local_network_addresses_are_allowed() {
        for ip in ["192.168.1.20", "10.3.2.4", "172.16.0.9", "127.0.0.1", "169.254.1.1", "::1", "fe80::1", "fd00::5", "::ffff:192.168.0.2"] {
            assert!(server::is_local(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.64.0.1", "2001:4860::8888", "::ffff:8.8.8.8"] {
            assert!(!server::is_local(ip.parse().unwrap()), "{ip}");
        }
    }

    type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

    fn send(ws: &mut Ws, msg: &ClientMsg) {
        ws.send(Message::text(serde_json::to_string(msg).unwrap())).unwrap();
    }

    fn recv(ws: &mut Ws) -> ServerMsg {
        loop {
            if let Message::Text(t) = ws.read().unwrap() {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    #[test]
    fn a_phone_pairs_with_the_code_then_controls_the_app() {
        let store = Arc::new(Store::in_memory().unwrap());
        let info = ServerInfo { name: "StageWeave on macOS".into(), host: "Test".into(), id: "abc".into(), version: "1".into(), protocol: VERSION };
        let shared = Shared::new(store.clone(), info);
        let (tx, rx) = channel();
        let server = Server::start_on(shared.clone(), tx, egui::Context::default(), "127.0.0.1", false).unwrap();
        let url = format!("ws://127.0.0.1:{}", server.port);

        let (mut ws, _) = tungstenite::connect(&url).unwrap();
        send(&mut ws, &ClientMsg::Hello { device: "Pixel".into(), token: None, v: Some(VERSION) });
        assert!(matches!(recv(&mut ws), ServerMsg::PairRequired { .. }));
        assert_eq!(shared.pairing.lock().unwrap().request.as_ref().map(|r| r.0.clone()), Some("Pixel".into()), "the computer shows the code");
        // Commands are refused until paired.
        send(&mut ws, &ClientMsg::Play);
        assert!(matches!(recv(&mut ws), ServerMsg::Error { .. }));
        send(&mut ws, &ClientMsg::Pair { device: "Pixel".into(), pin: "nope".into() });
        assert!(matches!(recv(&mut ws), ServerMsg::PairFailed { .. }));
        let pin = shared.pairing.lock().unwrap().pin.clone();
        send(&mut ws, &ClientMsg::Pair { device: "Pixel".into(), pin });
        let ServerMsg::Paired { token, server: info } = recv(&mut ws) else { panic!("not paired") };
        assert_eq!(info.name, "StageWeave on macOS");
        assert!(shared.pairing.lock().unwrap().request.is_none());
        drop(ws);

        // Next time the token is enough, and the device was saved.
        let saved: Vec<PairedDevice> = serde_json::from_str(&store.setting("remote.devices").unwrap()).unwrap();
        assert_eq!(saved[0].name, "Pixel");
        let (mut ws, _) = tungstenite::connect(&url).unwrap();
        send(&mut ws, &ClientMsg::Hello { device: "Pixel".into(), token: Some(token.clone()), v: Some(VERSION) });
        assert!(matches!(recv(&mut ws), ServerMsg::Welcome { .. }));
        // State published by the app reaches the phone.
        let state = RemoteState { project: Some(ProjectInfo { id: 1, name: "Sunday".into(), items: 0 }), ..Default::default() };
        shared.state.store(Arc::new((1, serde_json::to_string(&ServerMsg::State { state: state.clone() }).unwrap())));
        assert_eq!(recv(&mut ws), ServerMsg::State { state });
        // Commands reach the app.
        send(&mut ws, &ClientMsg::Toggle);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), ClientMsg::Toggle);
        assert_eq!(shared.clients.lock().unwrap().clone(), ["Pixel"]);

        // Removing the phone on the computer disconnects it.
        shared.forget(&token);
        assert!(matches!(recv(&mut ws), ServerMsg::Error { .. }));
        drop(ws);
        let (mut ws, _) = tungstenite::connect(&url).unwrap();
        send(&mut ws, &ClientMsg::Hello { device: "Pixel".into(), token: Some(token), v: Some(VERSION) });
        assert!(matches!(recv(&mut ws), ServerMsg::PairRequired { .. }));
        drop(server);
    }

    #[test]
    fn too_many_wrong_codes_change_the_code() {
        let shared = Shared::new(Arc::new(Store::in_memory().unwrap()), ServerInfo::default());
        let first = shared.pairing.lock().unwrap().pin.clone();
        for _ in 0..5 {
            let wrong = if first == "0000" { "1111" } else { "0000" };
            let _ = shared.pair("x", wrong);
        }
        assert_ne!(shared.pairing.lock().unwrap().pin, first);
    }
}
