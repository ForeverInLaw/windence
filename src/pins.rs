//! The pinned section: which items the account has pinned, and in what
//! order Spotify holds them.
//!
//! Pins live in a collection set called `ylpin`. It is read whole through
//! `collection/v2/paging`, kept current through `collection/v2/delta` with
//! the sync token the last response handed back, and Spotify pushes a
//! message over the dealer socket whenever the set changes on any device.
//! A change made here goes back through `collection/v2/write`.
//!
//! Everything here is pure. Decoding takes protobuf messages and gives back
//! a pin list, and a write is built from that list; see [`crate::playback`]
//! for the requests and [`crate::storage`] for where the list is kept.

use serde::{Deserialize, Serialize};

use crate::library_index::normalise_uri;
use crate::proto::collection2v2::{CollectionItem, DeltaResponse, PageResponse};

/// One pin: what is pinned, and when it entered the library.
///
/// `added_at` is not when the item was pinned. Spotify carries the
/// library's own Date Added here, in seconds, and a write sends it back
/// unchanged — which is why it is kept rather than recomputed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Pin {
    pub uri: String,
    pub added_at: i32,
}

impl Pin {
    /// A pin in the one spelling the index and the pin set share.
    pub fn new(uri: &str, added_at: i32) -> Self {
        Self {
            uri: normalise_uri(uri),
            added_at,
        }
    }
}

/// The account's pins, in the order Spotify holds them. Pin order is
/// hand-made by the listener, so nothing here ever sorts it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pins {
    pins: Vec<Pin>,
}

impl Pins {
    /// The pins as stored, which is how they were last read.
    pub fn from_pins(pins: impl IntoIterator<Item = Pin>) -> Self {
        let mut set = Self::default();
        for pin in pins {
            set.insert(Pin::new(&pin.uri, pin.added_at));
        }
        set
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
        let pin = Pin::new(&item.uri, item.added_at);
        if item.is_removed {
            self.remove(&pin.uri);
        } else {
            self.insert(pin);
        }
    }

    /// Adds an already normalised pin at the end, unless it is already held:
    /// one uri is one pin, and re-reading it must not double it.
    fn insert(&mut self, pin: Pin) {
        if !self.contains(&pin.uri) {
            self.pins.push(pin);
        }
    }

    fn remove(&mut self, uri: &str) {
        self.pins.retain(|pin| pin.uri != uri);
    }

    /// Takes in everything `other` holds that this set does not.
    ///
    /// A write sends the whole pin list, so the set about to be written has
    /// to hold both sides: what the account holds now, freshly read, and
    /// what Cadence already had. Neither side's pin is dropped, and the
    /// fresh side keeps the order, because that is the order Spotify draws.
    pub fn merge(&mut self, other: &Self) {
        for pin in &other.pins {
            self.insert(pin.clone());
        }
    }

    /// Pins one item, at the end of the section. Pinning something already
    /// pinned changes nothing.
    pub fn pin(&mut self, uri: &str, added_at: i32) {
        self.insert(Pin::new(uri, added_at));
    }

    /// Unpins one item. Unpinning something not pinned changes nothing.
    pub fn unpin(&mut self, uri: &str) {
        self.remove(&normalise_uri(uri));
    }

    /// Every pin, in order, including the ones no section draws.
    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// The pinned uris alone, which is all the library list needs to draw
    /// the section.
    pub fn uris(&self) -> Vec<String> {
        self.pins.iter().map(|pin| pin.uri.clone()).collect()
    }

    /// The whole set as a write sends it: every pin, in order, none removed.
    /// Spotify replaces the set with what it is given, so a partial list
    /// here would unpin the rest.
    pub fn write_items(&self) -> Vec<CollectionItem> {
        self.pins
            .iter()
            .map(|pin| CollectionItem {
                uri: pin.uri.clone(),
                added_at: pin.added_at,
                ..Default::default()
            })
            .collect()
    }

    pub fn contains(&self, uri: &str) -> bool {
        self.pins.iter().any(|pin| pin.uri == uri)
    }

    /// How many pins the account holds, Liked Songs included: the limit the
    /// interface warns about counts everything in the set.
    pub fn len(&self) -> usize {
        self.pins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }

    /// What an unpin sends: the one item, marked removed.
    ///
    /// Unlike a pin this is a change rather than a replacement, so nothing
    /// has to be read first — which is also how the official client does it.
    pub fn removal_items(uri: &str) -> Vec<CollectionItem> {
        vec![CollectionItem {
            uri: normalise_uri(uri),
            is_removed: true,
            ..Default::default()
        }]
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

    fn pins_of(uris: &[&str]) -> Pins {
        Pins::from_pins(uris.iter().map(|uri| Pin::new(uri, 0)))
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
        let mut pins = pins_of(&["spotify:playlist:aaa", "spotify:playlist:bbb"]);

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

    #[test]
    fn a_merge_keeps_both_sides_and_the_fresh_order() {
        // What the account holds now: a pin made on another device since
        // Cadence last read, and one Cadence never saw at all.
        let mut fresh = pins_of(&[
            "spotify:collection",
            "spotify:playlist:from-the-phone",
            "spotify:playlist:aaa",
        ]);
        let local = pins_of(&[
            "spotify:collection",
            "spotify:playlist:aaa",
            "spotify:playlist:only-here",
        ]);

        fresh.merge(&local);

        assert_eq!(
            fresh.uris(),
            [
                // The fresh read owns the order.
                "spotify:collection",
                "spotify:playlist:from-the-phone",
                "spotify:playlist:aaa",
                // What only Cadence held is kept, at the end.
                "spotify:playlist:only-here",
            ]
        );
    }

    #[test]
    fn a_pin_write_sends_the_whole_set_with_its_dates() {
        let mut pins = Pins::from_pins([
            Pin::new("spotify:collection", 1_708_029_391),
            Pin::new("spotify:user:someone:folder:f1", 1_708_029_386),
        ]);
        pins.pin("spotify:playlist:aaa", 1_708_029_349);
        // Pinning it again is not a second pin, and does not move it.
        pins.pin("spotify:playlist:aaa", 999);

        let items = pins.write_items();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|item| !item.is_removed));
        assert_eq!(items[1].uri, "spotify:folder:f1");
        assert_eq!(items[1].added_at, 1_708_029_386);
        assert_eq!(items[2].uri, "spotify:playlist:aaa");
        assert_eq!(items[2].added_at, 1_708_029_349);
        assert_eq!(pins.len(), 3);
    }

    #[test]
    fn an_unpin_sends_one_removal_and_drops_it_here() {
        let items = Pins::removal_items("spotify:user:someone:playlist:aaa");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].uri, "spotify:playlist:aaa");
        assert!(items[0].is_removed);

        let mut pins = pins_of(&["spotify:playlist:aaa", "spotify:playlist:bbb"]);
        pins.unpin("spotify:user:someone:playlist:aaa");
        assert_eq!(pins.uris(), ["spotify:playlist:bbb"]);
        // Unpinning what is not pinned is not an error and changes nothing.
        pins.unpin("spotify:playlist:aaa");
        assert_eq!(pins.uris(), ["spotify:playlist:bbb"]);
    }
}
