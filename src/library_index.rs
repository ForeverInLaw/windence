//! The library index: what Spotify's internal protocol says about the shape
//! of the playlist list, and the order that comes out of it.
//!
//! Spotify has no endpoint that returns an ordered library. Every client
//! keeps a local index and feeds it from shared signals, which is why
//! playing a playlist on a phone reorders it on a desktop. Cadence keeps an
//! index of the same shape: the rootlist gives the playlist set, the folder
//! tree and each item's Date Added; the recently-played endpoint gives when
//! each context was last played.
//!
//! Everything here is pure. Decoding takes protobuf messages and gives back
//! index entries; ordering takes entries and gives back rows. No session, no
//! storage, no network — see [`crate::playback`] for the fetches and
//! [`crate::storage`] for the table.

use std::collections::{HashMap, HashSet};

use librespot::protocol::playlist4_external::SelectedListContent;

use crate::model::{Playlist, Provider};
use crate::proto::recently_played_backend::RecentlyPlayed;
use crate::proto_convert;

/// Liked Songs, which the pinned set holds on most accounts. Cadence gives
/// it a permanent sidebar row of its own, so the pinned section never draws
/// it a second time.
pub const LIKED_SONGS_URI: &str = "spotify:collection";

/// The uri prefix every folder is written as once normalised.
const FOLDER_PREFIX: &str = "spotify:folder:";
const PLAYLIST_PREFIX: &str = "spotify:playlist:";
const START_GROUP_PREFIX: &str = "spotify:start-group:";
const END_GROUP_PREFIX: &str = "spotify:end-group:";

/// What one index entry is. Folders hold other entries; playlists are the
/// leaves the Web API knows names and artwork for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Playlist,
    Folder,
}

impl EntryKind {
    /// The storage spelling; `parse` is its inverse.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Playlist => "playlist",
            Self::Folder => "folder",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "playlist" => Some(Self::Playlist),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

/// One row of the index: where an item sits in the library, plus what the
/// rootlist says it is called and looks like. The Web API load stays the
/// first source of names, artwork, owners and track counts and is joined
/// by uri; the rootlist's own copy draws the playlists the Web API does not
/// list, which is every one Spotify itself made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    /// Normalised, so the three spellings of one folder are one entry.
    pub uri: String,
    pub kind: EntryKind,
    /// The folder holding this entry, or `None` at the top level.
    pub folder: Option<String>,
    /// Where the rootlist listed it, counted across every page.
    pub position: u32,
    /// Folders name themselves in their group marker; playlists are named
    /// by the rootlist's decoration.
    pub name: Option<String>,
    /// The owner's username, as the rootlist reports it.
    pub owner: Option<String>,
    pub track_count: Option<u32>,
    pub artwork_url: Option<String>,
    /// Date Added, in milliseconds since the epoch.
    pub added_at: Option<i64>,
    /// When this context was last played on any device, in milliseconds.
    pub last_played: Option<i64>,
}

impl IndexEntry {
    /// The playlist this entry draws as when the Web API has not named it:
    /// what the rootlist decorated it with. `None` for folders and for an
    /// entry the rootlist gave no name.
    pub fn playlist(&self) -> Option<Playlist> {
        if self.kind != EntryKind::Playlist {
            return None;
        }
        let owner = match self.owner.as_deref() {
            // The Web API shows display names; the one owner every account
            // has playlists from is named the way it names itself.
            Some("spotify") => "Spotify".to_owned(),
            Some(owner) => owner.to_owned(),
            None => String::new(),
        };
        Some(Playlist {
            provider: Provider::Spotify,
            source_id: self.uri.strip_prefix(PLAYLIST_PREFIX)?.to_owned(),
            name: self.name.clone()?,
            owner,
            track_count: self.track_count.unwrap_or_default(),
            artwork_url: self.artwork_url.clone(),
        })
    }
}

/// The whole index, as the app holds it.
#[derive(Clone, Debug, Default)]
pub struct LibraryIndex {
    pub entries: Vec<IndexEntry>,
}

impl LibraryIndex {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records that a context was played just now, so a playlist started in
    /// Cadence rises immediately instead of waiting for the next refresh.
    /// The folder holding it rises with it, because a folder's key is the
    /// newest of its children.
    pub fn mark_played(&mut self, uri: &str, at_ms: i64) {
        let uri = normalise_uri(uri);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.uri == uri) {
            entry.last_played = Some(at_ms);
        }
    }
}

/// Which order the playlist list is in. The same four the official client
/// offers, worded the same way.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaylistSort {
    #[default]
    Recents,
    DateAdded,
    Alphabetical,
    Creator,
}

impl PlaylistSort {
    /// Every mode, in the order the menu lists them.
    pub const ALL: [Self; 4] = [
        Self::Recents,
        Self::DateAdded,
        Self::Alphabetical,
        Self::Creator,
    ];

    /// What the menu calls this mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::Recents => "Recents",
            Self::DateAdded => "Date Added",
            Self::Alphabetical => "Alphabetical",
            Self::Creator => "Creator",
        }
    }

    /// The storage spelling; `parse` is its inverse.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recents => "recents",
            Self::DateAdded => "date_added",
            Self::Alphabetical => "alphabetical",
            Self::Creator => "creator",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    /// Whether the mode can order a library with no index behind it. Both
    /// timestamps come from the internal protocol, so without a session
    /// only the two that read the Web API's own fields can work; the other
    /// two are hidden rather than shown broken.
    pub fn works_without_index(self) -> bool {
        matches!(self, Self::Alphabetical | Self::Creator)
    }
}

/// One row of the playlist list as it is drawn: a playlist, or a folder
/// that may be opened to show the rows under it.
#[derive(Clone, Debug, PartialEq)]
pub enum LibraryRow {
    Playlist {
        playlist: Playlist,
        depth: usize,
    },
    Folder {
        uri: String,
        name: String,
        depth: usize,
        /// How many entries the folder holds directly.
        children: usize,
        expanded: bool,
    },
}

impl LibraryRow {
    /// A flat list of playlist rows: what a set with no folders and no index
    /// behind it looks like, such as search results.
    pub fn flat(playlists: impl IntoIterator<Item = Playlist>) -> Vec<Self> {
        playlists
            .into_iter()
            .map(|playlist| Self::Playlist { playlist, depth: 0 })
            .collect()
    }

    /// How far the row is indented: one step per folder above it.
    pub fn depth(&self) -> usize {
        match self {
            Self::Playlist { depth, .. } | Self::Folder { depth, .. } => *depth,
        }
    }

    /// The uri behind the row, in the spelling the index and the pin set
    /// share. This is what names a row when it is pinned or dragged.
    pub fn uri(&self) -> String {
        match self {
            Self::Playlist { playlist, .. } => playlist_uri(&playlist.source_id),
            Self::Folder { uri, .. } => uri.clone(),
        }
    }

    /// What the row is called on screen.
    pub fn label(&self) -> &str {
        match self {
            Self::Playlist { playlist, .. } => &playlist.name,
            Self::Folder { name, .. } => name,
        }
    }
}

/// The uri one playlist is known by, everywhere Cadence names it.
pub fn playlist_uri(source_id: &str) -> String {
    format!("{PLAYLIST_PREFIX}{source_id}")
}

/// Rewrites a uri into the one spelling the index uses.
///
/// One folder is named three ways across Spotify's own sources — a bare id
/// in the rootlist's group markers, `spotify:folder:<id>` in the pin set,
/// and `spotify:user:<user>:folder:<id>` in the desktop index — and the
/// legacy `spotify:user:<user>:playlist:<id>` form still turns up for
/// playlists. Everything entering the index goes through here.
pub fn normalise_uri(uri: &str) -> String {
    let uri = uri.trim();
    for (marker, prefix) in [(":folder:", FOLDER_PREFIX), (":playlist:", PLAYLIST_PREFIX)] {
        if let Some(offset) = uri.rfind(marker) {
            return format!("{prefix}{}", &uri[offset + marker.len()..]);
        }
    }
    for prefix in [START_GROUP_PREFIX, END_GROUP_PREFIX] {
        if let Some(rest) = uri.strip_prefix(prefix) {
            let id = rest.split(':').next().unwrap_or_default();
            return format!("{FOLDER_PREFIX}{id}");
        }
    }
    // A bare id appears nowhere but inside a group marker, so anything
    // without a colon is a folder; anything with one already names itself.
    if uri.contains(':') {
        uri.to_owned()
    } else {
        format!("{FOLDER_PREFIX}{uri}")
    }
}

/// A rootlist read in progress. The list arrives one page at a time and a
/// folder's group markers can straddle a page boundary, so the open folders
/// and the running position live across pages rather than inside one.
#[derive(Debug, Default)]
pub struct RootlistScan {
    entries: Vec<IndexEntry>,
    open_folders: Vec<String>,
    position: u32,
    /// The revision the first page reported, which the next refresh compares
    /// against to skip the walk entirely.
    revision: Option<String>,
    /// How many items the server says the whole list holds.
    total: u32,
}

impl RootlistScan {
    /// Reads one page into the scan.
    pub fn read(&mut self, page: &SelectedListContent) {
        if self.revision.is_none() && !page.revision().is_empty() {
            self.revision = Some(hex(page.revision()));
        }
        self.total = self.total.max(page.length().max(0) as u32);
        // The decorated list carries one meta item per item, in step:
        // the playlist's name, owner, length and cover.
        for (index, item) in page.contents.items.iter().enumerate() {
            let uri = item.uri();
            let added_at = timestamp(item.attributes.timestamp());
            let meta = page.contents.meta_items.get(index);
            if let Some(rest) = uri.strip_prefix(START_GROUP_PREFIX) {
                let (id, name) = group_marker(rest);
                let folder = format!("{FOLDER_PREFIX}{id}");
                self.push(IndexEntry {
                    uri: folder.clone(),
                    kind: EntryKind::Folder,
                    folder: self.open_folders.last().cloned(),
                    position: self.position,
                    name: Some(name),
                    owner: None,
                    track_count: None,
                    artwork_url: None,
                    added_at,
                    last_played: None,
                });
                self.open_folders.push(folder);
            } else if uri.starts_with(END_GROUP_PREFIX) {
                self.open_folders.pop();
                self.position += 1;
            } else {
                self.push(IndexEntry {
                    uri: normalise_uri(uri),
                    kind: EntryKind::Playlist,
                    folder: self.open_folders.last().cloned(),
                    position: self.position,
                    name: meta
                        .map(|meta| meta.attributes.name())
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                    owner: meta
                        .map(|meta| meta.owner_username())
                        .filter(|owner| !owner.is_empty())
                        .map(str::to_owned),
                    track_count: meta
                        .filter(|meta| meta.has_length())
                        .and_then(|meta| u32::try_from(meta.length()).ok()),
                    artwork_url: meta
                        .and_then(|meta| proto_convert::playlist_artwork(&meta.attributes)),
                    added_at,
                    last_played: None,
                });
            }
        }
    }

    fn push(&mut self, entry: IndexEntry) {
        self.entries.push(entry);
        self.position += 1;
    }

    /// How many rootlist items have been read, group markers included. The
    /// next page starts here.
    pub fn read_so_far(&self) -> u32 {
        self.position
    }

    /// Whether the server has more of the list than has been read.
    pub fn has_more(&self) -> bool {
        self.position < self.total
    }

    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    /// The entries read so far. A refresh that stopped halfway still hands
    /// back what did arrive rather than nothing at all.
    pub fn into_entries(self) -> Vec<IndexEntry> {
        self.entries
    }
}

/// When each context was last played, newest first as the endpoint returns
/// them. Entries without a uri or without a time are dropped: they say
/// nothing the index can order by.
pub fn recently_played(message: &RecentlyPlayed) -> Vec<(String, i64)> {
    message
        .contexts
        .iter()
        .filter(|context| !context.uri().is_empty())
        .filter_map(|context| {
            timestamp(context.lastPlayedTime()).map(|played| (normalise_uri(context.uri()), played))
        })
        .collect()
}

/// Whether the server withheld part of the recently-played list, going by
/// the response's own offset and total rather than by the limit asked for.
pub fn recently_played_withheld(message: &RecentlyPlayed) -> Option<u32> {
    let next = message.offset().max(0) as u32 + message.contexts.len() as u32;
    (next < message.total().max(0) as u32).then_some(next)
}

/// Everything the library draws, in one pass so that the two places it is
/// drawn cannot disagree about what is pinned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryRows {
    /// The pinned items alone, for the sidebar's own section, with the
    /// folders that section has open.
    pub pinned: Vec<LibraryRow>,
    /// The whole list: the pinned items first, in the order Spotify holds
    /// them, then everything else in the order the sort puts it. The pins
    /// here are the list's own copy, so they open with the list's folders
    /// rather than the sidebar's.
    pub all: Vec<LibraryRow>,
}

/// Builds both lists.
///
/// `playlists` is the Web API load, which owns every name, owner, artwork
/// and track count; the index owns where things sit. A playlist the index
/// has never heard of — the index is empty, or the rootlist arrived first —
/// is placed at the top level as if the rootlist had listed it last, so the
/// list is never shorter than the account's and every mode still sorts it.
///
/// `sort` orders what an opened folder holds, pinned folders included. The
/// pins themselves are hand-ordered and no mode moves them.
///
/// The two lists open their folders separately. A pinned folder is drawn in
/// both — the sidebar's own section and the top of the playlist list — and
/// opening it in one is no reason for the other to open. So `expanded` is
/// the playlist list's record, covering the pins it draws as well as the
/// tree below them, and `expanded_pins` is the sidebar section's.
pub fn rows(
    index: &LibraryIndex,
    playlists: &[Playlist],
    pins: &[String],
    sort: PlaylistSort,
    expanded: &HashSet<String>,
    expanded_pins: &HashSet<String>,
) -> LibraryRows {
    let entries = with_unplaced(index, playlists);
    let playlists = with_decorated(&entries, playlists);
    let ordering = Ordering::new(&entries, &playlists, pins, sort);
    let mut all = ordering.pinned(expanded);
    all.extend(ordering.level(None, 0, expanded));
    LibraryRows {
        pinned: ordering.pinned(expanded_pins),
        all,
    }
}

/// The index plus one top-level entry per playlist it has not placed. With
/// no index at all this is the Web API load in the order it arrived, which
/// is what Cadence showed before the index existed.
fn with_unplaced(index: &LibraryIndex, playlists: &[Playlist]) -> Vec<IndexEntry> {
    let placed: HashSet<&str> = index
        .entries
        .iter()
        .map(|entry| entry.uri.as_str())
        .collect();
    let mut entries = index.entries.clone();
    let mut position = index
        .entries
        .iter()
        .map(|entry| entry.position)
        .max()
        .map_or(0, |last| last + 1);
    for playlist in playlists {
        let uri = playlist_uri(&playlist.source_id);
        if placed.contains(uri.as_str()) {
            continue;
        }
        entries.push(IndexEntry {
            uri,
            kind: EntryKind::Playlist,
            folder: None,
            position,
            name: None,
            owner: None,
            track_count: None,
            artwork_url: None,
            // Nothing is known about when it was added or last played, so
            // the time-ordered modes leave it at the bottom.
            added_at: None,
            last_played: None,
        });
        position += 1;
    }
    entries
}

/// The Web API playlists plus one for every placed entry the Web API did
/// not list but the rootlist named. The Web API wins where both know a
/// playlist: its names and covers are the ones every other page shows.
fn with_decorated(entries: &[IndexEntry], playlists: &[Playlist]) -> Vec<Playlist> {
    let known: HashSet<String> = playlists
        .iter()
        .map(|playlist| playlist_uri(&playlist.source_id))
        .collect();
    playlists
        .iter()
        .cloned()
        .chain(
            entries
                .iter()
                .filter(|entry| !known.contains(&entry.uri))
                .filter_map(IndexEntry::playlist),
        )
        .collect()
}

/// One pass of ordering: the tree, the key every entry sorts by, and what
/// the Web API knows each playlist is called.
struct Ordering<'a> {
    /// Entries by the folder holding them, each level in rootlist order.
    children: HashMap<Option<&'a str>, Vec<&'a IndexEntry>>,
    /// The timestamp the two time-ordered modes compare, folders included.
    /// Empty in the two name-ordered modes, which read the Web API instead.
    newest: HashMap<&'a str, i64>,
    /// The Web API playlist behind each index entry, by uri.
    known: HashMap<String, &'a Playlist>,
    /// The index entry behind each uri, for the pinned section, which is
    /// given uris rather than a level of the tree.
    placed: HashMap<&'a str, &'a IndexEntry>,
    /// The pinned uris, in Spotify's own order.
    pins: &'a [String],
    sort: PlaylistSort,
}

impl<'a> Ordering<'a> {
    fn new(
        entries: &'a [IndexEntry],
        playlists: &'a [Playlist],
        pins: &'a [String],
        sort: PlaylistSort,
    ) -> Self {
        let known: HashMap<String, &Playlist> = playlists
            .iter()
            .map(|playlist| (playlist_uri(&playlist.source_id), playlist))
            .collect();
        // Index entries the Web API load knows nothing about are left out: a
        // playlist with no name has no row to draw.
        let mut children: HashMap<Option<&str>, Vec<&IndexEntry>> = HashMap::new();
        let mut placed = HashMap::new();
        for entry in entries {
            let listed = match entry.kind {
                EntryKind::Playlist => known.contains_key(&entry.uri),
                EntryKind::Folder => true,
            };
            if listed {
                children
                    .entry(entry.folder.as_deref())
                    .or_default()
                    .push(entry);
                placed.insert(entry.uri.as_str(), entry);
            }
        }
        for level in children.values_mut() {
            level.sort_by_key(|entry| entry.position);
        }
        let mut ordering = Self {
            children,
            newest: HashMap::new(),
            known,
            placed,
            pins,
            sort,
        };
        ordering.roll_up(None);
        ordering
    }

    /// Fills in the timestamp every entry under `folder` sorts by, deepest
    /// first, and answers with the newest of them. A folder has no play
    /// history and no Date Added a listener thinks about, so it takes the
    /// newest of what it holds.
    fn roll_up(&mut self, folder: Option<&'a str>) -> Option<i64> {
        let level = self.children.get(&folder).cloned().unwrap_or_default();
        let mut newest = None;
        for entry in level {
            let own = match self.sort {
                PlaylistSort::Recents => entry.last_played.max(entry.added_at),
                PlaylistSort::DateAdded => entry.added_at,
                PlaylistSort::Alphabetical | PlaylistSort::Creator => None,
            };
            let key = own.max(self.roll_up(Some(entry.uri.as_str())));
            if let Some(key) = key {
                self.newest.insert(entry.uri.as_str(), key);
            }
            newest = newest.max(key);
        }
        newest
    }

    /// One level of the tree in sorted order, following into the folders the
    /// listener has opened. A pinned item is drawn by [`Self::pinned`]
    /// instead, so the top level leaves it out rather than listing it twice.
    fn level(
        &self,
        folder: Option<&str>,
        depth: usize,
        expanded: &HashSet<String>,
    ) -> Vec<LibraryRow> {
        let Some(level) = self.children.get(&folder) else {
            return Vec::new();
        };
        let mut level = level.clone();
        // Ties keep their rootlist order, so equal entries do not shuffle
        // about between refreshes.
        level.sort_by(|left, right| {
            self.compare(left, right)
                .then(left.position.cmp(&right.position))
        });
        level
            .into_iter()
            .filter(|entry| folder.is_some() || !self.is_pinned(&entry.uri))
            .flat_map(|entry| self.rows_for(entry, depth, expanded))
            .collect()
    }

    /// The pinned section: every pin, in the order Spotify holds them, with
    /// a pinned folder opening in place like any other. A pin the library
    /// knows nothing about — a podcast, an artist, something a Cadence build
    /// does not draw — has no row and is passed over.
    fn pinned(&self, expanded: &HashSet<String>) -> Vec<LibraryRow> {
        self.pins
            .iter()
            .filter(|uri| uri.as_str() != LIKED_SONGS_URI)
            .filter_map(|uri| self.placed.get(uri.as_str()).copied())
            .flat_map(|entry| self.rows_for(entry, 0, expanded))
            .collect()
    }

    /// One entry's rows: itself, and what it holds when it is a folder the
    /// listener has opened.
    fn rows_for(
        &self,
        entry: &IndexEntry,
        depth: usize,
        expanded: &HashSet<String>,
    ) -> Vec<LibraryRow> {
        match entry.kind {
            EntryKind::Playlist => self
                .known
                .get(&entry.uri)
                .map(|playlist| {
                    vec![LibraryRow::Playlist {
                        playlist: (*playlist).clone(),
                        depth,
                    }]
                })
                .unwrap_or_default(),
            EntryKind::Folder => {
                let open = expanded.contains(&entry.uri);
                let mut rows = vec![LibraryRow::Folder {
                    uri: entry.uri.clone(),
                    name: entry.name.clone().unwrap_or_else(|| "Folder".to_owned()),
                    depth,
                    children: self
                        .children
                        .get(&Some(entry.uri.as_str()))
                        .map_or(0, Vec::len),
                    expanded: open,
                }];
                if open {
                    rows.extend(self.level(Some(&entry.uri), depth + 1, expanded));
                }
                rows
            }
        }
    }

    fn is_pinned(&self, uri: &str) -> bool {
        self.pins.iter().any(|pinned| pinned == uri)
    }

    fn compare(&self, left: &IndexEntry, right: &IndexEntry) -> std::cmp::Ordering {
        match self.sort {
            // Newest first, and an entry Spotify never dated counts as the
            // oldest so it settles at the bottom rather than jumping mid-list.
            PlaylistSort::Recents | PlaylistSort::DateAdded => self
                .newest
                .get(right.uri.as_str())
                .unwrap_or(&i64::MIN)
                .cmp(self.newest.get(left.uri.as_str()).unwrap_or(&i64::MIN)),
            PlaylistSort::Alphabetical | PlaylistSort::Creator => {
                self.name_key(left).cmp(&self.name_key(right))
            }
        }
    }

    fn name_key(&self, entry: &IndexEntry) -> (String, String) {
        let playlist = self.known.get(&entry.uri);
        let name = playlist
            .map(|playlist| playlist.name.as_str())
            .or(entry.name.as_deref())
            .unwrap_or_default();
        let owner = playlist.map_or("", |playlist| playlist.owner.as_str());
        name_key(self.sort, owner, name)
    }
}

/// The pair a name-ordered mode sorts by. Alphabetical reads the name alone;
/// Creator reads the creator first and the name to break ties. A folder has
/// no creator, so it sorts as if its creator were unnamed and leads the list.
fn name_key(sort: PlaylistSort, owner: &str, name: &str) -> (String, String) {
    match sort {
        PlaylistSort::Creator => (owner.to_lowercase(), name.to_lowercase()),
        _ => (String::new(), name.to_lowercase()),
    }
}

/// Splits `<id>:<url-encoded name>` out of a start-group marker. Spotify
/// percent-encodes the name, so a folder called "Дом" or "Rock & Roll"
/// arrives readable.
fn group_marker(rest: &str) -> (String, String) {
    let (id, name) = rest.split_once(':').unwrap_or((rest, ""));
    (
        id.to_owned(),
        percent_encoding::percent_decode_str(name)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

/// A Spotify timestamp in milliseconds; zero or negative means the server
/// never recorded one.
fn timestamp(milliseconds: i64) -> Option<i64> {
    (milliseconds > 0).then_some(milliseconds)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;
    use librespot::protocol::playlist4_external::{Item, ItemAttributes, ListItems};

    fn item(uri: &str, timestamp: Option<i64>) -> Item {
        Item {
            uri: Some(uri.to_owned()),
            attributes: timestamp
                .map(|timestamp| ItemAttributes {
                    timestamp: Some(timestamp),
                    ..Default::default()
                })
                .into(),
            ..Default::default()
        }
    }

    fn page(revision: &[u8], length: i32, items: Vec<Item>) -> SelectedListContent {
        SelectedListContent {
            revision: Some(revision.to_vec()),
            length: Some(length),
            contents: Some(ListItems {
                items,
                ..Default::default()
            })
            .into(),
            ..Default::default()
        }
    }

    fn playlist(id: &str, name: &str, owner: &str) -> Playlist {
        Playlist {
            provider: Provider::Spotify,
            source_id: id.to_owned(),
            name: name.to_owned(),
            owner: owner.to_owned(),
            track_count: 3,
            artwork_url: None,
        }
    }

    /// The fixture library: two loose playlists and a folder holding two more.
    fn fixture() -> (LibraryIndex, Vec<Playlist>) {
        let entry = |uri: &str, kind, folder: Option<&str>, position, added, played| IndexEntry {
            uri: uri.to_owned(),
            kind,
            folder: folder.map(str::to_owned),
            position,
            name: (kind == EntryKind::Folder).then(|| "Mixes".to_owned()),
            owner: None,
            track_count: None,
            artwork_url: None,
            added_at: added,
            last_played: played,
        };
        let index = LibraryIndex {
            entries: vec![
                entry(
                    "spotify:playlist:aaa",
                    EntryKind::Playlist,
                    None,
                    0,
                    Some(300),
                    Some(100),
                ),
                entry(
                    "spotify:folder:f1",
                    EntryKind::Folder,
                    None,
                    1,
                    Some(50),
                    None,
                ),
                entry(
                    "spotify:playlist:bbb",
                    EntryKind::Playlist,
                    Some("spotify:folder:f1"),
                    2,
                    Some(400),
                    Some(900),
                ),
                entry(
                    "spotify:playlist:ccc",
                    EntryKind::Playlist,
                    Some("spotify:folder:f1"),
                    3,
                    Some(200),
                    None,
                ),
                entry(
                    "spotify:playlist:ddd",
                    EntryKind::Playlist,
                    None,
                    5,
                    Some(1_000),
                    None,
                ),
            ],
        };
        let playlists = vec![
            playlist("aaa", "Alpha", "Zoe"),
            playlist("bbb", "Bravo", "Adam"),
            playlist("ccc", "Charlie", "Adam"),
            playlist("ddd", "Delta", "Spotify"),
        ];
        (index, playlists)
    }

    /// The whole list with nothing pinned, which is what most of these check.
    fn listed(
        index: &LibraryIndex,
        playlists: &[Playlist],
        sort: PlaylistSort,
        expanded: &HashSet<String>,
    ) -> Vec<LibraryRow> {
        rows(index, playlists, &[], sort, expanded, expanded).all
    }

    fn names(rows: &[LibraryRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                LibraryRow::Playlist { playlist, .. } => playlist.name.clone(),
                LibraryRow::Folder { name, .. } => name.clone(),
            })
            .collect()
    }

    #[test]
    fn the_three_spellings_of_one_folder_normalise_to_one() {
        let expected = "spotify:folder:ce46165c5f7ad11f";
        for uri in [
            "ce46165c5f7ad11f",
            "spotify:folder:ce46165c5f7ad11f",
            "spotify:user:aj9lqav0oqw73nb9ra85orfil:folder:ce46165c5f7ad11f",
            "spotify:start-group:ce46165c5f7ad11f:%D0%9C%D0%BE%D1%91",
            "spotify:end-group:ce46165c5f7ad11f",
        ] {
            assert_eq!(normalise_uri(uri), expected, "{uri}");
        }
        // Playlists keep their identity, legacy spelling included.
        assert_eq!(
            normalise_uri("spotify:user:someone:playlist:7k20G1aWvWJhag3Za3nJg5"),
            "spotify:playlist:7k20G1aWvWJhag3Za3nJg5"
        );
        assert_eq!(
            normalise_uri("spotify:playlist:7k20G1aWvWJhag3Za3nJg5"),
            "spotify:playlist:7k20G1aWvWJhag3Za3nJg5"
        );
    }

    #[test]
    fn a_rootlist_page_yields_entries_a_revision_and_a_total() {
        let mut scan = RootlistScan::default();
        scan.read(&page(
            &[0x00, 0x02, 0x4b, 0xaf],
            5,
            vec![
                item("spotify:playlist:aaa", Some(1_700_000_000_000)),
                item(
                    "spotify:start-group:f1:%D0%9C%D0%BE%D1%91",
                    Some(1_600_000_000_000),
                ),
                item("spotify:playlist:bbb", Some(0)),
                item("spotify:end-group:f1", None),
                item("spotify:playlist:ccc", None),
            ],
        ));

        assert_eq!(scan.revision(), Some("00024baf"));
        assert!(!scan.has_more());
        assert_eq!(scan.read_so_far(), 5);
        let entries = scan.into_entries();
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.uri.as_str(), entry.kind, entry.folder.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("spotify:playlist:aaa", EntryKind::Playlist, None),
                ("spotify:folder:f1", EntryKind::Folder, None),
                (
                    "spotify:playlist:bbb",
                    EntryKind::Playlist,
                    Some("spotify:folder:f1")
                ),
                ("spotify:playlist:ccc", EntryKind::Playlist, None),
            ]
        );
        assert_eq!(entries[0].added_at, Some(1_700_000_000_000));
        assert_eq!(entries[1].name.as_deref(), Some("Моё"));
        // A zero timestamp is Spotify saying it never recorded one.
        assert_eq!(entries[2].added_at, None);
        // The end marker still costs a position, so the next page starts right.
        assert_eq!(entries[3].position, 4);
    }

    #[test]
    fn a_decorated_page_names_each_playlist_and_says_who_made_it() {
        use librespot::protocol::playlist4_external::{ListAttributes, MetaItem, PictureSize};
        let mut content = page(
            &[0x01],
            2,
            vec![
                item("spotify:playlist:37i9dQZF1E39CQiaB7kkGx", Some(1_000)),
                item("spotify:start-group:f1:Mixes", None),
            ],
        );
        content.contents.mut_or_insert_default().meta_items = vec![
            MetaItem {
                attributes: Some(ListAttributes {
                    name: Some("Discover Weekly".to_owned()),
                    picture_size: vec![PictureSize {
                        target_name: Some("default".to_owned()),
                        url: Some("https://cdn/discover.jpg".to_owned()),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .into(),
                length: Some(30),
                owner_username: Some("spotify".to_owned()),
                ..Default::default()
            },
            MetaItem::default(),
        ];
        let mut scan = RootlistScan::default();
        scan.read(&content);
        let entries = scan.into_entries();

        assert_eq!(entries[0].name.as_deref(), Some("Discover Weekly"));
        assert_eq!(entries[0].owner.as_deref(), Some("spotify"));
        assert_eq!(entries[0].track_count, Some(30));
        assert_eq!(
            entries[0].artwork_url.as_deref(),
            Some("https://cdn/discover.jpg")
        );
        let playlist = entries[0].playlist().unwrap();
        assert_eq!(playlist.source_id, "37i9dQZF1E39CQiaB7kkGx");
        assert_eq!(playlist.owner, "Spotify");
        assert_eq!(playlist.track_count, 30);
        // The folder keeps its marker name and draws as no playlist.
        assert_eq!(entries[1].name.as_deref(), Some("Mixes"));
        assert_eq!(entries[1].playlist(), None);
    }

    #[test]
    fn a_folder_opened_on_one_page_still_holds_the_next_pages_items() {
        let mut scan = RootlistScan::default();
        scan.read(&page(
            b"rev",
            4,
            vec![item("spotify:start-group:f1:Mixes", None)],
        ));
        assert!(scan.has_more());
        assert_eq!(scan.read_so_far(), 1);
        scan.read(&page(b"rev", 4, vec![item("spotify:playlist:bbb", None)]));

        let entries = scan.into_entries();
        assert_eq!(entries[1].folder.as_deref(), Some("spotify:folder:f1"));
        assert_eq!(entries[1].position, 1);
    }

    #[test]
    fn recently_played_reads_contexts_and_reports_what_was_withheld() {
        use crate::proto::recently_played_backend::Context;
        let context = |uri: &str, played: i64| Context {
            uri: Some(uri.to_owned()),
            lastPlayedTime: Some(played),
            ..Default::default()
        };
        let message = RecentlyPlayed {
            contexts: vec![
                context("spotify:playlist:aaa", 1_787_750_862_108),
                context("spotify:user:someone:playlist:bbb", 1_787_749_191_093),
                context("spotify:playlist:ccc", 0),
                context("", 5),
            ],
            offset: Some(0),
            total: Some(10),
            ..Default::default()
        };

        assert_eq!(
            recently_played(&message),
            vec![
                ("spotify:playlist:aaa".to_owned(), 1_787_750_862_108),
                ("spotify:playlist:bbb".to_owned(), 1_787_749_191_093),
            ]
        );
        // Four contexts arrived out of ten, so the next request starts at four.
        assert_eq!(recently_played_withheld(&message), Some(4));

        let complete = RecentlyPlayed {
            contexts: vec![context("spotify:playlist:aaa", 1)],
            offset: Some(0),
            total: Some(1),
            ..Default::default()
        };
        assert_eq!(recently_played_withheld(&complete), None);
    }

    #[test]
    fn recents_orders_by_the_later_of_played_and_added_with_folders_taking_their_newest_child() {
        let (index, playlists) = fixture();
        let expanded = HashSet::from(["spotify:folder:f1".to_owned()]);

        let rows = listed(&index, &playlists, PlaylistSort::Recents, &expanded);

        // Mixes takes Bravo's play at 900, behind Delta's add at 1000 and
        // ahead of Alpha, whose newest signal is a play at 100.
        assert_eq!(
            names(&rows),
            vec!["Delta", "Mixes", "Bravo", "Charlie", "Alpha"]
        );
        assert_eq!(rows[2].depth(), 1);
        assert_eq!(rows[0].depth(), 0);
    }

    #[test]
    fn date_added_ignores_plays() {
        let (index, playlists) = fixture();
        let expanded = HashSet::from(["spotify:folder:f1".to_owned()]);

        let rows = listed(&index, &playlists, PlaylistSort::DateAdded, &expanded);

        // Mixes takes Bravo's add at 400, behind Delta's at 1000.
        assert_eq!(
            names(&rows),
            vec!["Delta", "Mixes", "Bravo", "Charlie", "Alpha"]
        );
    }

    #[test]
    fn alphabetical_and_creator_order_by_the_web_api_fields() {
        let (index, playlists) = fixture();
        let expanded = HashSet::from(["spotify:folder:f1".to_owned()]);

        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Alphabetical,
                &expanded
            )),
            vec!["Alpha", "Delta", "Mixes", "Bravo", "Charlie"]
        );
        // The folder has no creator, so it leads; then Spotify, then Zoe.
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Creator,
                &expanded
            )),
            vec!["Mixes", "Bravo", "Charlie", "Delta", "Alpha"]
        );
    }

    #[test]
    fn a_closed_folder_hides_its_children_but_keeps_its_place() {
        let (index, playlists) = fixture();

        let rows = listed(&index, &playlists, PlaylistSort::Recents, &HashSet::new());

        assert_eq!(names(&rows), vec!["Delta", "Mixes", "Alpha"]);
        match &rows[1] {
            LibraryRow::Folder {
                children, expanded, ..
            } => {
                assert_eq!(*children, 2);
                assert!(!expanded);
            }
            row => panic!("expected a folder, got {row:?}"),
        }
    }

    #[test]
    fn playing_a_playlist_lifts_it_and_its_folder_at_once() {
        let (mut index, playlists) = fixture();
        let expanded = HashSet::new();

        index.mark_played("spotify:playlist:ddd", 5_000);
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Recents,
                &expanded
            )),
            vec!["Delta", "Mixes", "Alpha"]
        );

        // The legacy spelling reaches the same entry.
        index.mark_played("spotify:user:someone:playlist:ccc", 9_000);
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Recents,
                &expanded
            )),
            vec!["Mixes", "Delta", "Alpha"]
        );
    }

    #[test]
    fn a_playlist_the_web_api_does_not_list_draws_from_the_rootlist_name() {
        let (mut index, playlists) = fixture();
        index.entries.push(IndexEntry {
            uri: "spotify:playlist:37i9dQZF1E39CQiaB7kkGx".to_owned(),
            kind: EntryKind::Playlist,
            folder: None,
            position: 9,
            name: Some("Discover Weekly".to_owned()),
            owner: Some("spotify".to_owned()),
            track_count: Some(30),
            artwork_url: None,
            added_at: Some(900),
            last_played: None,
        });

        let rows = listed(&index, &playlists, PlaylistSort::Recents, &HashSet::new());

        // It sorts in by its Date Added like any other entry.
        assert_eq!(
            names(&rows),
            vec!["Delta", "Mixes", "Discover Weekly", "Alpha"]
        );
        let discover = rows.iter().find_map(|row| match row {
            LibraryRow::Playlist { playlist, .. } if playlist.name == "Discover Weekly" => {
                Some(playlist)
            }
            _ => None,
        });
        assert_eq!(
            discover.map(|playlist| playlist.owner.as_str()),
            Some("Spotify")
        );
    }

    #[test]
    fn playlists_the_index_has_not_placed_still_get_a_row() {
        let (index, mut playlists) = fixture();
        playlists.push(playlist("eee", "Echo", "Zoe"));

        // Nothing is known about when it arrived, so a time-ordered mode
        // leaves it at the bottom.
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Recents,
                &HashSet::new()
            )),
            vec!["Delta", "Mixes", "Alpha", "Echo"]
        );
        // A name-ordered mode has everything it needs, so it sorts the new
        // playlist into place rather than leaving it below Z.
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Alphabetical,
                &HashSet::new()
            )),
            vec!["Alpha", "Delta", "Echo", "Mixes"]
        );
    }

    #[test]
    fn without_an_index_only_the_two_web_api_modes_reorder_anything() {
        let (_, playlists) = fixture();
        let index = LibraryIndex::default();
        let expanded = HashSet::new();

        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Recents,
                &expanded
            )),
            vec!["Alpha", "Bravo", "Charlie", "Delta"],
            "the Web API order stands"
        );
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Alphabetical,
                &expanded
            )),
            vec!["Alpha", "Bravo", "Charlie", "Delta"]
        );
        assert_eq!(
            names(&listed(
                &index,
                &playlists,
                PlaylistSort::Creator,
                &expanded
            )),
            vec!["Bravo", "Charlie", "Delta", "Alpha"]
        );
        for mode in PlaylistSort::ALL {
            assert_eq!(
                mode.works_without_index(),
                matches!(mode, PlaylistSort::Alphabetical | PlaylistSort::Creator),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn pins_lead_the_list_in_spotifys_order_and_no_sort_moves_them() {
        let (index, playlists) = fixture();
        let expanded = HashSet::from(["spotify:folder:f1".to_owned()]);
        let pins = [
            // Liked Songs has a permanent sidebar row of its own.
            LIKED_SONGS_URI.to_owned(),
            "spotify:playlist:ddd".to_owned(),
            "spotify:folder:f1".to_owned(),
            // Nothing in the library answers to this, so it has no row.
            "spotify:show:zzz".to_owned(),
        ];

        let pinned = rows(
            &index,
            &playlists,
            &pins,
            PlaylistSort::Recents,
            &expanded,
            &expanded,
        )
        .pinned;
        assert_eq!(names(&pinned), vec!["Delta", "Mixes", "Bravo", "Charlie"]);
        // A pinned folder opens in place, like any other.
        assert_eq!(
            pinned.iter().map(LibraryRow::depth).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );

        for sort in PlaylistSort::ALL {
            let rows = rows(&index, &playlists, &pins, sort, &expanded, &expanded);
            assert_eq!(rows.pinned, pinned, "{sort:?} moved the pins");
            assert_eq!(names(&rows.all[..pinned.len()]), names(&pinned), "{sort:?}");
            // What is pinned is drawn once: at the top, not again below.
            assert_eq!(names(&rows.all[pinned.len()..]), vec!["Alpha"], "{sort:?}");
        }
    }

    #[test]
    fn sort_modes_round_trip_through_their_storage_spelling() {
        for mode in PlaylistSort::ALL {
            assert_eq!(PlaylistSort::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(PlaylistSort::parse("nonsense"), None);
        assert_eq!(PlaylistSort::default(), PlaylistSort::Recents);
        for kind in [EntryKind::Playlist, EntryKind::Folder] {
            assert_eq!(EntryKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EntryKind::parse("nonsense"), None);
    }
}
