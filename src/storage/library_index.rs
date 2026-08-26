//! Where the playlist order is kept between launches.
//!
//! The index is fed from two places that fail independently — the rootlist
//! and the recently-played endpoint — so each writes only its own columns.
//! A refresh that got half way through leaves a fresh half beside
//! yesterday's other half rather than throwing both away.

use anyhow::Result;
use rusqlite::params;

use crate::library_index::{EntryKind, IndexEntry, LibraryIndex, PlaylistSort};

use super::Store;

/// Preference keys for the two scalars the index is refreshed against.
const REVISION_KEY: &str = "rootlist_revision";
const WATERMARK_KEY: &str = "recently_played_watermark";
const SORT_KEY: &str = "playlist_sort";
const EXPANDED_KEY: &str = "expanded_folders";

impl Store {
    /// The whole index, in rootlist order. Rows whose stored kind is no
    /// longer one this build knows are skipped rather than failing the read.
    pub fn library_index(&self) -> Result<LibraryIndex> {
        let mut statement = self.connection.prepare(
            "SELECT uri, kind, folder, position, name, added_at, last_played
             FROM library_index ORDER BY position",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let entries = rows
            .map(|row| {
                let (uri, kind, folder, position, name, added_at, last_played) = row?;
                Ok(EntryKind::parse(&kind).map(|kind| IndexEntry {
                    uri,
                    kind,
                    folder,
                    position: position.clamp(0, i64::from(u32::MAX)) as u32,
                    name,
                    added_at,
                    last_played,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LibraryIndex {
            entries: entries.into_iter().flatten().collect(),
        })
    }

    /// Replaces what the rootlist owns — the entry set, the tree, the
    /// positions and Date Added — keeping the play times the other source
    /// wrote.
    ///
    /// `complete` says whether `entries` is the whole rootlist. Only then
    /// are entries missing from it dropped, and only then is `revision`
    /// stored: a revision saved over a half-written index would make every
    /// later refresh skip the walk and serve the hole forever.
    pub fn replace_rootlist(
        &mut self,
        entries: &[IndexEntry],
        revision: Option<&str>,
        complete: bool,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        if complete {
            transaction.execute(
                "DELETE FROM library_index WHERE uri NOT IN (SELECT value FROM json_each(?1))",
                params![serde_json::to_string(
                    &entries
                        .iter()
                        .map(|entry| entry.uri.as_str())
                        .collect::<Vec<_>>()
                )?],
            )?;
        }
        for entry in entries {
            let position = i64::from(entry.position);
            transaction.execute(
                "INSERT INTO library_index (uri, kind, folder, position, name, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (uri) DO UPDATE SET
                     kind = excluded.kind,
                     folder = excluded.folder,
                     position = excluded.position,
                     name = excluded.name,
                     added_at = excluded.added_at",
                params![
                    entry.uri,
                    entry.kind.as_str(),
                    entry.folder,
                    position,
                    entry.name,
                    entry.added_at
                ],
            )?;
        }
        if let Some(revision) = revision.filter(|_| complete) {
            transaction.execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![REVISION_KEY, revision],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Records when each context was last played. Only entries the rootlist
    /// has already placed are touched: the endpoint reports albums, artists
    /// and single tracks too, and none of those has a row in the list.
    pub fn apply_recently_played(&mut self, plays: &[(String, i64)]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        for (uri, played) in plays {
            transaction.execute(
                "UPDATE library_index SET last_played = ?2 WHERE uri = ?1",
                params![uri, played],
            )?;
        }
        // Later pages of one refresh hold older plays, so the watermark only
        // ever rises: it is the newest play the index has ever taken in.
        if let Some(watermark) = plays.iter().map(|(_, played)| *played).max() {
            transaction.execute(
                "INSERT INTO preferences (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value
                 WHERE CAST(excluded.value AS INTEGER) > CAST(preferences.value AS INTEGER)",
                params![WATERMARK_KEY, watermark.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Records a play Cadence itself started, so the playlist keeps its new
    /// place across a restart as well as on screen.
    pub fn set_last_played(&mut self, uri: &str, played_at_ms: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE library_index SET last_played = ?2 WHERE uri = ?1",
            params![uri, played_at_ms],
        )?;
        Ok(())
    }

    /// The rootlist revision the stored index was built from. A refresh
    /// whose first page reports this same revision stops there.
    pub fn rootlist_revision(&self) -> Result<Option<String>> {
        Ok(self
            .preference(REVISION_KEY)?
            .filter(|value| !value.is_empty()))
    }

    /// The newest play time the index has taken in. A refresh whose first
    /// page reports nothing newer has nothing to page for.
    pub fn recently_played_watermark(&self) -> Result<Option<i64>> {
        Ok(self
            .preference(WATERMARK_KEY)?
            .and_then(|value| value.parse().ok()))
    }

    /// The sort the listener last chose; Recents until they choose one.
    pub fn playlist_sort(&self) -> Result<PlaylistSort> {
        Ok(self
            .preference(SORT_KEY)?
            .as_deref()
            .and_then(PlaylistSort::parse)
            .unwrap_or_default())
    }

    pub fn set_playlist_sort(&mut self, sort: PlaylistSort) -> Result<()> {
        self.set_preference(SORT_KEY, sort.as_str())
    }

    /// The folders the listener left open. Stored as one JSON list: the set
    /// is small, read whole, and written whole.
    pub fn expanded_folders(&self) -> Result<Vec<String>> {
        Ok(self
            .preference(EXPANDED_KEY)?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default())
    }

    pub fn set_expanded_folders(&mut self, folders: &[String]) -> Result<()> {
        self.set_preference(EXPANDED_KEY, &serde_json::to_string(folders)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library_index::normalise_uri;

    fn entry(uri: &str, kind: EntryKind, position: u32, added_at: Option<i64>) -> IndexEntry {
        IndexEntry {
            uri: normalise_uri(uri),
            kind,
            folder: None,
            position,
            name: (kind == EntryKind::Folder).then(|| "Mixes".to_owned()),
            added_at,
            last_played: None,
        }
    }

    #[test]
    fn an_index_survives_a_storage_round_trip() {
        let mut store = Store::in_memory().unwrap();
        let mut entries = vec![
            entry("spotify:playlist:aaa", EntryKind::Playlist, 0, Some(300)),
            entry("spotify:folder:f1", EntryKind::Folder, 1, Some(50)),
            entry("spotify:playlist:bbb", EntryKind::Playlist, 2, None),
        ];
        entries[2].folder = Some("spotify:folder:f1".to_owned());

        store
            .replace_rootlist(&entries, Some("00024baf"), true)
            .unwrap();

        assert_eq!(store.library_index().unwrap().entries, entries);
        assert_eq!(
            store.rootlist_revision().unwrap().as_deref(),
            Some("00024baf")
        );
        assert_eq!(store.recently_played_watermark().unwrap(), None);
    }

    #[test]
    fn plays_land_on_placed_entries_and_survive_the_next_rootlist() {
        let mut store = Store::in_memory().unwrap();
        let entries = vec![
            entry("spotify:playlist:aaa", EntryKind::Playlist, 0, Some(300)),
            entry("spotify:playlist:bbb", EntryKind::Playlist, 1, Some(400)),
        ];
        store
            .replace_rootlist(&entries, Some("rev1"), true)
            .unwrap();

        store
            .apply_recently_played(&[
                ("spotify:playlist:aaa".to_owned(), 900),
                // Never placed by the rootlist: reported, but not a row.
                ("spotify:album:zzz".to_owned(), 999),
            ])
            .unwrap();
        store
            .set_last_played("spotify:playlist:bbb", 1_000)
            .unwrap();

        let played = |store: &Store, uri: &str| {
            store
                .library_index()
                .unwrap()
                .entries
                .into_iter()
                .find(|entry| entry.uri == uri)
                .and_then(|entry| entry.last_played)
        };
        assert_eq!(played(&store, "spotify:playlist:aaa"), Some(900));
        assert_eq!(played(&store, "spotify:playlist:bbb"), Some(1_000));
        // The watermark is the newest play the endpoint reported, whether or
        // not the index had a row for it.
        assert_eq!(store.recently_played_watermark().unwrap(), Some(999));

        // A later rootlist rewrites the tree without losing the play times.
        let mut moved = entries.clone();
        moved.swap(0, 1);
        moved[0].position = 0;
        moved[1].position = 1;
        store.replace_rootlist(&moved, Some("rev2"), true).unwrap();
        assert_eq!(played(&store, "spotify:playlist:aaa"), Some(900));
        assert_eq!(store.rootlist_revision().unwrap().as_deref(), Some("rev2"));
    }

    #[test]
    fn a_rootlist_that_dropped_an_entry_drops_its_row() {
        let mut store = Store::in_memory().unwrap();
        store
            .replace_rootlist(
                &[
                    entry("spotify:playlist:aaa", EntryKind::Playlist, 0, None),
                    entry("spotify:playlist:bbb", EntryKind::Playlist, 1, None),
                ],
                None,
                true,
            )
            .unwrap();

        store
            .replace_rootlist(
                &[entry("spotify:playlist:bbb", EntryKind::Playlist, 0, None)],
                None,
                true,
            )
            .unwrap();

        assert_eq!(
            store
                .library_index()
                .unwrap()
                .entries
                .into_iter()
                .map(|entry| entry.uri)
                .collect::<Vec<_>>(),
            vec!["spotify:playlist:bbb"]
        );
        // No revision was reported, so none was stored to skip the next walk.
        assert_eq!(store.rootlist_revision().unwrap(), None);
    }

    #[test]
    fn the_sort_and_the_open_folders_are_remembered() {
        let mut store = Store::in_memory().unwrap();
        assert_eq!(store.playlist_sort().unwrap(), PlaylistSort::Recents);
        assert!(store.expanded_folders().unwrap().is_empty());

        store.set_playlist_sort(PlaylistSort::Creator).unwrap();
        store
            .set_expanded_folders(&["spotify:folder:f1".to_owned()])
            .unwrap();

        assert_eq!(store.playlist_sort().unwrap(), PlaylistSort::Creator);
        assert_eq!(
            store.expanded_folders().unwrap(),
            vec!["spotify:folder:f1".to_owned()]
        );
    }

    #[test]
    fn clearing_the_library_cache_clears_the_index_with_it() {
        let mut store = Store::in_memory().unwrap();
        store
            .replace_rootlist(
                &[entry("spotify:playlist:aaa", EntryKind::Playlist, 0, None)],
                Some("rev1"),
                true,
            )
            .unwrap();

        store.clear_library_cache().unwrap();

        assert!(store.library_index().unwrap().is_empty());
    }
}
