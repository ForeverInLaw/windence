use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result};
#[cfg(not(target_os = "windows"))]
use directories::ProjectDirs;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::model::{
    ListSort, ListSortColumn, ListSortDirection, ListedTrack, Playlist, QueueItem, Track,
};
use crate::shuffle::{ContextKind, Origin, ShuffleMode, ShuffleState};

const DATABASE_FILE: &str = "cadence.sqlite3";

/// Highest schema version this build knows how to migrate to.
const SCHEMA_VERSION: u32 = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// The volume a listener who has never touched the slider gets.
pub const DEFAULT_VOLUME: f32 = 0.72;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppPreferences {
    pub sidebar_collapsed: bool,
    pub theme: ThemePreference,
    pub autoplay: bool,
    /// Playback volume, 0.0 to 1.0.
    pub volume: f32,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            sidebar_collapsed: false,
            theme: ThemePreference::default(),
            // Autoplay is on unless the listener turned it off.
            autoplay: true,
            volume: DEFAULT_VOLUME,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackSnapshot {
    pub tracks: Vec<Track>,
    pub index: usize,
    pub position_ms: u32,
    /// The queue's shuffle bookkeeping: mode, base order, per-track origins.
    pub shuffle: ShuffleState,
    /// A track-radio context is already a recommendation stream; its toggle
    /// stays a no-op across a restart.
    pub radio: bool,
    /// Where the context was started from, so the Smart Shuffle gate
    /// survives a restart.
    pub context_kind: ContextKind,
}

/// What a cheap library reload compares before committing to a full walk:
/// the first pages and Spotify's totals for both collections. The totals
/// catch removals past the first page; the heads catch additions and
/// renames; the snapshot ids catch edits inside a playlist, which move
/// neither list.
///
/// Persisted next to the caches it vouches for, so a restart seeds its
/// first probe from disk instead of refetching everything.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryFingerprint {
    pub liked_head: Vec<String>,
    pub liked_total: u32,
    pub playlist_head: Vec<(String, String, String)>,
    pub playlist_total: u32,
}

impl LibraryFingerprint {
    pub(crate) fn new(
        liked: &(Vec<Track>, u32),
        playlists: &(Vec<(Playlist, String)>, u32),
    ) -> Self {
        Self {
            liked_head: liked
                .0
                .iter()
                .map(|track| track.source_id.clone())
                .collect(),
            liked_total: liked.1,
            playlist_head: playlists
                .0
                .iter()
                .map(|(playlist, snapshot)| {
                    (
                        playlist.source_id.clone(),
                        playlist.name.clone(),
                        snapshot.clone(),
                    )
                })
                .collect(),
            playlist_total: playlists.1,
        }
    }
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let data_dir = Self::data_dir()?;
        #[cfg(target_os = "windows")]
        relocate_legacy_windows_database(&data_dir);
        std::fs::create_dir_all(&data_dir)
            .context("could not create the Cadence data directory")?;
        Self::open(data_dir.join(DATABASE_FILE))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path).context("could not open the Cadence database")?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    /// Per-user application data directory. On Windows that is Roaming
    /// AppData plus the app name; `directories` instead mirrors XDG there
    /// (`...\AppData\Roaming\Cadence\Cadence\data`), so the path is resolved
    /// by hand. Other platforms keep the upstream mapping.
    #[cfg(target_os = "windows")]
    fn data_dir() -> Result<PathBuf> {
        let base = directories::BaseDirs::new()
            .context("could not resolve the per-user data directory")?;
        Ok(base.data_dir().join("Cadence"))
    }

    #[cfg(not(target_os = "windows"))]
    fn data_dir() -> Result<PathBuf> {
        let project = ProjectDirs::from("com", "Cadence", "Cadence")
            .context("could not resolve the Cadence data directory")?;
        Ok(project.data_dir().to_owned())
    }

    fn migrate(&self) -> Result<()> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;",
        )?;
        let version: u32 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            anyhow::bail!("database schema version {version} is newer than this Cadence build");
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        if version == 0 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;

             CREATE TABLE IF NOT EXISTS favorites (
                 provider TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 track_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 PRIMARY KEY (provider, source_id)
             );

             CREATE TABLE IF NOT EXISTS pinned_playlists (
                 provider TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 playlist_json TEXT NOT NULL,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 PRIMARY KEY (provider, source_id)
             );

             CREATE TABLE IF NOT EXISTS history (
                 id INTEGER PRIMARY KEY,
                 provider TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 track_json TEXT NOT NULL,
                 played_at INTEGER NOT NULL DEFAULT (unixepoch())
             );

             CREATE TABLE IF NOT EXISTS queue (
                 position INTEGER PRIMARY KEY,
                 item_id INTEGER NOT NULL,
                 track_json TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS liked_tracks_cache (
                 position INTEGER PRIMARY KEY,
                 track_json TEXT NOT NULL,
                 refreshed_at INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS playback_state (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 tracks_json TEXT NOT NULL,
                 current_index INTEGER NOT NULL,
                 position_ms INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             );

             CREATE TABLE IF NOT EXISTS preferences (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );

             PRAGMA user_version = 4;
             COMMIT;",
            )?;
        } else if version == 1 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE liked_tracks_cache (
                     position INTEGER PRIMARY KEY,
                     track_json TEXT NOT NULL,
                      refreshed_at INTEGER NOT NULL
                  );
                   CREATE TABLE playback_state (
                      id INTEGER PRIMARY KEY CHECK (id = 1),
                      tracks_json TEXT NOT NULL,
                      current_index INTEGER NOT NULL,
                      position_ms INTEGER NOT NULL,
                      updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                   );
                   CREATE TABLE preferences (
                       key TEXT PRIMARY KEY,
                       value TEXT NOT NULL
                   );
                   PRAGMA user_version = 4;
                  COMMIT;",
            )?;
        } else if version == 2 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                  CREATE TABLE playback_state (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     tracks_json TEXT NOT NULL,
                     current_index INTEGER NOT NULL,
                     position_ms INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                  );
                  CREATE TABLE preferences (
                      key TEXT PRIMARY KEY,
                      value TEXT NOT NULL
                  );
                  PRAGMA user_version = 4;
                  COMMIT;",
            )?;
        } else if version == 3 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE preferences (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
        }
        if version < 5 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;

             CREATE TABLE IF NOT EXISTS library_playlists_cache (
                 position INTEGER PRIMARY KEY,
                 playlist_json TEXT NOT NULL
             );

             CREATE TABLE IF NOT EXISTS library_fingerprint (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 fingerprint_json TEXT NOT NULL
             );

             PRAGMA user_version = 5;
             COMMIT;",
            )?;
        }
        if version < 6 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE playback_state ADD COLUMN context_json TEXT NOT NULL DEFAULT '[]';
                 ALTER TABLE playback_state ADD COLUMN origins_json TEXT NOT NULL DEFAULT '[]';
                 ALTER TABLE playback_state ADD COLUMN shuffle_mode TEXT NOT NULL DEFAULT 'off';
                 ALTER TABLE playback_state ADD COLUMN radio INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 6;
                 COMMIT;",
            )?;
        }
        if version < 7 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE playback_state ADD COLUMN context_kind TEXT NOT NULL DEFAULT 'collection';
                 PRAGMA user_version = 7;
                 COMMIT;",
            )?;
        }
        if version < 8 {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE liked_tracks_cache ADD COLUMN added_at INTEGER;
                 CREATE TABLE IF NOT EXISTS list_sorts (
                     list_key TEXT PRIMARY KEY,
                     sort_column TEXT NOT NULL,
                     sort_direction TEXT NOT NULL
                 );
                 PRAGMA user_version = 8;
                 COMMIT;",
            )?;
        }
        Ok(())
    }

    pub fn preferences(&self) -> Result<AppPreferences> {
        let sidebar_collapsed = self
            .preference("sidebar_collapsed")?
            .is_some_and(|value| value == "true");
        let theme = match self.preference("theme")?.as_deref() {
            Some("light") => ThemePreference::Light,
            Some("dark") => ThemePreference::Dark,
            _ => ThemePreference::System,
        };
        let autoplay = self.preference("autoplay")?.as_deref() != Some("false");
        // A value that is missing, unparsable or out of range is not
        // trusted: the slider only ever spans 0.0 to 1.0.
        let volume = self
            .preference("volume")?
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|volume| volume.is_finite())
            .map_or(DEFAULT_VOLUME, |volume| volume.clamp(0., 1.));
        Ok(AppPreferences {
            sidebar_collapsed,
            theme,
            autoplay,
            volume,
        })
    }

    /// Remembers the volume across launches. Written when the listener
    /// settles on one, not on every step of a drag.
    pub fn set_volume(&mut self, volume: f32) -> Result<()> {
        self.set_preference("volume", &volume.clamp(0., 1.).to_string())
    }

    pub fn set_autoplay(&mut self, autoplay: bool) -> Result<()> {
        self.set_preference("autoplay", if autoplay { "true" } else { "false" })
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool) -> Result<()> {
        self.set_preference(
            "sidebar_collapsed",
            if collapsed { "true" } else { "false" },
        )
    }

    pub fn set_theme_preference(&mut self, theme: ThemePreference) -> Result<()> {
        let value = match theme {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        };
        self.set_preference("theme", value)
    }

    /// The DJ station's cursor: the internal-protocol url that hands back
    /// the stretch after the one last queued. Kept across restarts because
    /// starting a session afresh always returns the same opening stretch,
    /// however far the station has actually moved.
    pub fn dj_cursor(&self) -> Result<Option<String>> {
        Ok(self
            .preference("dj_cursor")?
            .filter(|cursor| !cursor.is_empty()))
    }

    pub fn set_dj_cursor(&mut self, cursor: Option<&str>) -> Result<()> {
        self.set_preference("dj_cursor", cursor.unwrap_or_default())
    }

    pub fn spotify_client_id(&self) -> Result<Option<String>> {
        Ok(self
            .preference("spotify_client_id")?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()))
    }

    pub fn set_spotify_client_id(&mut self, client_id: &str) -> Result<()> {
        self.set_preference("spotify_client_id", client_id.trim())
    }

    pub fn configure_spotify(&mut self, client_id: &str) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for (key, value) in [
            ("spotify_client_id", client_id.trim()),
            ("spotify_oauth_credentials_invalidated", "true"),
            ("spotify_playback_credentials_invalidated", "true"),
        ] {
            transaction.execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_spotify_client_id(&mut self) -> Result<()> {
        self.connection.execute(
            "DELETE FROM preferences WHERE key = ?1",
            params!["spotify_client_id"],
        )?;
        Ok(())
    }

    pub fn reset_spotify_configuration(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM preferences WHERE key = ?1",
            params!["spotify_client_id"],
        )?;
        for key in [
            "spotify_oauth_credentials_invalidated",
            "spotify_playback_credentials_invalidated",
        ] {
            transaction.execute(
                "INSERT INTO preferences (key, value) VALUES (?1, 'true')
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn spotify_oauth_credentials_invalidated(&self) -> Result<bool> {
        Ok(self
            .preference("spotify_oauth_credentials_invalidated")?
            .is_some_and(|value| value == "true"))
    }

    pub fn set_spotify_oauth_credentials_invalidated(&mut self, invalidated: bool) -> Result<()> {
        self.set_preference(
            "spotify_oauth_credentials_invalidated",
            if invalidated { "true" } else { "false" },
        )
    }

    pub fn spotify_playback_credentials_invalidated(&self) -> Result<bool> {
        Ok(self
            .preference("spotify_playback_credentials_invalidated")?
            .is_some_and(|value| value == "true"))
    }

    pub fn set_spotify_playback_credentials_invalidated(
        &mut self,
        invalidated: bool,
    ) -> Result<()> {
        self.set_preference(
            "spotify_playback_credentials_invalidated",
            if invalidated { "true" } else { "false" },
        )
    }

    fn preference(&self, key: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM preferences WHERE key = ?1")?;
        let mut rows = statement.query(params![key])?;
        Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
    }

    fn set_preference(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO preferences (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn set_favorite(&mut self, track: &Track, favorite: bool) -> Result<()> {
        if favorite {
            self.connection.execute(
                "INSERT INTO favorites (provider, source_id, track_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (provider, source_id) DO UPDATE SET track_json = excluded.track_json",
                params![
                    track.provider.as_str(),
                    track.source_id,
                    serde_json::to_string(track)?
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM favorites WHERE provider = ?1 AND source_id = ?2",
                params![track.provider.as_str(), track.source_id],
            )?;
        }
        Ok(())
    }

    pub fn favorites(&self) -> Result<Vec<Track>> {
        let mut statement = self
            .connection
            .prepare("SELECT track_json FROM favorites ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn set_playlist_pinned(&mut self, playlist: &Playlist, pinned: bool) -> Result<()> {
        if pinned {
            self.connection.execute(
                "INSERT INTO pinned_playlists (provider, source_id, playlist_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (provider, source_id) DO UPDATE SET playlist_json = excluded.playlist_json",
                params![
                    playlist.provider.as_str(),
                    playlist.source_id,
                    serde_json::to_string(playlist)?
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM pinned_playlists WHERE provider = ?1 AND source_id = ?2",
                params![playlist.provider.as_str(), playlist.source_id],
            )?;
        }
        Ok(())
    }

    pub fn pinned_playlists(&self) -> Result<Vec<Playlist>> {
        let mut statement = self
            .connection
            .prepare("SELECT playlist_json FROM pinned_playlists ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn add_history(&mut self, track: &Track) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO history (provider, source_id, track_json) VALUES (?1, ?2, ?3)",
            params![
                track.provider.as_str(),
                track.source_id,
                serde_json::to_string(track)?
            ],
        )?;
        transaction.execute(
            "DELETE FROM history
             WHERE provider = ?1 AND source_id = ?2 AND id <> last_insert_rowid()",
            params![track.provider.as_str(), track.source_id],
        )?;
        transaction.execute(
            "DELETE FROM history
             WHERE id NOT IN (SELECT id FROM history ORDER BY played_at DESC, id DESC LIMIT 500)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn recent_tracks(&self, limit: usize) -> Result<Vec<Track>> {
        let mut statement = self.connection.prepare(
            "SELECT track_json
             FROM (
                 SELECT track_json, played_at, id,
                        ROW_NUMBER() OVER (
                            PARTITION BY provider, source_id
                            ORDER BY played_at DESC, id DESC
                        ) AS recency
                 FROM history
             )
             WHERE recency = 1
             ORDER BY played_at DESC, id DESC
             LIMIT ?1",
        )?;
        let limit = i64::try_from(limit).context("history limit exceeds SQLite range")?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn replace_queue(&mut self, queue: &[QueueItem]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM queue", [])?;
        for (position, item) in queue.iter().enumerate() {
            let position =
                i64::try_from(position).context("queue position exceeds SQLite range")?;
            transaction.execute(
                "INSERT INTO queue (position, item_id, track_json) VALUES (?1, ?2, ?3)",
                params![position, item.id, serde_json::to_string(&item.track)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn queue(&self) -> Result<Vec<QueueItem>> {
        let mut statement = self
            .connection
            .prepare("SELECT item_id, track_json FROM queue ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (id, json) = row?;
            Ok(QueueItem {
                id,
                track: serde_json::from_str(&json)?,
            })
        })
        .collect()
    }

    pub fn set_playback_state(
        &self,
        tracks: &[Track],
        index: usize,
        position_ms: u32,
        shuffle: &ShuffleState,
        radio: bool,
        context_kind: ContextKind,
    ) -> Result<()> {
        anyhow::ensure!(
            tracks.get(index).is_some(),
            "playback index is out of bounds"
        );
        let index = i64::try_from(index).context("playback index exceeds SQLite range")?;
        self.connection.execute(
            "INSERT INTO playback_state
                 (id, tracks_json, current_index, position_ms, context_json, origins_json,
                  shuffle_mode, radio, context_kind, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, unixepoch())
             ON CONFLICT (id) DO UPDATE SET
                 tracks_json = excluded.tracks_json,
                 current_index = excluded.current_index,
                 position_ms = excluded.position_ms,
                 context_json = excluded.context_json,
                 origins_json = excluded.origins_json,
                 shuffle_mode = excluded.shuffle_mode,
                 radio = excluded.radio,
                 context_kind = excluded.context_kind,
                 updated_at = excluded.updated_at",
            params![
                serde_json::to_string(tracks)?,
                index,
                i64::from(position_ms),
                serde_json::to_string(&shuffle.context)?,
                serde_json::to_string(&shuffle.origins)?,
                shuffle_mode_as_str(shuffle.mode),
                i64::from(radio),
                context_kind_as_str(context_kind),
            ],
        )?;
        Ok(())
    }

    pub fn update_playback_position(&self, position_ms: u32) -> Result<()> {
        self.connection.execute(
            "UPDATE playback_state SET position_ms = ?1, updated_at = unixepoch() WHERE id = 1",
            [i64::from(position_ms)],
        )?;
        Ok(())
    }

    pub fn playback_state(&self) -> Result<Option<PlaybackSnapshot>> {
        let state = self.connection.query_row(
            "SELECT tracks_json, current_index, position_ms, context_json, origins_json,
                    shuffle_mode, radio, context_kind
             FROM playback_state WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        );
        let (tracks_json, index, position_ms, context_json, origins_json, mode, radio, kind) =
            match state {
                Ok(state) => state,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        let tracks: Vec<Track> = serde_json::from_str(&tracks_json)?;
        let index = usize::try_from(index).context("stored playback index is invalid")?;
        anyhow::ensure!(
            tracks.get(index).is_some(),
            "stored playback index is out of bounds"
        );
        // Anything malformed in the shuffle columns degrades to an unshuffled
        // queue instead of failing the whole restore.
        let context: Vec<Track> = serde_json::from_str(&context_json).unwrap_or_default();
        let origins: Vec<Origin> = serde_json::from_str(&origins_json).unwrap_or_default();
        Ok(Some(PlaybackSnapshot {
            shuffle: ShuffleState::restored(
                tracks.len(),
                parse_shuffle_mode(&mode),
                context,
                origins,
            ),
            tracks,
            index,
            position_ms: u32::try_from(position_ms)
                .context("stored playback position is invalid")?,
            radio: radio != 0,
            context_kind: parse_context_kind(&kind),
        }))
    }

    pub fn clear_playback_state(&self) -> Result<()> {
        self.connection.execute("DELETE FROM playback_state", [])?;
        Ok(())
    }

    pub fn cached_playlists(&self) -> Result<Vec<Playlist>> {
        let mut statement = self
            .connection
            .prepare("SELECT playlist_json FROM library_playlists_cache ORDER BY position")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn saved_library_fingerprint(&self) -> Result<Option<LibraryFingerprint>> {
        let json = match self.connection.query_row(
            "SELECT fingerprint_json FROM library_fingerprint WHERE id = 1",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(json) => json,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(serde_json::from_str(&json)?))
    }

    /// Replaces the whole library cache — liked tracks, playlists, and the
    /// fingerprint vouching for both — in one transaction. The fingerprint
    /// must never be committed over missing or partial contents: a later
    /// probe would answer Unchanged and serve a hole as the whole library.
    pub fn replace_library_cache(
        &mut self,
        liked_tracks: &[ListedTrack],
        playlists: &[Playlist],
        fingerprint: &LibraryFingerprint,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM liked_tracks_cache", [])?;
        for (position, listed) in liked_tracks
            .iter()
            .filter(|listed| listed.track.is_displayable())
            .enumerate()
        {
            let position =
                i64::try_from(position).context("liked-track position exceeds SQLite range")?;
            transaction.execute(
                "INSERT INTO liked_tracks_cache (position, track_json, added_at, refreshed_at)
                 VALUES (?1, ?2, ?3, unixepoch())",
                params![
                    position,
                    serde_json::to_string(&listed.track)?,
                    listed.added_at.map(|date| date.timestamp())
                ],
            )?;
        }
        transaction.execute("DELETE FROM library_playlists_cache", [])?;
        for (position, playlist) in playlists.iter().enumerate() {
            let position =
                i64::try_from(position).context("playlist cache position exceeds SQLite range")?;
            transaction.execute(
                "INSERT INTO library_playlists_cache (position, playlist_json)
                 VALUES (?1, ?2)",
                params![position, serde_json::to_string(playlist)?],
            )?;
        }
        transaction.execute(
            "INSERT INTO library_fingerprint (id, fingerprint_json) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET fingerprint_json = excluded.fingerprint_json",
            params![serde_json::to_string(fingerprint)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Drops the cached catalog: liked tracks, playlists, and the
    /// fingerprint that answers for them.
    pub fn clear_library_cache(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM liked_tracks_cache", [])?;
        transaction.execute("DELETE FROM library_playlists_cache", [])?;
        transaction.execute("DELETE FROM library_fingerprint", [])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn liked_tracks(&self) -> Result<Vec<ListedTrack>> {
        let mut statement = self
            .connection
            .prepare("SELECT track_json, added_at FROM liked_tracks_cache ORDER BY position")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
        })?;
        rows.map(|row| {
            let (json, added_at) = row?;
            Ok(ListedTrack {
                track: serde_json::from_str(&json)?,
                added_at: added_at.and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0)),
            })
        })
        .filter(|listed: &Result<ListedTrack>| {
            listed
                .as_ref()
                .is_ok_and(|listed| listed.track.is_displayable())
        })
        .collect()
    }

    /// A list's persisted sort, if it has one. No row means the default
    /// order — the state a reset also persists as. Unreadable stored values
    /// degrade to the default rather than failing the read.
    pub fn list_sort(&self, list_key: &str) -> Result<Option<ListSort>> {
        let mut statement = self
            .connection
            .prepare("SELECT sort_column, sort_direction FROM list_sorts WHERE list_key = ?1")?;
        let mut rows = statement.query(params![list_key])?;
        match rows.next()? {
            None => Ok(None),
            Some(row) => {
                let column = ListSortColumn::parse(&row.get::<_, String>(0)?);
                let direction = ListSortDirection::parse(&row.get::<_, String>(1)?);
                Ok(column
                    .zip(direction)
                    .map(|(column, direction)| ListSort { column, direction }))
            }
        }
    }

    /// Persists a list's sort, or removes the row to persist the default.
    pub fn set_list_sort(&mut self, list_key: &str, sort: Option<ListSort>) -> Result<()> {
        match sort {
            Some(sort) => {
                self.connection.execute(
                    "INSERT INTO list_sorts (list_key, sort_column, sort_direction)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT (list_key) DO UPDATE SET
                         sort_column = excluded.sort_column,
                         sort_direction = excluded.sort_direction",
                    params![list_key, sort.column.as_str(), sort.direction.as_str()],
                )?;
            }
            None => {
                self.connection.execute(
                    "DELETE FROM list_sorts WHERE list_key = ?1",
                    params![list_key],
                )?;
            }
        }
        Ok(())
    }
}

fn shuffle_mode_as_str(mode: ShuffleMode) -> &'static str {
    match mode {
        ShuffleMode::Off => "off",
        ShuffleMode::Shuffle => "shuffle",
        ShuffleMode::Smart => "smart",
    }
}

fn parse_shuffle_mode(value: &str) -> ShuffleMode {
    match value {
        "shuffle" => ShuffleMode::Shuffle,
        "smart" => ShuffleMode::Smart,
        _ => ShuffleMode::Off,
    }
}

fn context_kind_as_str(kind: ContextKind) -> &'static str {
    match kind {
        ContextKind::Collection => "collection",
        ContextKind::Album => "album",
    }
}

fn parse_context_kind(value: &str) -> ContextKind {
    match value {
        "album" => ContextKind::Album,
        _ => ContextKind::Collection,
    }
}

/// How many "already offered" Smart Shuffle track ids to keep. Old entries
/// fall off the end FIFO-style, so a track can be suggested again after
/// roughly this many newer suggestions — months of listening, like
/// Spotify's eventual rotation.
const SMART_SHUFFLE_SEEN_CAP: usize = 500;

const SMART_SHUFFLE_SEEN_KEY: &str = "smart_shuffle_seen";

impl Store {
    /// Tracks Smart Shuffle has already offered, oldest first. Survives
    /// restarts and toggle cycles, so the same song is never suggested
    /// twice within the retained window.
    pub fn smart_shuffle_seen(&self) -> Result<Vec<String>> {
        match self.preference(SMART_SHUFFLE_SEEN_KEY)? {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(Vec::new()),
        }
    }

    /// Records newly offered track ids, dropping the oldest beyond the cap.
    pub fn add_smart_shuffle_seen(&mut self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut seen = self.smart_shuffle_seen()?;
        for id in ids {
            if !seen.contains(id) {
                seen.push(id.clone());
            }
        }
        let excess = seen.len().saturating_sub(SMART_SHUFFLE_SEEN_CAP);
        seen.drain(..excess);
        self.set_preference(SMART_SHUFFLE_SEEN_KEY, &serde_json::to_string(&seen)?)
    }

    /// Drops the offered-history: part of signing out.
    pub fn clear_smart_shuffle_seen(&mut self) -> Result<()> {
        self.connection.execute(
            "DELETE FROM preferences WHERE key = ?1",
            params![SMART_SHUFFLE_SEEN_KEY],
        )?;
        Ok(())
    }
}

/// One-time move of a database created under the pre-port Windows path
/// (`...\Cadence\Cadence\data`) to its platform-correct home. Best effort:
/// when nothing needs moving or a rename fails, the app simply starts with
/// whatever the target directory holds. The legacy tree is only deleted
/// when every file made it across, so a half-finished move can never take
/// un-relocated data with it.
#[cfg(target_os = "windows")]
fn relocate_legacy_windows_database(data_dir: &Path) {
    let database = data_dir.join(DATABASE_FILE);
    if database.exists() {
        return;
    }
    let legacy = data_dir.join("Cadence").join("data");
    let moved = std::fs::rename(legacy.join(DATABASE_FILE), &database);
    if moved.is_err() {
        return;
    }
    let mut complete = true;
    for suffix in ["-wal", "-shm"] {
        let source = legacy.join(format!("{DATABASE_FILE}{suffix}"));
        if source.exists() {
            complete &=
                std::fs::rename(source, data_dir.join(format!("{DATABASE_FILE}{suffix}"))).is_ok();
        }
    }
    if complete {
        // The emptied tree is this port's own leftover; dropping it keeps a
        // second "Cadence" directory from confusing later inspection.
        let _ = std::fs::remove_dir_all(data_dir.join("Cadence"));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppPreferences, DEFAULT_VOLUME, LibraryFingerprint, Store, ThemePreference,
        relocate_legacy_windows_database,
    };
    use crate::model::{Playlist, Provider, QueueItem, Track};
    use crate::shuffle::{ContextKind, Origin, ShuffleMode, ShuffleRng, ShuffleState};
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn track(id: &str) -> Track {
        Track {
            provider: Provider::Spotify,
            source_id: id.to_owned(),
            spotify_uri: Some(format!("spotify:track:{id}")),
            isrc: None,
            title: format!("Track {id}"),
            artist: "Artist".to_owned(),
            artists: Vec::new(),
            album: "Album".to_owned(),
            album_ref: None,
            duration_ms: 180_000,
            artwork_url: None,
        }
    }

    fn playlist(id: &str) -> Playlist {
        Playlist {
            provider: Provider::Spotify,
            source_id: id.to_owned(),
            name: format!("Playlist {id}"),
            owner: "Owner".to_owned(),
            track_count: 10,
            artwork_url: None,
        }
    }

    fn listed(track: Track, added_at: impl Into<Option<i64>>) -> super::ListedTrack {
        use chrono::TimeZone;
        super::ListedTrack {
            added_at: added_at
                .into()
                .map(|seconds| chrono::Utc.timestamp_opt(seconds, 0).unwrap()),
            track,
        }
    }

    fn fingerprint(liked_total: u32, playlist_total: u32) -> LibraryFingerprint {
        LibraryFingerprint {
            liked_head: vec!["one".to_owned()],
            liked_total,
            playlist_head: vec![("focus".to_owned(), "Focus".to_owned(), "snap".to_owned())],
            playlist_total,
        }
    }

    /// A unique scratch path; callers remove the database when done. The
    /// sidecars go too: a stale WAL left beside a fresh file resurrects
    /// deleted contents.
    fn temporary_database(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("cadence-{name}-{}.sqlite3", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    #[test]
    fn list_sorts_round_trip_and_reset_to_the_default() {
        use crate::model::{ListSort, ListSortColumn, ListSortDirection};

        let mut store = Store::in_memory().unwrap();
        assert_eq!(store.list_sort("liked").unwrap(), None);

        store
            .set_list_sort(
                "liked",
                Some(ListSort {
                    column: ListSortColumn::Title,
                    direction: ListSortDirection::Descending,
                }),
            )
            .unwrap();
        store
            .set_list_sort(
                "focus",
                Some(ListSort {
                    column: ListSortColumn::DateAdded,
                    direction: ListSortDirection::Ascending,
                }),
            )
            .unwrap();
        assert_eq!(
            store.list_sort("liked").unwrap(),
            Some(ListSort {
                column: ListSortColumn::Title,
                direction: ListSortDirection::Descending,
            })
        );
        // Lists keep independent sorts.
        assert_ne!(
            store.list_sort("liked").unwrap(),
            store.list_sort("focus").unwrap()
        );

        // Resetting one list leaves the other alone.
        store.set_list_sort("liked", None).unwrap();
        assert_eq!(store.list_sort("liked").unwrap(), None);
        assert!(
            store.list_sort("focus").unwrap().is_some(),
            "resetting liked must not clear focus"
        );
    }

    #[test]
    fn favorites_can_be_added_and_removed() {
        let mut store = Store::in_memory().unwrap();
        let track = track("one");

        store.set_favorite(&track, true).unwrap();
        assert_eq!(store.favorites().unwrap(), vec![track.clone()]);

        store.set_favorite(&track, false).unwrap();
        assert!(store.favorites().unwrap().is_empty());
    }

    #[test]
    fn preferences_round_trip() {
        let mut store = Store::in_memory().unwrap();
        assert_eq!(store.preferences().unwrap(), AppPreferences::default());

        store.set_sidebar_collapsed(true).unwrap();
        store.set_theme_preference(ThemePreference::Dark).unwrap();
        store.set_autoplay(false).unwrap();
        store.set_volume(0.4).unwrap();
        assert_eq!(
            store.preferences().unwrap(),
            AppPreferences {
                sidebar_collapsed: true,
                theme: ThemePreference::Dark,
                autoplay: false,
                volume: 0.4,
            }
        );
    }

    #[test]
    fn stored_volume_is_clamped_and_falls_back_to_the_default() {
        let mut store = Store::in_memory().unwrap();
        assert_eq!(store.preferences().unwrap().volume, DEFAULT_VOLUME);

        store.set_volume(1.8).unwrap();
        assert_eq!(store.preferences().unwrap().volume, 1.);

        store.set_preference("volume", "loud").unwrap();
        assert_eq!(store.preferences().unwrap().volume, DEFAULT_VOLUME);

        store.set_preference("volume", "-3").unwrap();
        assert_eq!(store.preferences().unwrap().volume, 0.);
    }

    #[test]
    fn invalid_preferences_use_safe_defaults() {
        let mut store = Store::in_memory().unwrap();
        store
            .set_preference("sidebar_collapsed", "sometimes")
            .unwrap();
        store.set_preference("theme", "midnight").unwrap();

        assert_eq!(store.preferences().unwrap(), AppPreferences::default());
    }

    #[test]
    fn spotify_client_id_round_trips_and_can_be_removed() {
        let mut store = Store::in_memory().unwrap();

        assert_eq!(store.spotify_client_id().unwrap(), None);
        store
            .set_spotify_client_id(" 0123456789abcdef0123456789abcdef ")
            .unwrap();
        assert_eq!(
            store.spotify_client_id().unwrap().as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );

        store.remove_spotify_client_id().unwrap();
        assert_eq!(store.spotify_client_id().unwrap(), None);
    }

    #[test]
    fn blank_spotify_client_id_is_not_configured() {
        let mut store = Store::in_memory().unwrap();

        store.set_spotify_client_id("   ").unwrap();

        assert_eq!(store.spotify_client_id().unwrap(), None);
    }

    #[test]
    fn spotify_configuration_and_credential_invalidation_round_trip() {
        let mut store = Store::in_memory().unwrap();

        store
            .configure_spotify("0123456789abcdef0123456789abcdef")
            .unwrap();
        assert_eq!(
            store.spotify_client_id().unwrap().as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert!(store.spotify_oauth_credentials_invalidated().unwrap());
        assert!(store.spotify_playback_credentials_invalidated().unwrap());

        store
            .set_spotify_oauth_credentials_invalidated(false)
            .unwrap();
        store
            .set_spotify_playback_credentials_invalidated(false)
            .unwrap();
        assert!(!store.spotify_oauth_credentials_invalidated().unwrap());
        assert!(!store.spotify_playback_credentials_invalidated().unwrap());

        store.reset_spotify_configuration().unwrap();
        assert_eq!(store.spotify_client_id().unwrap(), None);
        assert!(store.spotify_oauth_credentials_invalidated().unwrap());
        assert!(store.spotify_playback_credentials_invalidated().unwrap());
    }

    #[test]
    fn queue_order_and_duplicates_are_persisted() {
        let mut store = Store::in_memory().unwrap();
        let queue = vec![
            QueueItem {
                id: 10,
                track: track("one"),
            },
            QueueItem {
                id: 11,
                track: track("two"),
            },
            QueueItem {
                id: 12,
                track: track("one"),
            },
        ];

        store.replace_queue(&queue).unwrap();
        assert_eq!(store.queue().unwrap(), queue);
    }

    #[test]
    fn playback_state_round_trips_and_clears() {
        let store = Store::in_memory().unwrap();
        let tracks = vec![track("one"), track("two")];

        store
            .set_playback_state(
                &tracks,
                1,
                42_000,
                &ShuffleState::default(),
                false,
                ContextKind::Collection,
            )
            .unwrap();
        let state = store.playback_state().unwrap().unwrap();
        assert_eq!(state.tracks, tracks);
        assert_eq!(state.index, 1);
        assert_eq!(state.position_ms, 42_000);
        assert_eq!(state.shuffle.mode, ShuffleMode::Off);
        assert!(!state.radio);
        assert_eq!(state.context_kind, ContextKind::Collection);

        store.update_playback_position(45_000).unwrap();
        assert_eq!(store.playback_state().unwrap().unwrap().position_ms, 45_000);

        store.clear_playback_state().unwrap();
        assert!(store.playback_state().unwrap().is_none());
    }

    #[test]
    fn shuffle_bookkeeping_round_trips_with_the_playback_state() {
        let store = Store::in_memory().unwrap();
        let mut queue = vec![track("one"), track("two"), track("three"), track("four")];
        let mut shuffle = ShuffleState::for_context(&queue, ShuffleMode::Shuffle);
        shuffle.set_mode(
            ShuffleMode::Shuffle,
            &mut queue,
            1,
            &mut ShuffleRng::from_seed(7),
        );
        queue.insert(2, track("next"));
        shuffle.insert_anchor(2);

        store
            .set_playback_state(&queue, 1, 500, &shuffle, true, ContextKind::Collection)
            .unwrap();

        let state = store.playback_state().unwrap().unwrap();
        assert_eq!(state.shuffle.mode, ShuffleMode::Shuffle);
        assert_eq!(state.shuffle.context.len(), 4);
        assert_eq!(state.shuffle.origins, shuffle.origins);
        assert!(state.radio);
    }

    #[test]
    fn injected_origins_round_trip_through_the_playback_state() {
        let store = Store::in_memory().unwrap();
        let mut queue = vec![track("one"), track("smart"), track("two"), track("three")];
        let mut shuffle = ShuffleState::for_context(
            &[track("one"), track("two"), track("three")],
            ShuffleMode::Smart,
        );
        shuffle.origins.insert(1, Origin::Injected);

        store
            .set_playback_state(&queue, 0, 0, &shuffle, false, ContextKind::Collection)
            .unwrap();

        // Re-read through the raw column too, so the on-disk shape —
        // numbers, nulls, and the "injected" string — is pinned down.
        let stored: String = store
            .connection
            .query_row(
                "SELECT origins_json FROM playback_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, r#"[0,"injected",1,2]"#);

        queue.remove(1);
        shuffle.origins.remove(1);
        queue.insert(3, track("next"));
        shuffle.insert_anchor(3);
        store
            .set_playback_state(&queue, 0, 0, &shuffle, false, ContextKind::Collection)
            .unwrap();

        let state = store.playback_state().unwrap().unwrap();
        assert_eq!(state.shuffle.mode, ShuffleMode::Smart);
        assert_eq!(
            state.shuffle.origins,
            vec![
                Origin::Context { ordinal: 0 },
                Origin::Context { ordinal: 1 },
                Origin::Context { ordinal: 2 },
                Origin::Anchor,
            ]
        );
    }

    #[test]
    fn malformed_shuffle_columns_degrade_to_an_unshuffled_queue() {
        let store = Store::in_memory().unwrap();
        let tracks = vec![track("one"), track("two")];
        store
            .set_playback_state(
                &tracks,
                0,
                0,
                &ShuffleState::default(),
                false,
                ContextKind::Collection,
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE playback_state SET origins_json = '[0]', shuffle_mode = 'shuffle',
                        context_json = '[]'",
                [],
            )
            .unwrap();

        let state = store.playback_state().unwrap().unwrap();

        assert_eq!(state.shuffle.mode, ShuffleMode::Off);
        assert!(state.shuffle.context.is_empty());
    }

    #[test]
    fn liked_track_cache_replaces_the_previous_snapshot() {
        let mut store = Store::in_memory().unwrap();
        store
            .replace_library_cache(
                &[listed(track("one"), 100), listed(track("two"), 200)],
                &[],
                &fingerprint(2, 0),
            )
            .unwrap();
        assert_eq!(
            store
                .liked_tracks()
                .unwrap()
                .into_iter()
                .map(|listed| listed.track.source_id)
                .collect::<Vec<_>>(),
            ["one", "two"]
        );

        store
            .replace_library_cache(&[listed(track("three"), 300)], &[], &fingerprint(1, 0))
            .unwrap();
        assert_eq!(
            store.liked_tracks().unwrap(),
            vec![listed(track("three"), 300)]
        );
    }

    #[test]
    fn liked_track_cache_ignores_incomplete_tracks() {
        let mut store = Store::in_memory().unwrap();
        let mut incomplete = track("missing");
        incomplete.title.clear();
        incomplete.artist.clear();
        incomplete.duration_ms = 0;

        store
            .replace_library_cache(
                &[listed(track("one"), None), listed(incomplete.clone(), None)],
                &[],
                &fingerprint(1, 0),
            )
            .unwrap();
        assert_eq!(
            store.liked_tracks().unwrap(),
            vec![listed(track("one"), None)]
        );

        store
            .connection
            .execute(
                "INSERT INTO liked_tracks_cache (position, track_json, refreshed_at)
                 VALUES (1, ?1, unixepoch())",
                [serde_json::to_string(&incomplete).unwrap()],
            )
            .unwrap();
        assert_eq!(
            store.liked_tracks().unwrap(),
            vec![listed(track("one"), None)]
        );
    }

    #[test]
    fn library_cache_round_trips_with_its_fingerprint() {
        let mut store = Store::in_memory().unwrap();

        assert!(store.saved_library_fingerprint().unwrap().is_none());
        assert!(store.cached_playlists().unwrap().is_empty());

        let playlists = vec![playlist("focus"), playlist("gym")];
        let saved = fingerprint(3, 2);
        store
            .replace_library_cache(&[listed(track("one"), None)], &playlists, &saved)
            .unwrap();
        assert_eq!(store.cached_playlists().unwrap(), playlists);
        assert_eq!(store.saved_library_fingerprint().unwrap(), Some(saved));

        store.clear_library_cache().unwrap();
        assert_eq!(store.saved_library_fingerprint().unwrap(), None);
        assert!(store.cached_playlists().unwrap().is_empty());
        assert!(store.liked_tracks().unwrap().is_empty());
    }

    #[test]
    fn schema_version_four_databases_migrate_and_keep_their_contents() {
        let path = temporary_database("migration");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE favorites (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE pinned_playlists (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         playlist_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE history (
                         id INTEGER PRIMARY KEY,
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         played_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE queue (
                         position INTEGER PRIMARY KEY,
                         item_id INTEGER NOT NULL,
                         track_json TEXT NOT NULL
                     );
                     CREATE TABLE liked_tracks_cache (
                         position INTEGER PRIMARY KEY,
                         track_json TEXT NOT NULL,
                         refreshed_at INTEGER NOT NULL
                     );
                     CREATE TABLE playback_state (
                         id INTEGER PRIMARY KEY CHECK (id = 1),
                         tracks_json TEXT NOT NULL,
                         current_index INTEGER NOT NULL,
                         position_ms INTEGER NOT NULL,
                         updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE preferences (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL
                     );
                     INSERT INTO preferences (key, value) VALUES ('theme', 'dark');
                     PRAGMA user_version = 4;
                     COMMIT;",
                )
                .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            super::SCHEMA_VERSION
        );
        assert_eq!(store.preferences().unwrap().theme, ThemePreference::Dark);

        // The migrated database accepts the v5 tables and remembers them
        // across a reopen.
        let saved = fingerprint(1, 1);
        store
            .replace_library_cache(&[listed(track("one"), None)], &[playlist("focus")], &saved)
            .unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.saved_library_fingerprint().unwrap(), Some(saved));
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_version_five_databases_migrate_and_keep_their_playback_contents() {
        let path = temporary_database("migration-v5");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE favorites (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE pinned_playlists (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         playlist_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE history (
                         id INTEGER PRIMARY KEY,
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         played_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE queue (
                         position INTEGER PRIMARY KEY,
                         item_id INTEGER NOT NULL,
                         track_json TEXT NOT NULL
                     );
                     CREATE TABLE liked_tracks_cache (
                         position INTEGER PRIMARY KEY,
                         track_json TEXT NOT NULL,
                         refreshed_at INTEGER NOT NULL
                     );
                     CREATE TABLE playback_state (
                         id INTEGER PRIMARY KEY CHECK (id = 1),
                         tracks_json TEXT NOT NULL,
                         current_index INTEGER NOT NULL,
                         position_ms INTEGER NOT NULL,
                         updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE preferences (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL
                     );
                     CREATE TABLE library_playlists_cache (
                         position INTEGER PRIMARY KEY,
                         playlist_json TEXT NOT NULL
                     );
                     CREATE TABLE library_fingerprint (
                         id INTEGER PRIMARY KEY CHECK (id = 1),
                         fingerprint_json TEXT NOT NULL
                     );
                     PRAGMA user_version = 5;
                     COMMIT;",
                )
                .unwrap();
            let saved = serde_json::to_string(&vec![track("one"), track("two")]).unwrap();
            connection
                .execute(
                    "INSERT INTO playback_state (id, tracks_json, current_index, position_ms)
                     VALUES (1, ?1, 1, 30_000)",
                    [&saved],
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            super::SCHEMA_VERSION
        );

        // The migrated snapshot keeps its queue and reads as unshuffled: the
        // toggle-off bookkeeping simply starts empty.
        let state = store.playback_state().unwrap().unwrap();
        assert_eq!(
            state
                .tracks
                .iter()
                .map(|t| t.source_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(state.index, 1);
        assert_eq!(state.shuffle.mode, ShuffleMode::Off);

        // And it accepts shuffle-aware writes across a reopen.
        store
            .set_playback_state(
                &state.tracks,
                0,
                0,
                &ShuffleState::default(),
                false,
                ContextKind::Collection,
            )
            .unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.playback_state().unwrap().unwrap().index, 0);
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn schema_version_six_databases_gain_the_context_kind_column() {
        let path = temporary_database("migration-v6");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE favorites (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE pinned_playlists (
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         playlist_json TEXT NOT NULL,
                         created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                         PRIMARY KEY (provider, source_id)
                     );
                     CREATE TABLE history (
                         id INTEGER PRIMARY KEY,
                         provider TEXT NOT NULL,
                         source_id TEXT NOT NULL,
                         track_json TEXT NOT NULL,
                         played_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE queue (
                         position INTEGER PRIMARY KEY,
                         item_id INTEGER NOT NULL,
                         track_json TEXT NOT NULL
                     );
                     CREATE TABLE liked_tracks_cache (
                         position INTEGER PRIMARY KEY,
                         track_json TEXT NOT NULL,
                         refreshed_at INTEGER NOT NULL
                     );
                     CREATE TABLE playback_state (
                         id INTEGER PRIMARY KEY CHECK (id = 1),
                         tracks_json TEXT NOT NULL,
                         current_index INTEGER NOT NULL,
                         position_ms INTEGER NOT NULL,
                         context_json TEXT NOT NULL DEFAULT '[]',
                         origins_json TEXT NOT NULL DEFAULT '[]',
                         shuffle_mode TEXT NOT NULL DEFAULT 'off',
                         radio INTEGER NOT NULL DEFAULT 0,
                         updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                     );
                     CREATE TABLE preferences (
                         key TEXT PRIMARY KEY,
                         value TEXT NOT NULL
                     );
                     CREATE TABLE library_playlists_cache (
                         position INTEGER PRIMARY KEY,
                         playlist_json TEXT NOT NULL
                     );
                      CREATE TABLE library_fingerprint (
                          id INTEGER PRIMARY KEY CHECK (id = 1),
                          fingerprint_json TEXT NOT NULL
                      );
                     PRAGMA user_version = 6;
                     COMMIT;",
                )
                .unwrap();
            let saved = serde_json::to_string(&vec![track("one"), track("two")]).unwrap();
            connection
                .execute(
                    "INSERT INTO playback_state (id, tracks_json, current_index, position_ms,
                                                 context_json, origins_json, shuffle_mode, radio)
                         VALUES (1, ?1, 0, 0, ?1, '[0, 1]', 'shuffle', 0)",
                    [&saved],
                )
                .unwrap();
        }
        // The v6 snapshot survives: its shuffle mode reads back and the new
        // column defaults to a playlist-like kind, the permissive choice.
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            super::SCHEMA_VERSION
        );
        let state = store.playback_state().unwrap().unwrap();
        assert_eq!(
            state
                .tracks
                .iter()
                .map(|t| t.source_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(state.shuffle.mode, ShuffleMode::Shuffle);
        assert_eq!(state.context_kind, ContextKind::Collection);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn legacy_windows_database_relocates_to_the_data_directory() {
        let root = std::env::temp_dir().join(format!("cadence-relocate-{}", std::process::id()));
        let data_dir = root.join("Cadence");
        let legacy = data_dir.join("Cadence").join("data");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(super::DATABASE_FILE), b"database").unwrap();
        std::fs::write(legacy.join(format!("{}-wal", super::DATABASE_FILE)), b"wal").unwrap();

        relocate_legacy_windows_database(&data_dir);

        assert_eq!(
            std::fs::read(data_dir.join(super::DATABASE_FILE)).unwrap(),
            b"database"
        );
        assert_eq!(
            std::fs::read(data_dir.join(format!("{}-wal", super::DATABASE_FILE))).unwrap(),
            b"wal"
        );
        assert!(!data_dir.join("Cadence").exists());

        // An existing database is never touched by the relocation.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(super::DATABASE_FILE), b"stale").unwrap();
        relocate_legacy_windows_database(&data_dir);
        assert_eq!(
            std::fs::read(data_dir.join(super::DATABASE_FILE)).unwrap(),
            b"database"
        );
        assert!(legacy.join(super::DATABASE_FILE).exists());

        // A sidecar that cannot move aborts the cleanup: the legacy tree
        // stays until every file made it across.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!(
                "{}{suffix}",
                data_dir.join(super::DATABASE_FILE).display()
            ));
        }
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(super::DATABASE_FILE), b"database").unwrap();
        std::fs::write(legacy.join(format!("{}-wal", super::DATABASE_FILE)), b"wal").unwrap();
        std::fs::create_dir(data_dir.join(format!("{}-wal", super::DATABASE_FILE))).unwrap();
        relocate_legacy_windows_database(&data_dir);
        assert!(data_dir.join(super::DATABASE_FILE).exists());
        assert!(!legacy.join(super::DATABASE_FILE).exists());
        assert!(
            legacy
                .join(format!("{}-wal", super::DATABASE_FILE))
                .exists()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn smart_shuffle_seen_history_round_trips_caps_and_clears() {
        let mut store = Store::in_memory().unwrap();
        assert!(store.smart_shuffle_seen().unwrap().is_empty());

        store
            .add_smart_shuffle_seen(&["one".to_owned(), "two".to_owned()])
            .unwrap();
        // Re-adding an id keeps one entry, oldest-first order preserved.
        store
            .add_smart_shuffle_seen(&["two".to_owned(), "three".to_owned()])
            .unwrap();
        assert_eq!(store.smart_shuffle_seen().unwrap(), ["one", "two", "three"]);

        for i in 0..super::SMART_SHUFFLE_SEEN_CAP {
            store
                .add_smart_shuffle_seen(&[format!("fill-{i}")])
                .unwrap();
        }
        let seen = store.smart_shuffle_seen().unwrap();
        assert_eq!(seen.len(), super::SMART_SHUFFLE_SEEN_CAP);
        // The oldest entries fell off; the newest additions survived.
        assert!(!seen.contains(&"one".to_owned()));
        assert_eq!(
            seen.last().map(String::as_str),
            Some(format!("fill-{}", super::SMART_SHUFFLE_SEEN_CAP - 1).as_str())
        );

        store.clear_smart_shuffle_seen().unwrap();
        assert!(store.smart_shuffle_seen().unwrap().is_empty());
    }

    #[test]
    fn recent_tracks_are_newest_first() {
        let mut store = Store::in_memory().unwrap();
        store.add_history(&track("one")).unwrap();
        store.add_history(&track("two")).unwrap();
        store.add_history(&track("one")).unwrap();

        let recent = store.recent_tracks(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].source_id, "one");
        assert_eq!(recent[1].source_id, "two");
    }

    #[test]
    fn playlists_can_be_pinned_and_unpinned() {
        let mut store = Store::in_memory().unwrap();
        let playlist = Playlist {
            provider: Provider::Spotify,
            source_id: "focus".to_owned(),
            name: "Focus".to_owned(),
            owner: "Owner".to_owned(),
            track_count: 10,
            artwork_url: None,
        };

        store.set_playlist_pinned(&playlist, true).unwrap();
        assert_eq!(store.pinned_playlists().unwrap(), vec![playlist.clone()]);

        store.set_playlist_pinned(&playlist, false).unwrap();
        assert!(store.pinned_playlists().unwrap().is_empty());
    }
}
