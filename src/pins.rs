//! The pinned section: which items the account has pinned, and in what
//! order Spotify holds them.
//!
//! Pins live in a collection set called `ylpin`. It is read whole through
//! `collection/v2/paging`, kept current through `collection/v2/delta` with
//! the sync token the last response handed back, and Spotify pushes a
//! message over the dealer socket whenever the set changes on any device.
//!
//! Everything here is pure. Decoding takes protobuf messages and gives back
//! a pin list; see [`crate::playback`] for the requests and
//! [`crate::storage`] for where the list is kept.

use crate::library_index::normalise_uri;
use crate::proto::collection2v2::{CollectionItem, DeltaResponse, PageResponse};

/// The account's pins, in the order Spotify holds them. Pin order is
/// hand-made by the listener, so nothing here ever sorts it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pins {
    uris: Vec<String>,
}

impl Pins {
    /// The pins as stored, which is how they were last read.
    pub fn from_uris(uris: impl IntoIterator<Item = String>) -> Self {
        let mut pins = Self::default();
        for uri in uris {
            pins.insert(normalise_uri(&uri));
        }
        pins
    }

    /// Reads one page of a full read. The set arrives in Spotify's order,
    /// one page at a time, so pages are appended rather than replacing what
    /// came before.
    pub fn read_page(&mut self, page: &PageResponse) {
        for item in &page.items {
            self.apply(item);
        }
    }

    /// Applies the changes since the sync token the request carried.
    ///
    /// A delta says what changed but not where it sits, so a pin made on
    /// another device lands at the end of the section. The next full read —
    /// the next launch — puts it where Spotify holds it.
    pub fn apply_delta(&mut self, delta: &DeltaResponse) {
        for item in &delta.items {
            self.apply(item);
        }
    }

    fn apply(&mut self, item: &CollectionItem) {
        if item.uri.trim().is_empty() {
            return;
        }
        let uri = normalise_uri(&item.uri);
        if item.is_removed {
            self.uris.retain(|pinned| *pinned != uri);
        } else {
            self.insert(uri);
        }
    }

    /// Adds an already normalised pin at the end, unless it is already held:
    /// one uri is one pin, and re-reading it must not double it.
    fn insert(&mut self, uri: String) {
        if !self.contains(&uri) {
            self.uris.push(uri);
        }
    }

    /// Every pin, in order, including the ones no section draws.
    pub fn uris(&self) -> &[String] {
        &self.uris
    }

    pub fn contains(&self, uri: &str) -> bool {
        self.uris.iter().any(|pinned| pinned == uri)
    }

    pub fn is_empty(&self) -> bool {
        self.uris.is_empty()
    }
}

/// A token a response hands back for the next request to carry, whether it
/// is the sync token or the next page. An empty one is Spotify saying it has
/// none, not a token that happens to be blank.
pub fn token(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(uri: &str, removed: bool) -> CollectionItem {
        CollectionItem {
            uri: uri.to_owned(),
            is_removed: removed,
            ..Default::default()
        }
    }

    fn page(items: Vec<CollectionItem>, next: &str, sync: &str) -> PageResponse {
        PageResponse {
            items,
            next_page_token: next.to_owned(),
            sync_token: sync.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_full_read_keeps_spotifys_order_across_pages() {
        let mut pins = Pins::default();
        pins.read_page(&page(
            vec![
                item("spotify:collection", false),
                item("spotify:playlist:aaa", false),
            ],
            "page-2",
            "sync-1",
        ));
        pins.read_page(&page(
            vec![
                // The rootlist spelling of a folder, and a legacy playlist
                // uri: both name something the index already knows.
                item("spotify:user:someone:folder:f1", false),
                item("spotify:user:someone:playlist:bbb", false),
                // A repeat of what page one held must not double it.
                item("spotify:playlist:aaa", false),
            ],
            "",
            "sync-2",
        ));

        assert_eq!(
            pins.uris(),
            [
                // Liked Songs stays in the list: a later write sends the
                // whole set, and dropping it here would unpin it.
                "spotify:collection",
                "spotify:playlist:aaa",
                "spotify:folder:f1",
                "spotify:playlist:bbb",
            ]
        );
        assert_eq!(token("sync-2").as_deref(), Some("sync-2"));
        assert_eq!(token(""), None);
    }

    #[test]
    fn a_delta_adds_at_the_end_and_removes_in_place() {
        let mut pins = Pins::from_uris(vec![
            "spotify:playlist:aaa".to_owned(),
            "spotify:playlist:bbb".to_owned(),
        ]);

        pins.apply_delta(&DeltaResponse {
            delta_update_possible: true,
            items: vec![
                item("spotify:playlist:aaa", true),
                item("spotify:playlist:ccc", false),
                item("", false),
            ],
            sync_token: "sync-3".to_owned(),
            ..Default::default()
        });

        assert_eq!(
            pins.uris(),
            ["spotify:playlist:bbb", "spotify:playlist:ccc"]
        );
        assert!(pins.contains("spotify:playlist:ccc"));

        // A pin the account already holds is not a second pin.
        pins.apply_delta(&DeltaResponse {
            delta_update_possible: true,
            items: vec![item("spotify:playlist:bbb", false)],
            ..Default::default()
        });
        assert_eq!(
            pins.uris(),
            ["spotify:playlist:bbb", "spotify:playlist:ccc"]
        );
    }
}
