use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Spotify,
    Tidal,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spotify => "spotify",
            Self::Tidal => "tidal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "spotify" => Some(Self::Spotify),
            "tidal" => Some(Self::Tidal),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtistRef {
    pub name: String,
    pub source_id: Option<String>,
    pub spotify_uri: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AlbumRef {
    pub name: String,
    pub source_id: Option<String>,
    pub spotify_uri: Option<String>,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Track {
    pub provider: Provider,
    pub source_id: String,
    pub spotify_uri: Option<String>,
    pub isrc: Option<String>,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    pub album: String,
    #[serde(default)]
    pub album_ref: Option<AlbumRef>,
    pub duration_ms: u32,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Artist {
    pub provider: Provider,
    pub source_id: String,
    pub spotify_uri: Option<String>,
    pub name: String,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Album {
    pub provider: Provider,
    pub source_id: String,
    pub spotify_uri: Option<String>,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub release_date: Option<String>,
    pub artwork_url: Option<String>,
    pub track_count: Option<u32>,
}

impl Track {
    pub fn is_displayable(&self) -> bool {
        !self.title.trim().is_empty() && !self.artist.trim().is_empty() && self.duration_ms > 0
    }
}

/// A track as one listing shows it, with the date that belongs to the
/// listing rather than the track: the same song carries different
/// `added_at` dates in different playlists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedTrack {
    pub track: Track,
    /// When the track entered this context, when the context reports it.
    pub added_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ListedTrack {
    pub fn undated(track: Track) -> Self {
        Self {
            track,
            added_at: None,
        }
    }

    /// Wraps plain tracks as an undated listing slice. Contexts without a
    /// date added (albums, search results) show tracks through the same
    /// list type as contexts with one.
    pub fn undated_slice(tracks: Vec<Track>) -> std::sync::Arc<[Self]> {
        tracks.into_iter().map(Self::undated).collect()
    }

    /// The tracks alone, in the order given — what playback consumes.
    pub fn tracks(listed: &[Self]) -> Vec<Track> {
        listed.iter().map(|entry| entry.track.clone()).collect()
    }
}

/// Which column a track list is sorted by.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListSortColumn {
    Title,
    Album,
    DateAdded,
}

impl ListSortColumn {
    /// The storage spelling of the column; `parse` is its inverse.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Album => "album",
            Self::DateAdded => "date_added",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "title" => Some(Self::Title),
            "album" => Some(Self::Album),
            "date_added" => Some(Self::DateAdded),
            _ => None,
        }
    }
}

/// The direction of an active sort. The absence of a sort is the default
/// order: the sequence Spotify reports for the context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListSortDirection {
    Ascending,
    Descending,
}

impl ListSortDirection {
    /// The storage spelling of the direction; `parse` is its inverse.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ascending => "asc",
            Self::Descending => "desc",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "asc" => Some(Self::Ascending),
            "desc" => Some(Self::Descending),
            _ => None,
        }
    }
}

/// One list's active sort: which column and which way, or none for the
/// default order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListSort {
    pub column: ListSortColumn,
    pub direction: ListSortDirection,
}

impl ListSort {
    /// The state a header click moves to. Cycling runs default → A-Z →
    /// Z-A → back to default; clicking another column starts it at A-Z.
    pub fn cycle(active: Option<Self>, column: ListSortColumn) -> Option<Self> {
        match active {
            Some(Self {
                column: active_column,
                direction,
            }) if active_column == column => match direction {
                ListSortDirection::Ascending => Some(Self {
                    column,
                    direction: ListSortDirection::Descending,
                }),
                ListSortDirection::Descending => None,
            },
            _ => Some(Self {
                column,
                direction: ListSortDirection::Ascending,
            }),
        }
    }
}

/// The display order of a listing: one entry per row, naming the index in
/// `listed` that the row shows. The default order is the identity; an
/// active sort is a stable permutation of it, so tracks equal under the
/// sort keep their relative order.
pub fn list_order(listed: &[ListedTrack], sort: Option<ListSort>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..listed.len()).collect();
    if let Some(sort) = sort {
        // Unreported dates count as the oldest entries so they hold the top
        // ascending and the bottom descending, never jumping mid-list.
        let added_key = |entry: &ListedTrack| {
            entry
                .added_at
                .map(|date| date.timestamp())
                .unwrap_or(i64::MIN)
        };
        order.sort_by(|&left, &right| {
            let ordering = match sort.column {
                ListSortColumn::Title => listed[left]
                    .track
                    .title
                    .to_lowercase()
                    .cmp(&listed[right].track.title.to_lowercase()),
                ListSortColumn::Album => listed[left]
                    .track
                    .album
                    .to_lowercase()
                    .cmp(&listed[right].track.album.to_lowercase()),
                ListSortColumn::DateAdded => {
                    added_key(&listed[left]).cmp(&added_key(&listed[right]))
                }
            };
            match sort.direction {
                ListSortDirection::Ascending => ordering,
                ListSortDirection::Descending => ordering.reverse(),
            }
        });
    }
    order
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Playlist {
    pub provider: Provider,
    pub source_id: String,
    pub name: String,
    pub owner: String,
    pub track_count: u32,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserProfile {
    pub display_name: String,
    pub artwork_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueItem {
    pub id: i64,
    pub track: Track,
}

#[cfg(test)]
mod tests {
    use super::{ListSort, ListSortColumn, ListSortDirection, ListedTrack, Track, list_order};
    use chrono::TimeZone;

    fn track(id: &str, title: &str, album: &str) -> Track {
        Track {
            provider: super::Provider::Spotify,
            source_id: id.to_owned(),
            spotify_uri: None,
            isrc: None,
            title: title.to_owned(),
            artist: "Artist".to_owned(),
            artists: Vec::new(),
            album: album.to_owned(),
            album_ref: None,
            duration_ms: 1_000,
            artwork_url: None,
        }
    }

    fn listed(id: &str, title: &str, album: &str, added_at: Option<i64>) -> ListedTrack {
        ListedTrack {
            track: track(id, title, album),
            added_at: added_at.map(|seconds| chrono::Utc.timestamp_opt(seconds, 0).unwrap()),
        }
    }

    fn sort(column: ListSortColumn, direction: ListSortDirection) -> Option<ListSort> {
        Some(ListSort { column, direction })
    }

    #[test]
    fn default_order_is_the_given_order() {
        let listed = vec![
            listed("b", "B", "Second", None),
            listed("a", "A", "First", None),
        ];
        assert_eq!(list_order(&listed, None), vec![0, 1]);
    }

    #[test]
    fn title_sort_is_case_insensitive_and_stable() {
        let listed = vec![
            listed("1", "beta", "X", None),
            listed("2", "Alpha", "X", None),
            listed("3", "alpha", "X", None),
        ];
        let order = list_order(
            &listed,
            sort(ListSortColumn::Title, ListSortDirection::Ascending),
        );
        // The two "alpha" tracks keep their relative order.
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn descending_reverses_the_comparison_but_not_ties() {
        let listed = vec![
            listed("1", "Alpha", "X", None),
            listed("2", "Beta", "X", None),
            listed("3", "Beta", "Y", None),
        ];
        let order = list_order(
            &listed,
            sort(ListSortColumn::Title, ListSortDirection::Descending),
        );
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn date_added_sorts_oldest_first_and_unknowns_count_as_oldest() {
        let listed = vec![
            listed("1", "A", "X", Some(3_000)),
            listed("2", "B", "X", None),
            listed("3", "C", "X", Some(1_000)),
        ];
        let ascending = list_order(
            &listed,
            sort(ListSortColumn::DateAdded, ListSortDirection::Ascending),
        );
        assert_eq!(ascending, vec![1, 2, 0]);
        let descending = list_order(
            &listed,
            sort(ListSortColumn::DateAdded, ListSortDirection::Descending),
        );
        assert_eq!(descending, vec![0, 2, 1]);
    }

    #[test]
    fn header_clicks_cycle_default_then_a_to_z_then_z_to_a() {
        let title = ListSortColumn::Title;
        // Default → A-Z.
        assert_eq!(
            ListSort::cycle(None, title),
            sort(title, ListSortDirection::Ascending)
        );
        // A-Z → Z-A.
        assert_eq!(
            ListSort::cycle(sort(title, ListSortDirection::Ascending), title),
            sort(title, ListSortDirection::Descending)
        );
        // Z-A → default.
        assert_eq!(
            ListSort::cycle(sort(title, ListSortDirection::Descending), title),
            None
        );
        // Another column restarts at A-Z.
        assert_eq!(
            ListSort::cycle(
                sort(title, ListSortDirection::Descending),
                ListSortColumn::Album
            ),
            sort(ListSortColumn::Album, ListSortDirection::Ascending)
        );
    }
}
