//! Saved playlist prefixes keep raw paging offsets separate from displayed rows.

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};

use crate::{
    model::ListedTrack,
    playlist::{PAGE_SIZE, PlaylistPage, PlaylistSnapshot},
};

use super::Store;

pub(super) fn migrate(connection: &Connection) -> Result<()> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE playlist_cache (
             account TEXT NOT NULL,
             source_id TEXT NOT NULL,
             revision TEXT NOT NULL,
             next_offset INTEGER NOT NULL CHECK (next_offset >= 0),
             total INTEGER NOT NULL CHECK (total >= 0),
             PRIMARY KEY (account, source_id)
         );
         CREATE TABLE playlist_tracks_cache (
             account TEXT NOT NULL,
             source_id TEXT NOT NULL,
             position INTEGER NOT NULL CHECK (position >= 0),
             track_json TEXT NOT NULL,
             added_at TEXT,
             PRIMARY KEY (account, source_id, position),
             FOREIGN KEY (account, source_id)
                 REFERENCES playlist_cache (account, source_id) ON DELETE CASCADE
         );
         PRAGMA user_version = 13;",
    )?;
    transaction.commit()?;
    Ok(())
}

impl Store {
    /// Reads one account's saved prefix without inferring its raw cursor from rows.
    pub fn cached_playlist(
        &self,
        account: &str,
        source_id: &str,
    ) -> Result<Option<PlaylistSnapshot>> {
        let transaction = self.connection.unchecked_transaction()?;
        let Some((revision, next_offset, total)) = transaction
            .query_row(
                "SELECT revision, next_offset, total FROM playlist_cache
                 WHERE account = ?1 AND source_id = ?2",
                params![account, source_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, u32>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let tracks = {
            let mut statement = transaction.prepare(
                "SELECT track_json, added_at FROM playlist_tracks_cache
                 WHERE account = ?1 AND source_id = ?2 ORDER BY position",
            )?;
            let rows = statement.query_map(params![account, source_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            rows.map(|row| {
                let (json, added_at) = row?;
                Ok(ListedTrack {
                    track: serde_json::from_str(&json)?,
                    added_at: added_at.map(|date| date.parse()).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(Some(PlaylistSnapshot {
            revision,
            tracks,
            next_offset,
            total,
        }))
    }

    /// Replaces a prefix and its revision together, preserving duplicate occurrences.
    pub fn replace_playlist_cache(
        &mut self,
        account: &str,
        source_id: &str,
        snapshot: &PlaylistSnapshot,
    ) -> Result<()> {
        ensure!(
            snapshot.next_offset <= snapshot.total
                && displayable_tracks(&snapshot.tracks).count()
                    <= usize::try_from(snapshot.next_offset)?,
            "playlist prefix cursor does not match its contents"
        );
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM playlist_cache WHERE account = ?1 AND source_id = ?2",
            params![account, source_id],
        )?;
        transaction.execute(
            "INSERT INTO playlist_cache (account, source_id, revision, next_offset, total)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                account,
                source_id,
                snapshot.revision,
                snapshot.next_offset,
                snapshot.total
            ],
        )?;
        insert_tracks(&transaction, account, source_id, 0, &snapshot.tracks)?;
        transaction.commit()?;
        Ok(())
    }

    /// Appends only the next page of the saved revision. Stale pages change nothing.
    pub fn append_playlist_cache(
        &mut self,
        account: &str,
        source_id: &str,
        page: &PlaylistPage,
    ) -> Result<bool> {
        let raw_count = page.next_offset.saturating_sub(page.offset);
        if raw_count == 0
            || raw_count > PAGE_SIZE
            || page.next_offset > page.total
            || displayable_tracks(&page.tracks).count() > usize::try_from(raw_count)?
        {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        let matched = transaction.execute(
            "UPDATE playlist_cache SET next_offset = ?6
             WHERE account = ?1 AND source_id = ?2 AND revision = ?3
                 AND next_offset = ?4 AND total = ?5 AND next_offset < total",
            params![
                account,
                source_id,
                page.revision,
                page.offset,
                page.total,
                page.next_offset,
            ],
        )?;
        if matched == 0 {
            return Ok(false);
        }
        let first_position = transaction.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks_cache
             WHERE account = ?1 AND source_id = ?2",
            params![account, source_id],
            |row| row.get::<_, i64>(0),
        )?;
        insert_tracks(
            &transaction,
            account,
            source_id,
            first_position,
            &page.tracks,
        )?;
        transaction.commit()?;
        Ok(true)
    }
}

fn displayable_tracks(tracks: &[ListedTrack]) -> impl Iterator<Item = &ListedTrack> {
    tracks.iter().filter(|listed| listed.track.is_displayable())
}

fn insert_tracks(
    transaction: &Transaction<'_>,
    account: &str,
    source_id: &str,
    first_position: i64,
    tracks: &[ListedTrack],
) -> Result<()> {
    let mut statement = transaction.prepare(
        "INSERT INTO playlist_tracks_cache (account, source_id, position, track_json, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (index, listed) in displayable_tracks(tracks).enumerate() {
        let position = first_position
            .checked_add(i64::try_from(index).context("playlist position exceeds SQLite range")?)
            .context("playlist position exceeds SQLite range")?;
        statement.execute(params![
            account,
            source_id,
            position,
            serde_json::to_string(&listed.track)?,
            listed.added_at.map(|date| date.to_rfc3339()),
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{ListedTrack, Provider, Track},
        playlist::{PlaylistPage, PlaylistSnapshot},
        storage::{Store, ThemePreference},
    };
    use rusqlite::Connection;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct DatabaseFile(PathBuf);

    impl DatabaseFile {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                "cadence-playlist-cache-{}-{}.sqlite3",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }

    impl Drop for DatabaseFile {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    fn listed(id: &str, added_at: &str) -> ListedTrack {
        ListedTrack {
            track: Track {
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
            },
            added_at: Some(added_at.parse().unwrap()),
        }
    }

    fn snapshot(revision: &str, next_offset: u32, total: u32) -> PlaylistSnapshot {
        PlaylistSnapshot {
            revision: revision.to_owned(),
            tracks: vec![listed("first", "2026-10-01T12:00:00Z")],
            next_offset,
            total,
        }
    }

    #[test]
    fn playlist_prefix_survives_restart_with_raw_cursor_and_duplicate_dates() {
        let file = DatabaseFile::new();
        let prefix = PlaylistSnapshot {
            revision: "revision-1".to_owned(),
            tracks: vec![
                listed("same", "2026-10-01T12:00:00Z"),
                listed("same", "2026-10-02T12:00:00.125Z"),
            ],
            next_offset: 50,
            total: 200,
        };
        {
            let mut store = Store::open(&file.0).unwrap();
            assert_eq!(store.cached_playlist("alice", "playlist").unwrap(), None);
            store
                .replace_playlist_cache("alice", "playlist", &prefix)
                .unwrap();
        }

        let reopened = Store::open(&file.0).unwrap();
        let restored = reopened.cached_playlist("alice", "playlist").unwrap();
        assert_eq!(restored, Some(prefix));
    }

    #[test]
    fn restarted_playlist_resumes_at_raw_cursor_and_appends_duplicate_occurrences() {
        let file = DatabaseFile::new();
        let first = listed("same", "2026-10-01T12:00:00Z");
        let second = listed("same", "2026-10-02T12:00:00Z");
        {
            let mut store = Store::open(&file.0).unwrap();
            store
                .replace_playlist_cache(
                    "alice",
                    "playlist",
                    &PlaylistSnapshot {
                        revision: "revision-1".to_owned(),
                        tracks: vec![first.clone()],
                        next_offset: 50,
                        total: 100,
                    },
                )
                .unwrap();
        }

        let mut reopened = Store::open(&file.0).unwrap();
        assert!(
            reopened
                .append_playlist_cache(
                    "alice",
                    "playlist",
                    &PlaylistPage {
                        revision: "revision-1".to_owned(),
                        offset: 50,
                        next_offset: 100,
                        total: 100,
                        tracks: vec![second.clone()],
                    },
                )
                .unwrap()
        );
        let complete = reopened
            .cached_playlist("alice", "playlist")
            .unwrap()
            .unwrap();
        assert!(complete.complete());
        assert_eq!(complete.tracks, vec![first, second]);
        assert_eq!(complete.next_offset, 100);
    }

    #[test]
    fn signing_out_forgets_all_accounts_saved_playlist_prefixes() {
        let mut store = Store::in_memory().unwrap();
        for account in ["alice", "bob"] {
            store
                .replace_playlist_cache(account, "playlist", &snapshot("revision-1", 50, 100))
                .unwrap();
        }

        store.clear_library_cache().unwrap();

        for account in ["alice", "bob"] {
            assert_eq!(store.cached_playlist(account, "playlist").unwrap(), None);
        }
    }

    #[test]
    fn mismatched_pages_leave_saved_prefix_unchanged() {
        let mut store = Store::in_memory().unwrap();
        let prefix = snapshot("revision-1", 50, 90);
        store
            .replace_playlist_cache("alice", "playlist", &prefix)
            .unwrap();
        let page = PlaylistPage {
            revision: "revision-1".to_owned(),
            offset: 50,
            next_offset: 90,
            total: 90,
            tracks: vec![listed("last", "2026-10-02T12:00:00Z")],
        };
        for stale in [
            PlaylistPage {
                revision: "revision-2".to_owned(),
                ..page.clone()
            },
            PlaylistPage {
                offset: 0,
                ..page.clone()
            },
            PlaylistPage {
                total: 91,
                ..page.clone()
            },
            PlaylistPage {
                next_offset: 91,
                ..page.clone()
            },
            PlaylistPage {
                next_offset: 51,
                tracks: vec![page.tracks[0].clone(); 2],
                ..page.clone()
            },
            PlaylistPage {
                next_offset: 50,
                ..page.clone()
            },
        ] {
            assert!(
                !store
                    .append_playlist_cache("alice", "playlist", &stale)
                    .unwrap()
            );
            assert_eq!(
                store.cached_playlist("alice", "playlist").unwrap(),
                Some(prefix.clone())
            );
        }
        let longer = snapshot("revision-1", 50, 200);
        store
            .replace_playlist_cache("alice", "playlist", &longer)
            .unwrap();
        let oversized = PlaylistPage {
            next_offset: 101,
            total: 200,
            ..page
        };
        assert!(
            !store
                .append_playlist_cache("alice", "playlist", &oversized)
                .unwrap()
        );
        assert_eq!(
            store.cached_playlist("alice", "playlist").unwrap(),
            Some(longer)
        );
    }

    #[test]
    fn malformed_replacement_leaves_the_previous_prefix_intact() {
        let mut store = Store::in_memory().unwrap();
        let prefix = snapshot("revision-1", 50, 100);
        store
            .replace_playlist_cache("alice", "playlist", &prefix)
            .unwrap();

        for invalid in [
            snapshot("revision-2", 101, 100),
            PlaylistSnapshot {
                revision: "revision-2".to_owned(),
                tracks: vec![prefix.tracks[0].clone(); 2],
                next_offset: 1,
                total: 100,
            },
        ] {
            assert!(
                store
                    .replace_playlist_cache("alice", "playlist", &invalid)
                    .is_err()
            );
            assert_eq!(
                store.cached_playlist("alice", "playlist").unwrap(),
                Some(prefix.clone())
            );
        }
    }

    #[test]
    fn changing_a_snapshot_preserves_other_playlists_and_accounts() {
        let mut store = Store::in_memory().unwrap();
        let old = snapshot("revision-1", 50, 100);
        for (account, source_id) in [("alice", "first"), ("alice", "second"), ("bob", "first")] {
            store
                .replace_playlist_cache(account, source_id, &old)
                .unwrap();
        }
        let replacement = PlaylistSnapshot {
            revision: "revision-2".to_owned(),
            tracks: vec![listed("new", "2026-10-03T12:00:00Z")],
            next_offset: 20,
            total: 20,
        };

        store
            .replace_playlist_cache("alice", "first", &replacement)
            .unwrap();

        assert_eq!(
            store.cached_playlist("alice", "first").unwrap(),
            Some(replacement)
        );
        assert_eq!(
            store.cached_playlist("alice", "second").unwrap(),
            Some(old.clone())
        );
        assert_eq!(store.cached_playlist("bob", "first").unwrap(), Some(old));
        assert_eq!(store.cached_playlist("bob", "second").unwrap(), None);
    }

    #[test]
    fn hidden_rows_advance_the_raw_cursor_without_changing_display_order() {
        let mut store = Store::in_memory().unwrap();
        let first = listed("first", "2026-10-01T12:00:00Z");
        let mut hidden = listed("hidden", "2026-10-02T12:00:00Z");
        hidden.track.title.clear();
        let mut last = listed("last", "2026-10-03T12:00:00Z");
        last.added_at = None;
        store
            .replace_playlist_cache(
                "alice",
                "playlist",
                &PlaylistSnapshot {
                    revision: "revision-1".to_owned(),
                    tracks: vec![first.clone(), hidden.clone()],
                    next_offset: 50,
                    total: 150,
                },
            )
            .unwrap();

        for page in [
            PlaylistPage {
                revision: "revision-1".to_owned(),
                offset: 50,
                next_offset: 100,
                total: 150,
                tracks: vec![hidden],
            },
            PlaylistPage {
                revision: "revision-1".to_owned(),
                offset: 100,
                next_offset: 150,
                total: 150,
                tracks: vec![last.clone()],
            },
        ] {
            assert!(
                store
                    .append_playlist_cache("alice", "playlist", &page)
                    .unwrap()
            );
        }

        let restored = store.cached_playlist("alice", "playlist").unwrap().unwrap();
        assert_eq!(restored.tracks, vec![first, last]);
        assert_eq!(restored.next_offset, 150);
        assert!(restored.complete());
    }

    fn version_twelve_database(file: &DatabaseFile) -> Track {
        let heard = listed("heard", "2026-10-01T12:00:00Z").track;
        {
            let mut store = Store::open(&file.0).unwrap();
            store.set_theme_preference(ThemePreference::Dark).unwrap();
            store.add_history(&heard).unwrap();
        }
        Connection::open(&file.0)
            .unwrap()
            .execute_batch(
                "DROP TABLE playlist_tracks_cache;
             DROP TABLE playlist_cache;
             PRAGMA user_version = 12;",
            )
            .unwrap();
        heard
    }

    #[test]
    fn version_twelve_profile_migrates_and_keeps_settings_and_history() {
        let file = DatabaseFile::new();
        let heard = version_twelve_database(&file);

        let mut migrated = Store::open(&file.0).unwrap();

        assert_eq!(migrated.preferences().unwrap().theme, ThemePreference::Dark);
        assert_eq!(migrated.recent_tracks(10).unwrap(), vec![heard]);
        assert_eq!(migrated.cached_playlist("alice", "playlist").unwrap(), None);
        let prefix = snapshot("revision-1", 50, 100);
        migrated
            .replace_playlist_cache("alice", "playlist", &prefix)
            .unwrap();
        assert_eq!(
            migrated.cached_playlist("alice", "playlist").unwrap(),
            Some(prefix)
        );
    }

    #[test]
    fn failed_migration_rolls_back_and_the_profile_can_be_reopened_after_repair() {
        let file = DatabaseFile::new();
        let heard = version_twelve_database(&file);
        {
            let connection = Connection::open(&file.0).unwrap();
            connection
                .execute_batch("CREATE TABLE playlist_tracks_cache (unexpected TEXT);")
                .unwrap();
        }

        assert!(Store::open(&file.0).is_err());

        {
            let connection = Connection::open(&file.0).unwrap();
            assert_eq!(
                connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                    .unwrap(),
                12
            );
            connection
                .execute_batch("DROP TABLE playlist_tracks_cache;")
                .unwrap();
        }
        // A partially committed header would prevent this second migration.
        let repaired = Store::open(&file.0).unwrap();
        assert_eq!(repaired.preferences().unwrap().theme, ThemePreference::Dark);
        assert_eq!(repaired.recent_tracks(10).unwrap(), vec![heard]);
        assert_eq!(repaired.cached_playlist("alice", "playlist").unwrap(), None);
    }
}
