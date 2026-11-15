//! Messages between StageWeave and the phone remote (`mobileapp/`), as JSON
//! text frames over a WebSocket. Every message has a `type`. Keep this in
//! step with `mobileapp/lib/protocol.dart`.

use serde::{Deserialize, Serialize};

/// Bumped when a change would break older phone apps.
pub const VERSION: u32 = 1;

/// mDNS service the phone browses for.
pub const SERVICE_TYPE: &str = "_stageweave._tcp.local.";

/// Phone → StageWeave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First message. A known `token` is let straight in.
    Hello { device: String, token: Option<String>, v: Option<u32> },
    /// The 4-digit code shown on the computer.
    Pair { device: String, pin: String },
    OpenProject { id: i64 },
    /// Open a setlist item (song, YouTube song or slides).
    OpenItem { id: i64 },
    /// Close the open item (back to the setlist), or the setlist (back home).
    Back,
    Play,
    Pause,
    Toggle,
    Stop,
    Seek { t: f64 },
    /// Next slide, or the next setlist item.
    Next,
    Prev,
    Loop { on: bool },
    /// Loop cue `i`.
    Cue { i: usize },
    ExitCue,
    /// Jump to the start of section `i`.
    Section { i: usize },
    /// Change stem `i`; fields left out stay as they are.
    Track {
        i: usize,
        #[serde(default)]
        muted: Option<bool>,
        #[serde(default)]
        solo: Option<bool>,
        #[serde(default)]
        gain_db: Option<f32>,
    },
    Master { gain_db: f32 },
    Ping,
}

/// StageWeave → phone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { server: ServerInfo },
    /// Ask for the code shown on the computer.
    PairRequired { server: ServerInfo },
    /// Keep this to connect without a code next time.
    Paired { token: String, server: ServerInfo },
    PairFailed { message: String },
    State { state: RemoteState },
    Error { message: String },
    Pong,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// "StageWeave on macOS"
    pub name: String,
    /// The computer's name.
    pub host: String,
    /// Stays the same across launches, so a phone can remember its token.
    pub id: String,
    pub version: String,
    pub protocol: u32,
}

/// Everything the phone shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoteState {
    pub projects: Vec<ProjectInfo>,
    /// The open setlist.
    pub project: Option<ProjectInfo>,
    pub items: Vec<ItemInfo>,
    /// The open item.
    pub current: Option<Current>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: i64,
    pub name: String,
    pub items: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: i64,
    pub title: String,
    pub artist: String,
    /// "song", "youtube" or "deck".
    pub kind: String,
    pub duration: f64,
    /// Imported (or downloaded / converted) and ready to open.
    pub ready: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Current {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub kind: String,
    pub playing: bool,
    pub loading: bool,
    pub position: f64,
    pub duration: f64,
    #[serde(rename = "loop")]
    pub looping: bool,
    pub cues: Vec<Marker>,
    pub active_cue: Option<usize>,
    pub sections: Vec<Marker>,
    pub current_section: Option<usize>,
    /// Empty when the item isn't split (only the original mix plays).
    pub tracks: Vec<TrackInfo>,
    pub master_db: f32,
    pub deck: Option<DeckInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub name: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub name: String,
    /// "vocals", "drums", …
    pub kind: String,
    pub muted: bool,
    pub solo: bool,
    pub gain_db: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckInfo {
    /// 0-based slide index.
    pub index: usize,
    pub count: usize,
}
