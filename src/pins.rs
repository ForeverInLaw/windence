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

use crate::library_index::normalise_uri;
use crate::proto::collection2v2::{CollectionItem, DeltaResponse, PageResponse};

/// The account's pins, in the order Spotify holds them.
///
/// The order is the list: Spotify draws the set newest first by each item's
/// `added_at`, and every client rewrites those numbers to say where things
/// sit. So the number is a sort key and never a date — the library's own
/// Date Added comes from the rootlist and is untouched by any of this — and
/// Cadence keeps the order rather than the numbers, writing fresh ones out
/// of the list's own order. Pin order is hand-made, so nothing here sorts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pins {
    uris: Vec<String>,
}

impl Pins {
    /// The pins as stored, which is how they were last read.
    pub fn from_uris(uris: impl IntoIterator<Item = String>) -> Self {
        let mut set = Self::default();
        for uri in uris {
            set.insert(normalise_uri(&uri));
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
        let uri = normalise_uri(&item.uri);
        if item.is_removed {
            self.remove(&uri);
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

    fn remove(&mut self, uri: &str) {
        self.uris.retain(|pinned| pinned != uri);
    }

    /// Takes in everything `other` holds that this set does not.
    ///
    /// A write sends the whole pin list, so the set about to be written has
    /// to hold both sides: what the account holds now, freshly read, and
    /// what Cadence already had. Neither side's pin is dropped, and the
    /// fresh side keeps the order, because that is the order Spotify draws.
    pub fn merge(&mut self, other: &Self) {
        for uri in &other.uris {
            self.insert(uri.clone());
        }
    }

    /// Pins one item, at the end of the section. Pinning something already
    /// pinned changes nothing.
    pub fn pin(&mut self, uri: &str) {
        self.insert(normalise_uri(uri));
    }

    /// Moves a pin to where `target` sits, which is what dropping one row
    /// on another means: dragging up puts it above the row it was dropped
    /// on, dragging down puts it below. A uri neither side knows changes
    /// nothing.
    pub fn move_onto(&mut self, uri: &str, target: &str) {
        let uri = normalise_uri(uri);
        let target = normalise_uri(target);
        let (Some(from), Some(to)) = (self.position(&uri), self.position(&target)) else {
            return;
        };
        let pin = self.uris.remove(from);
        // Removing first is what makes the two directions differ: dragging
        // down shifts the target up into the freed slot, so the pin lands
        // after it rather than in front of it.
        let to = self.position(&target).unwrap_or(to);
        self.uris.insert(if from < to { to + 1 } else { to }, pin);
    }

    fn position(&self, uri: &str) -> Option<usize> {
        self.uris.iter().position(|pinned| pinned == uri)
    }

    /// Unpins one item. Unpinning something not pinned changes nothing.
    pub fn unpin(&mut self, uri: &str) {
        self.remove(&normalise_uri(uri));
    }

    /// Every pin, in order, including the ones no section draws.
    pub fn uris(&self) -> &[String] {
        &self.uris
    }

    /// The whole set as a write sends it: every pin, in order, none removed.
    /// Spotify replaces the set with what it is given, so a partial list
    /// here would unpin the rest.
    ///
    /// `now` is the second the first pin is stamped with, and each pin after
    /// it takes one second less. Spotify draws the set newest first, so
    /// numbering down the list is how the list's order becomes the drawn
    /// order — on this account and on every other device.
    pub fn write_items(&self, now: i32) -> Vec<CollectionItem> {
        self.uris
            .iter()
            .enumerate()
            .map(|(position, uri)| CollectionItem {
                uri: uri.clone(),
                added_at: now.saturating_sub(position.try_into().unwrap_or(i32::MAX)),
                ..Default::default()
            })
            .collect()
    }

    pub fn contains(&self, uri: &str) -> bool {
        self.uris.iter().any(|pinned| pinned == uri)
    }

    /// How many pins the account holds, Liked Songs included: the limit the
    /// interface warns about counts everything in the set.
    pub fn len(&self) -> usize {
        self.uris.len()
    }

    pub fn is_empty(&self) -> bool {
        self.uris.is_empty()
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
        Pins::from_uris(uris.iter().map(|uri| (*uri).to_owned()))
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
    fn a_pin_write_sends_the_whole_set_numbered_down_the_list() {
        let mut pins = pins_of(&["spotify:collection", "spotify:user:someone:folder:f1"]);
        pins.pin("spotify:playlist:aaa");
        // Pinning it again is not a second pin, and does not move it.
        pins.pin("spotify:playlist:aaa");

        let items = pins.write_items(1_800_000_000);
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|item| !item.is_removed));
        assert_eq!(items[1].uri, "spotify:folder:f1");
        assert_eq!(items[2].uri, "spotify:playlist:aaa");
        // Spotify draws the set newest first, so the numbers have to fall
        // down the list for the list to be what it draws.
        assert_eq!(items[0].added_at, 1_800_000_000);
        assert_eq!(items[1].added_at, 1_799_999_999);
        assert_eq!(items[2].added_at, 1_799_999_998);
        assert_eq!(pins.len(), 3);
    }

    #[test]
    fn a_pin_dropped_on_another_takes_its_place() {
        let mut pins = pins_of(&[
            "spotify:collection",
            "spotify:playlist:aaa",
            "spotify:playlist:bbb",
            "spotify:playlist:ccc",
        ]);

        // Dragged up: it goes in front of the row it was dropped on.
        pins.move_onto("spotify:playlist:ccc", "spotify:playlist:aaa");
        assert_eq!(
            pins.uris(),
            [
                "spotify:collection",
                "spotify:playlist:ccc",
                "spotify:playlist:aaa",
                "spotify:playlist:bbb",
            ]
        );

        // Dragged down: it goes behind it. The legacy spelling of the same
        // playlist names the same pin.
        pins.move_onto("spotify:user:someone:playlist:ccc", "spotify:playlist:bbb");
        assert_eq!(
            pins.uris(),
            [
                "spotify:collection",
                "spotify:playlist:aaa",
                "spotify:playlist:bbb",
                "spotify:playlist:ccc",
            ]
        );

        // Neither an unpinned row nor an unpinned target moves anything.
        pins.move_onto("spotify:playlist:zzz", "spotify:playlist:aaa");
        pins.move_onto("spotify:playlist:aaa", "spotify:playlist:zzz");
        assert_eq!(
            pins.uris(),
            [
                "spotify:collection",
                "spotify:playlist:aaa",
                "spotify:playlist:bbb",
                "spotify:playlist:ccc",
            ]
        );
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
