//! SQLite persistence: settings, projects (setlists) and their songs, model
//! download state and separation history.
//!
//! Lives at `<app data>/stageweave.db`. Schema changes go through `migrate`,
//! keyed on `PRAGMA user_version`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{ItemKind, ItemSource};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelRecord {
    pub key: String,
    pub downloaded: bool,
    pub bytes_on_disk: u64,
    /// Download size, once known (fetched from the server or measured).
    pub bytes_total: Option<u64>,
    pub downloaded_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub use_count: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeparationRecord {
    pub source_path: String,
    pub model_key: String,
    pub out_dir: String,
    pub stems: u32,
    pub seconds: f64,
    pub created_at: i64,
}

pub struct Store {
    conn: Mutex<Connection>,
    /// Read caches (the UI reads every frame); cleared on every write.
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    settings: std::collections::HashMap<String, Option<String>>,
    models: Option<Vec<ModelRecord>>,
    recent: Option<(usize, Vec<SeparationRecord>)>,
    seconds: std::collections::HashMap<String, Option<f64>>,
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let conn = Connection::open(path).map_err(|e| format!("Can't open database: {e}"))?;
        Self::with_connection(conn)
    }

    pub fn in_memory() -> Result<Self, String> {
        Self::with_connection(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn with_connection(conn: Connection) -> Result<Self, String> {
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(|e| e.to_string())?;
        migrate(&conn).map_err(|e| format!("Database migration failed: {e}"))?;
        Ok(Self { conn: Mutex::new(conn), cache: Mutex::default() })
    }

    fn invalidate(&self) {
        *self.cache.lock().unwrap() = Cache::default();
    }

    pub fn models(&self) -> Vec<ModelRecord> {
        if let Some(m) = &self.cache.lock().unwrap().models {
            return m.clone();
        }
        let rows = self.query_models();
        self.cache.lock().unwrap().models = Some(rows.clone());
        rows
    }

    fn query_models(&self) -> Vec<ModelRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT key, downloaded, bytes_on_disk, bytes_total, downloaded_at, last_used_at, use_count, error
             FROM models",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| {
            Ok(ModelRecord {
                key: r.get(0)?,
                downloaded: r.get(1)?,
                bytes_on_disk: r.get::<_, i64>(2)? as u64,
                bytes_total: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                downloaded_at: r.get(4)?,
                last_used_at: r.get(5)?,
                use_count: r.get(6)?,
                error: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    /// Record what is on disk. A known total is never overwritten with `None`.
    pub fn set_model_status(&self, key: &str, downloaded: bool, on_disk: u64, total: Option<u64>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO models (key, downloaded, bytes_on_disk, bytes_total, downloaded_at)
             VALUES (?1, ?2, ?3, ?4, CASE WHEN ?2 THEN ?5 END)
             ON CONFLICT(key) DO UPDATE SET
                downloaded = ?2,
                bytes_on_disk = ?3,
                bytes_total = COALESCE(?4, bytes_total),
                downloaded_at = CASE WHEN ?2 THEN COALESCE(downloaded_at, ?5) ELSE NULL END,
                error = CASE WHEN ?2 THEN NULL ELSE error END",
            params![key, downloaded, on_disk as i64, total.map(|t| t as i64), now()],
        );
        drop(conn);
        self.invalidate();
    }

    pub fn set_model_error(&self, key: &str, error: Option<&str>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO models (key, error) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET error = ?2",
            params![key, error],
        );
        drop(conn);
        self.invalidate();
    }

    pub fn touch_model(&self, key: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO models (key, last_used_at, use_count) VALUES (?1, ?2, 1)
             ON CONFLICT(key) DO UPDATE SET last_used_at = ?2, use_count = use_count + 1",
            params![key, now()],
        );
        drop(conn);
        self.invalidate();
    }

    pub fn record_separation(&self, rec: &SeparationRecord) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO separations (source_path, model_key, out_dir, stems, seconds, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(out_dir) DO UPDATE SET stems = ?4, seconds = ?5, created_at = ?6",
            params![rec.source_path, rec.model_key, rec.out_dir, rec.stems, rec.seconds, rec.created_at],
        );
        drop(conn);
        self.invalidate();
    }

    pub fn recent_separations(&self, limit: usize) -> Vec<SeparationRecord> {
        if let Some((l, r)) = &self.cache.lock().unwrap().recent
            && *l == limit
        {
            return r.clone();
        }
        let rows = self.query_recent(limit);
        self.cache.lock().unwrap().recent = Some((limit, rows.clone()));
        rows
    }

    fn query_recent(&self, limit: usize) -> Vec<SeparationRecord> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT source_path, model_key, out_dir, stems, seconds, created_at
             FROM separations ORDER BY created_at DESC LIMIT ?1",
        ) else {
            return Vec::new();
        };
        stmt.query_map([limit as i64], |r| {
            Ok(SeparationRecord {
                source_path: r.get(0)?,
                model_key: r.get(1)?,
                out_dir: r.get(2)?,
                stems: r.get(3)?,
                seconds: r.get(4)?,
                created_at: r.get(5)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn separation_seconds(&self, model_key: &str) -> Option<f64> {
        if let Some(v) = self.cache.lock().unwrap().seconds.get(model_key) {
            return *v;
        }
        let conn = self.conn.lock().unwrap();
        let v = conn.query_row(
            "SELECT AVG(seconds) FROM separations WHERE model_key = ?1 AND seconds > 0",
            [model_key],
            |r| r.get::<_, Option<f64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten();
        drop(conn);
        self.cache.lock().unwrap().seconds.insert(model_key.to_owned(), v);
        v
    }
}

// --------------------------------------------------------------------------- projects & songs

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectRow {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub default_model: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub song_count: u32,
    pub total_duration: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongRow {
    pub id: i64,
    pub project_id: i64,
    pub position: u32,
    /// Relative to the workspace.
    pub path: String,
    pub original_path: String,
    pub imported: bool,
    pub title: String,
    pub artist: String,
    pub duration: f64,
    pub sample_rate: u32,
    pub model_key: Option<String>,
    pub song_key: String,
    pub bpm: Option<f64>,
    pub notes: String,
    pub lyrics_raw: String,
    pub lyric_lines: Option<String>,
    pub lyric_style: Option<String>,
    pub mixer: Option<String>,
    pub thumb: Vec<u8>,
    pub kind: ItemKind,
    pub source: Option<ItemSource>,
}

impl SongRow {
    pub fn effective_model<'a>(&'a self, project: &'a ProjectRow) -> &'a str {
        self.model_key.as_deref().unwrap_or(&project.default_model)
    }
}

/// Editable song metadata (the details panel).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SongMeta {
    pub title: String,
    pub artist: String,
    pub song_key: String,
    pub bpm: Option<f64>,
    pub notes: String,
    pub model_key: Option<String>,
}

const SONG_COLUMNS: &str = "id, project_id, position, path, original_path, imported, title, artist, duration,
    sample_rate, model_key, song_key, bpm, notes, lyrics_raw, lyric_lines, lyric_style, mixer, thumb, kind, source";

fn song_from_row(r: &rusqlite::Row) -> rusqlite::Result<SongRow> {
    Ok(SongRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        position: r.get(2)?,
        path: r.get(3)?,
        original_path: r.get(4)?,
        imported: r.get(5)?,
        title: r.get(6)?,
        artist: r.get(7)?,
        duration: r.get(8)?,
        sample_rate: r.get(9)?,
        model_key: r.get(10)?,
        song_key: r.get(11)?,
        bpm: r.get(12)?,
        notes: r.get(13)?,
        lyrics_raw: r.get(14)?,
        lyric_lines: r.get(15)?,
        lyric_style: r.get(16)?,
        mixer: r.get(17)?,
        thumb: r.get::<_, Option<Vec<u8>>>(18)?.unwrap_or_default(),
        kind: ItemKind::parse(&r.get::<_, String>(19)?),
        source: r.get::<_, Option<String>>(20)?.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

impl Store {
    /// Settings are read every frame by the UI, so they're cached in memory.
    pub fn setting(&self, key: &str) -> Option<String> {
        if let Some(v) = self.cache.lock().unwrap().settings.get(key) {
            return v.clone();
        }
        let conn = self.conn.lock().unwrap();
        let v: Option<String> =
            conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten();
        drop(conn);
        self.cache.lock().unwrap().settings.insert(key.to_owned(), v.clone());
        v
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        );
        drop(conn);
        self.cache.lock().unwrap().settings.insert(key.to_owned(), Some(value.to_owned()));
    }

    pub fn create_project(&self, name: &str, folder: &str, default_model: &str) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        let t = now();
        conn.execute(
            "INSERT INTO projects (name, folder, default_model, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![name, folder, default_model, t],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_projects(&self) -> Vec<ProjectRow> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT p.id, p.name, p.folder, p.default_model, p.created_at, p.updated_at,
                    COUNT(s.id), COALESCE(SUM(s.duration), 0)
             FROM projects p LEFT JOIN songs s ON s.project_id = p.id
             GROUP BY p.id ORDER BY p.updated_at DESC",
        ) else {
            return Vec::new();
        };
        stmt.query_map([], |r| {
            Ok(ProjectRow {
                id: r.get(0)?,
                name: r.get(1)?,
                folder: r.get(2)?,
                default_model: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
                song_count: r.get(6)?,
                total_duration: r.get(7)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn project(&self, id: i64) -> Option<ProjectRow> {
        self.list_projects().into_iter().find(|p| p.id == id)
    }

    fn touch_project(conn: &Connection, id: i64) {
        let _ = conn.execute("UPDATE projects SET updated_at = ?2 WHERE id = ?1", params![id, now()]);
    }

    pub fn update_project(&self, id: i64, name: &str, folder: &str, default_model: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE projects SET name = ?2, folder = ?3, default_model = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, name, folder, default_model, now()],
        );
    }

    /// Point a project at a new folder and rewrite its songs' relative paths.
    pub fn rename_project_folder(&self, id: i64, old: &str, new: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("UPDATE projects SET folder = ?2 WHERE id = ?1", params![id, new]);
        let _ = conn.execute(
            "UPDATE songs SET path = ?3 || substr(path, length(?2) + 1)
             WHERE project_id = ?1 AND substr(path, 1, length(?2) + 1) = ?2 || '/'",
            params![id, old, new],
        );
    }

    pub fn delete_project(&self, id: i64) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM projects WHERE id = ?1", [id]);
    }

    pub fn songs(&self, project_id: i64) -> Vec<SongRow> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) =
            conn.prepare(&format!("SELECT {SONG_COLUMNS} FROM songs WHERE project_id = ?1 ORDER BY position"))
        else {
            return Vec::new();
        };
        stmt.query_map([project_id], song_from_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Every song in every project (without thumbnails), most recently
    /// edited project first. Used for search and the song library.
    pub fn all_songs(&self) -> Vec<SongRow> {
        let conn = self.conn.lock().unwrap();
        let cols = SONG_COLUMNS.replace("thumb", "NULL").replace("id, project_id", "s.id, s.project_id").replace("position", "s.position");
        let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT {cols} FROM songs s JOIN projects p ON p.id = s.project_id ORDER BY p.updated_at DESC, s.position"
        )) else {
            return Vec::new();
        };
        stmt.query_map([], song_from_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn song(&self, id: i64) -> Option<SongRow> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(&format!("SELECT {SONG_COLUMNS} FROM songs WHERE id = ?1"), [id], song_from_row).optional().ok().flatten()
    }

    /// Append a song (not yet imported) and return its id.
    pub fn add_song(&self, project_id: i64, path: &str, original_path: &str, title: &str) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap();
        let pos: u32 = conn
            .query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM songs WHERE project_id = ?1", [project_id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        conn.execute(
            "INSERT INTO songs (project_id, position, path, original_path, title, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![project_id, pos, path, original_path, title, now()],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        Self::touch_project(&conn, project_id);
        Ok(id)
    }

    /// Append a YouTube video or slide deck (not yet downloaded / converted).
    pub fn add_item(&self, project_id: i64, kind: ItemKind, path: &str, original: &str, title: &str, source: &ItemSource) -> Result<i64, String> {
        let id = self.add_song(project_id, path, original, title)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE songs SET kind = ?2, source = ?3 WHERE id = ?1",
            params![id, kind.as_str(), serde_json::to_string(source).unwrap_or_default()],
        )
        .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn set_item_source(&self, id: i64, source: &ItemSource) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("UPDATE songs SET source = ?2 WHERE id = ?1", params![id, serde_json::to_string(source).unwrap_or_default()]);
    }

    /// Where the item's file ended up, plus the title and artist found while
    /// downloading (empty strings keep what's there).
    pub fn set_item_file(&self, id: i64, path: &str, title: &str, artist: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE songs SET path = ?2, title = CASE WHEN ?3 = '' THEN title ELSE ?3 END,
                artist = CASE WHEN ?4 = '' THEN artist ELSE ?4 END WHERE id = ?1",
            params![id, path, title, artist],
        );
    }

    pub fn finish_import(&self, id: i64, duration: f64, sample_rate: u32, thumb: &[u8]) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE songs SET imported = 1, duration = ?2, sample_rate = ?3, thumb = ?4 WHERE id = ?1",
            params![id, duration, sample_rate, thumb],
        );
    }

    pub fn update_song_meta(&self, id: i64, m: &SongMeta) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE songs SET title = ?2, artist = ?3, song_key = ?4, bpm = ?5, notes = ?6, model_key = ?7 WHERE id = ?1",
            params![id, m.title, m.artist, m.song_key, m.bpm, m.notes, m.model_key],
        );
    }

    pub fn save_song_state(&self, id: i64, lyrics_raw: &str, lyric_lines: &str, lyric_style: &str, mixer: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE songs SET lyrics_raw = ?2, lyric_lines = ?3, lyric_style = ?4, mixer = ?5 WHERE id = ?1",
            params![id, lyrics_raw, lyric_lines, lyric_style, mixer],
        );
    }

    pub fn remove_song(&self, id: i64) {
        let mut conn = self.conn.lock().unwrap();
        let Ok(tx) = conn.transaction() else { return };
        let project: Option<i64> = tx.query_row("SELECT project_id FROM songs WHERE id = ?1", [id], |r| r.get(0)).ok();
        let _ = tx.execute("DELETE FROM songs WHERE id = ?1", [id]);
        if let Some(p) = project {
            renumber(&tx, p);
            Self::touch_project(&tx, p);
        }
        let _ = tx.commit();
    }

    /// Move a song to `new_pos` (0-based) within its project.
    pub fn move_song(&self, id: i64, new_pos: u32) {
        let mut conn = self.conn.lock().unwrap();
        let Ok(tx) = conn.transaction() else { return };
        let Ok(project) = tx.query_row("SELECT project_id FROM songs WHERE id = ?1", [id], |r| r.get::<_, i64>(0)) else {
            return;
        };
        let mut ids: Vec<i64> = {
            let Ok(mut stmt) = tx.prepare("SELECT id FROM songs WHERE project_id = ?1 ORDER BY position") else { return };
            stmt.query_map([project], |r| r.get(0)).map(|r| r.flatten().collect()).unwrap_or_default()
        };
        ids.retain(|&s| s != id);
        ids.insert((new_pos as usize).min(ids.len()), id);
        for (i, sid) in ids.iter().enumerate() {
            let _ = tx.execute("UPDATE songs SET position = ?2 WHERE id = ?1", params![sid, i as u32]);
        }
        Self::touch_project(&tx, project);
        let _ = tx.commit();
    }

    /// Copy everything but the identity and location columns from `s` (project import).
    pub fn restore_song_fields(&self, id: i64, s: &SongRow) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE songs SET imported = ?2, title = ?3, artist = ?4, duration = ?5, sample_rate = ?6, model_key = ?7,
                song_key = ?8, bpm = ?9, notes = ?10, lyrics_raw = ?11, lyric_lines = ?12, lyric_style = ?13,
                mixer = ?14, thumb = ?15 WHERE id = ?1",
            params![
                id, s.imported, s.title, s.artist, s.duration, s.sample_rate, s.model_key, s.song_key, s.bpm, s.notes,
                s.lyrics_raw, s.lyric_lines, s.lyric_style, s.mixer, s.thumb
            ],
        );
    }
}

fn renumber(conn: &Connection, project_id: i64) {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM songs WHERE project_id = ?1 ORDER BY position")
        .and_then(|mut s| s.query_map([project_id], |r| r.get(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (i, id) in ids.iter().enumerate() {
        let _ = conn.execute("UPDATE songs SET position = ?2 WHERE id = ?1", params![id, i as u32]);
    }
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE models (
                key           TEXT PRIMARY KEY,   -- engine:id
                downloaded    INTEGER NOT NULL DEFAULT 0,
                bytes_on_disk INTEGER NOT NULL DEFAULT 0,
                bytes_total   INTEGER,
                downloaded_at INTEGER,
                last_used_at  INTEGER,
                use_count     INTEGER NOT NULL DEFAULT 0,
                error         TEXT
            );
            CREATE TABLE separations (
                id          INTEGER PRIMARY KEY,
                source_path TEXT NOT NULL,
                model_key   TEXT NOT NULL,
                out_dir     TEXT NOT NULL UNIQUE,
                stems       INTEGER NOT NULL,
                seconds     REAL NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX separations_by_time ON separations(created_at);
            PRAGMA user_version = 1;",
        )?;
    }
    if version < 2 {
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE projects (
                id            INTEGER PRIMARY KEY,
                name          TEXT NOT NULL,
                folder        TEXT NOT NULL,      -- relative to the workspace
                default_model TEXT NOT NULL,
                created_at    INTEGER NOT NULL,
                updated_at    INTEGER NOT NULL
            );
            CREATE TABLE songs (
                id            INTEGER PRIMARY KEY,
                project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                position      INTEGER NOT NULL,
                path          TEXT NOT NULL,      -- relative to the workspace
                original_path TEXT NOT NULL,
                imported      INTEGER NOT NULL DEFAULT 0,  -- copied + analysed
                title         TEXT NOT NULL,
                artist        TEXT NOT NULL DEFAULT '',
                duration      REAL NOT NULL DEFAULT 0,
                sample_rate   INTEGER NOT NULL DEFAULT 0,
                model_key     TEXT,               -- overrides projects.default_model
                song_key      TEXT NOT NULL DEFAULT '',
                bpm           REAL,
                notes         TEXT NOT NULL DEFAULT '',
                lyrics_raw    TEXT NOT NULL DEFAULT '',
                lyric_lines   TEXT,               -- json
                lyric_style   TEXT,               -- json
                mixer         TEXT,               -- json
                thumb         BLOB,
                added_at      INTEGER NOT NULL
            );
            CREATE INDEX songs_by_project ON songs(project_id, position);
            PRAGMA user_version = 2;",
        )?;
    }
    if version < 3 {
        // Setlist items that aren't splittable songs: YouTube videos, slide decks.
        conn.execute_batch(
            "ALTER TABLE songs ADD COLUMN kind TEXT NOT NULL DEFAULT 'song';
            ALTER TABLE songs ADD COLUMN source TEXT;   -- json (model::ItemSource)
            PRAGMA user_version = 3;",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_status_upsert_keeps_known_total() {
        let s = Store::in_memory().unwrap();
        s.set_model_status("uvr:a.onnx", false, 0, Some(1000));
        s.set_model_status("uvr:a.onnx", true, 1000, None);
        let m = &s.models()[0];
        assert!(m.downloaded);
        assert_eq!(m.bytes_total, Some(1000));
        assert!(m.downloaded_at.is_some());
        s.set_model_status("uvr:a.onnx", false, 0, None);
        assert_eq!(s.models()[0].downloaded_at, None);
    }

    #[test]
    fn projects_and_song_order() {
        let s = Store::in_memory().unwrap();
        let p = s.create_project("Sunday", "Sunday", "demucs:htdemucs").unwrap();
        let ids: Vec<i64> = ["a", "b", "c", "d"]
            .iter()
            .map(|t| s.add_song(p, &format!("Sunday/songs/{t}.mp3"), &format!("/x/{t}.mp3"), t).unwrap())
            .collect();
        let order = |s: &Store| s.songs(p).iter().map(|r| r.title.clone()).collect::<Vec<_>>().join("");
        assert_eq!(order(&s), "abcd");
        s.move_song(ids[3], 0);
        assert_eq!(order(&s), "dabc");
        s.move_song(ids[3], 99);
        assert_eq!(order(&s), "abcd");
        s.move_song(ids[0], 2);
        assert_eq!(order(&s), "bcad");
        s.remove_song(ids[2]);
        assert_eq!(order(&s), "bad");
        assert_eq!(s.songs(p).iter().map(|r| r.position).collect::<Vec<_>>(), vec![0, 1, 2]);

        let row = s.project(p).unwrap();
        assert_eq!(row.song_count, 3);
        let song = s.songs(p)[0].clone();
        assert_eq!(song.effective_model(&row), "demucs:htdemucs");
        s.update_song_meta(song.id, &SongMeta { title: "B".into(), model_key: Some("uvr:x.onnx".into()), ..Default::default() });
        assert_eq!(s.song(song.id).unwrap().effective_model(&row), "uvr:x.onnx");

        s.delete_project(p);
        assert!(s.songs(p).is_empty(), "songs cascade with their project");
    }

    #[test]
    fn migrates_v1_database_keeping_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE models (key TEXT PRIMARY KEY, downloaded INTEGER NOT NULL DEFAULT 0,
                bytes_on_disk INTEGER NOT NULL DEFAULT 0, bytes_total INTEGER, downloaded_at INTEGER,
                last_used_at INTEGER, use_count INTEGER NOT NULL DEFAULT 0, error TEXT);
             CREATE TABLE separations (id INTEGER PRIMARY KEY, source_path TEXT NOT NULL, model_key TEXT NOT NULL,
                out_dir TEXT NOT NULL UNIQUE, stems INTEGER NOT NULL, seconds REAL NOT NULL, created_at INTEGER NOT NULL);
             CREATE INDEX separations_by_time ON separations(created_at);
             INSERT INTO models (key, downloaded, bytes_on_disk) VALUES ('demucs:htdemucs', 1, 84);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        let s = Store::with_connection(conn).unwrap();
        assert_eq!(s.models()[0].key, "demucs:htdemucs");
        assert!(s.create_project("P", "P", "demucs:htdemucs").is_ok());
    }

    #[test]
    fn songs_from_v2_become_song_items_and_items_round_trip() {
        let s = Store::in_memory().unwrap();
        let p = s.create_project("P", "P", "demucs:htdemucs").unwrap();
        let a = s.add_song(p, "P/songs/a.mp3", "/a.mp3", "A").unwrap();
        let src = ItemSource::YouTube { url: "https://youtu.be/dQw4w9WgXcQ".into(), video_id: "dQw4w9WgXcQ".into(), video: None, show_video: true };
        let b = s.add_item(p, ItemKind::YouTube, "P/songs/yt.m4a", "https://youtu.be/dQw4w9WgXcQ", "Video", &src).unwrap();
        assert_eq!(s.song(a).unwrap().kind, ItemKind::Song);
        let row = s.song(b).unwrap();
        assert_eq!((row.kind, row.source), (ItemKind::YouTube, Some(src)));
        s.set_item_file(b, "P/songs/yt.webm", "Real Title", "");
        let row = s.song(b).unwrap();
        assert_eq!((row.path.as_str(), row.title.as_str(), row.position), ("P/songs/yt.webm", "Real Title", 1));
    }

    #[test]
    fn settings_round_trip() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.setting("workspace_dir"), None);
        s.set_setting("workspace_dir", "/a");
        s.set_setting("workspace_dir", "/b");
        assert_eq!(s.setting("workspace_dir").as_deref(), Some("/b"));
    }

    #[test]
    fn usage_and_history() {
        let s = Store::in_memory().unwrap();
        s.touch_model("demucs:htdemucs");
        s.touch_model("demucs:htdemucs");
        assert_eq!(s.models()[0].use_count, 2);
        for (dir, secs) in [("/a", 30.0), ("/b", 50.0)] {
            s.record_separation(&SeparationRecord {
                source_path: "/song.mp3".into(),
                model_key: "demucs:htdemucs".into(),
                out_dir: dir.into(),
                stems: 4,
                seconds: secs,
                created_at: now(),
            });
        }
        assert_eq!(s.recent_separations(10).len(), 2);
        assert_eq!(s.separation_seconds("demucs:htdemucs"), Some(40.0));
    }
}
