//! DJ X: Spotify's AI-curated personal lineup, served as one Spotify-owned
//! playlist the server keeps rewriting for the listening account.
//!
//! Everything special about DJ X lives here as pure decisions: what the
//! playlist is, what Cadence calls it, which actions its page suppresses,
//! and which internal-protocol failures mean "Spotify does not offer this
//! here" rather than "the network broke". The fetch itself lives on the
//! playback session (see [`crate::playback`]); the Web API never serves this
//! playlist to development-mode apps.

use librespot::core::error::ErrorKind;

use crate::model::{ListedTrack, Playlist, Provider};

/// What resolving the lineup delivered.
#[derive(Debug)]
pub enum Lineup {
    /// The fresh lineup plus the refreshed entry the page header shows:
    /// real track count and artwork, still named "DJ X".
    Fresh(Playlist, Vec<ListedTrack>),
    /// Spotify serves the lineup only inside its own live Connect sessions,
    /// so no fetch can return it (see docs/adr/0004).
    NotOffered,
}

/// The well-known ID of the Spotify-owned DJ playlist. Stable since 2023:
/// no following, no discovery, no configuration.
pub const SOURCE_ID: &str = "37i9dQZF1EYkqdzj48dyYq";

/// The name every part of the app shows for the lineup, matching the
/// official client.
pub const DISPLAY_NAME: &str = "DJ X";

/// Whether a playlist is the DJ lineup. The one identity predicate; every
/// other DJ decision is derived from it so the page and sidebar cannot
/// disagree about what they are showing.
pub fn matches(source_id: &str) -> bool {
    source_id == SOURCE_ID
}

/// The synthetic library entry for the lineup: always present, nothing to
/// follow or configure. Track count and artwork arrive with the first fetch.
pub fn playlist() -> Playlist {
    Playlist {
        provider: Provider::Spotify,
        source_id: SOURCE_ID.to_owned(),
        name: DISPLAY_NAME.to_owned(),
        owner: "Spotify".to_owned(),
        track_count: 0,
        artwork_url: None,
    }
}

/// The entry as the page header shows it after a successful fetch: real
/// track count and artwork, still named "DJ X" whatever the server calls it.
pub fn refreshed_playlist(track_count: u32, artwork_url: Option<String>) -> Playlist {
    Playlist {
        track_count,
        artwork_url,
        ..playlist()
    }
}

/// Whether the pin action is hidden on a playlist's page: pinned playlists
/// get a sidebar row, and DJ X already has a permanent one.
pub fn pin_hidden(source_id: &str) -> bool {
    matches(source_id)
}

/// Whether the shuffle-play action is hidden on a playlist's page: the
/// lineup is already curated by Spotify, so reordering it adds nothing.
/// This is the entire shuffle treatment — no DJ-specific ordering code
/// exists anywhere else.
pub fn shuffle_hidden(source_id: &str) -> bool {
    matches(source_id)
}

/// Whether a failed internal-protocol fetch means Spotify refuses to serve
/// the lineup here (account or region restriction) rather than the transport
/// failing. Refusals render the dedicated "not available" state; everything
/// else keeps the ordinary retryable error treatment.
pub fn refusal(error_kind: ErrorKind) -> bool {
    use ErrorKind::*;
    matches!(error_kind, NotFound | PermissionDenied)
}

#[cfg(test)]
mod tests {
    use librespot::core::error::ErrorKind;

    use super::{
        DISPLAY_NAME, SOURCE_ID, matches, pin_hidden, playlist, refreshed_playlist, refusal,
        shuffle_hidden,
    };

    #[test]
    fn only_the_hardcoded_identity_matches() {
        [(SOURCE_ID, true)]
            .into_iter()
            .chain(
                [
                    "37i9dQZF1DXcBWIGoYBM5M", // Today's Top Hits
                    "37I9DQZF1EYKQDZJ48DYYQ", // wrong case
                    "37i9dQZF1EYkqdzj48dyYr", // one letter off
                    "spotify:playlist:37i9dQZF1EYkqdzj48dyYq",
                ]
                .map(|id| (id, false)),
            )
            .for_each(|(source_id, expected)| {
                assert_eq!(matches(source_id), expected, "{source_id}");
            });
    }

    #[test]
    fn the_synthetic_entry_carries_the_overridden_name_and_no_counts() {
        let entry = playlist();
        assert_eq!(entry.source_id, SOURCE_ID);
        assert_eq!(entry.name, DISPLAY_NAME);
        assert_eq!(entry.track_count, 0);
        assert_eq!(entry.artwork_url, None);
        assert_eq!(entry.owner, "Spotify");
    }

    #[test]
    fn refresh_fills_counts_and_artwork_but_never_the_server_name() {
        let entry = refreshed_playlist(31, Some("https://i.scdn.co/image/abc".to_owned()));
        assert_eq!(entry.name, DISPLAY_NAME);
        assert_eq!(entry.track_count, 31);
        assert_eq!(
            entry.artwork_url.as_deref(),
            Some("https://i.scdn.co/image/abc")
        );
    }

    #[test]
    fn pin_and_shuffle_are_hidden_exactly_on_the_dj_page() {
        [(SOURCE_ID, true), ("37i9dQZF1DXcBWIGoYBM5M", false)]
            .into_iter()
            .for_each(|(source_id, hidden)| {
                assert_eq!(pin_hidden(source_id), hidden, "pin for {source_id}");
                assert_eq!(shuffle_hidden(source_id), hidden, "shuffle for {source_id}");
            });
    }

    #[test]
    fn refusals_are_restrictions_and_transport_failures_are_not() {
        [
            (ErrorKind::NotFound, true),
            (ErrorKind::PermissionDenied, true),
            (ErrorKind::Unauthenticated, false),
            (ErrorKind::Unavailable, false),
            (ErrorKind::Unknown, false),
            (ErrorKind::Cancelled, false),
            (ErrorKind::FailedPrecondition, false),
        ]
        .into_iter()
        .for_each(|(kind, expected)| assert_eq!(refusal(kind), expected, "{kind:?}"));
    }
}
