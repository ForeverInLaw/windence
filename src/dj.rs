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

use crate::model::{Playlist, Provider};

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

/// Whether the pin action is hidden on a playlist's page: pinned playlists
/// get a sidebar row, and DJ X already has a permanent one.
pub fn pin_hidden(source_id: &str) -> bool {
    matches(source_id)
}

/// Whether a list's rows refuse to start playback when clicked: the DJ
/// station plays the running order the DJ built, and picking a song out
/// of it is the one thing a station does not do.
pub fn row_play_hidden(source_id: &str) -> bool {
    matches(source_id)
}

/// Whether the shuffle-play action is hidden on a playlist's page: the
/// lineup is already curated by Spotify, so reordering it adds nothing.
/// This is the entire shuffle treatment — no DJ-specific ordering code
/// exists anywhere else.
pub fn shuffle_hidden(source_id: &str) -> bool {
    matches(source_id)
}

/// The internal-protocol url that starts a DJ session and returns its
/// first stretch of songs. `interactive` is idempotent — asking twice
/// returns the same stretch — so opening the page costs the session
/// nothing. The stretch after this one is reached through the returned
/// [`SessionPage::next_page_url`], never by asking again.
pub(crate) fn session_url() -> String {
    format!(
        "hm://lexicon-session-provider/context-resolve/v2/session\
         ?contextUri=spotify:playlist:{SOURCE_ID}&reason=interactive"
    )
}

/// One line the DJ has prepared, ready to be synthesized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Line {
    pub ssml: String,
}

/// One song of a DJ session, with whatever the DJ prepared to say on the
/// way in. Most songs carry nothing: only the first of each stretch does.
#[derive(Clone, Debug)]
pub(crate) struct SessionTrack {
    pub uri: String,
    /// Spoken when the song is reached in the ordinary way.
    pub intro: Option<Line>,
    /// Spoken when the listener skipped to get here.
    pub jump: Option<Line>,
}

impl SessionTrack {
    /// What to speak before this song, given how it was reached. The two
    /// variants are not interchangeable: the DJ's "moving on" line only
    /// makes sense after a skip, and its introduction only without one.
    pub fn line(&self, after_skip: bool) -> Option<&Line> {
        if after_skip {
            self.jump.as_ref()
        } else {
            self.intro.as_ref()
        }
    }
}

/// One stretch of a DJ session: the songs, and where the next stretch
/// lives. A session that has expired answers with neither, which is how
/// its end is recognised.
#[derive(Debug, Default)]
pub(crate) struct SessionPage {
    pub tracks: Vec<SessionTrack>,
    /// The server-side cursor. Fetching it is what moves the session on,
    /// so it is followed rather than re-derived.
    pub next_page_url: Option<String>,
}

/// Reads one lexicon body, whether it is a whole session (songs under
/// `pages`) or a single stretch (songs at the top level). Entries that
/// name no song are skipped, and a song already listed is not repeated.
pub(crate) fn session_page(value: &serde_json::Value) -> SessionPage {
    let entries = value
        .get("pages")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .chain(std::iter::once(value))
        .filter_map(|page| page.get("tracks").and_then(serde_json::Value::as_array))
        .flatten();

    let mut tracks: Vec<SessionTrack> = Vec::new();
    for entry in entries {
        let metadata = entry.get("metadata");
        let field = |name: &str| {
            metadata
                .and_then(|metadata| metadata.get(name))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        // The song's uri is normally on the entry; when it is empty the
        // real one hides in the metadata.
        let uri = match field("canonical_track_uri") {
            canonical if !canonical.is_empty() => canonical,
            _ => entry
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
        };
        if !uri.starts_with("spotify:track:") || tracks.iter().any(|track| track.uri == uri) {
            continue;
        }
        let line = |variant: &str| {
            let ssml = field(&format!("narration.{variant}.ssml"));
            (!ssml.is_empty()).then(|| Line {
                ssml: ssml.to_owned(),
            })
        };
        tracks.push(SessionTrack {
            uri: uri.to_owned(),
            intro: line("intro"),
            jump: line("jump"),
        });
    }

    SessionPage {
        tracks,
        next_page_url: value
            .get("next_page_url")
            .and_then(serde_json::Value::as_str)
            .filter(|url| url.starts_with("hm://"))
            .map(str::to_owned),
    }
}

/// Encodes the synthesis request for one line. The service ships no proto
/// file with our dependencies and the message is five scalar fields, so
/// the wire format is written directly. Field numbers and enum values are
/// recorded from the official desktop client: the ssml as field 2, then
/// MP3 output, voice one, the fast Sonantic provider, and the rate.
///
/// The rate asked for is playback's own. A line's metadata carries a rate
/// too, but asking for anything else would only produce audio that has to
/// be refused for want of a resampler.
pub(crate) fn tts_request(line: &Line, sample_rate: u32) -> Vec<u8> {
    fn varint(buffer: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            buffer.push((value as u8) | 0x80);
            value >>= 7;
        }
        buffer.push(value as u8);
    }

    let mut body = vec![0x12];
    varint(&mut body, line.ssml.len() as u64);
    body.extend_from_slice(line.ssml.as_bytes());
    body.extend_from_slice(&[0x18, 0x05, 0x28, 0x01, 0x30, 0x06, 0x38]);
    varint(&mut body, u64::from(sample_rate));
    body
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

    use super::{DISPLAY_NAME, SOURCE_ID, matches, pin_hidden, playlist, refusal, shuffle_hidden};

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

    #[test]
    fn a_session_body_yields_its_songs_and_its_cursor() {
        let body = serde_json::json!({
            "uri": format!("spotify:playlist:{SOURCE_ID}"),
            "pages": [{"tracks": [
                {
                    "uri": "spotify:track:2IilktLdCKhha2Mynoibtk",
                    "uid": "265a42c870f2f46f140b",
                    "metadata": {
                        "narration.intro.ssml": "<speak>Up next</speak>",
                        "narration.jump.ssml": "<speak>Moving on</speak>"
                    }
                },
                {"uri": "", "uid": "u2", "metadata": {
                    "canonical_track_uri": "spotify:track:4v5ElcHnmIUim0ezLQOyAx"
                }},
                {"uri": "spotify:album:1234567890123456789012", "uid": "u3"},
                {"uri": "spotify:track:2IilktLdCKhha2Mynoibtk", "uid": "dupe"}
            ]}],
            "next_page_url": "hm://lexicon-session-provider/context-resolve/v2/session/0?x=1"
        });

        let page = super::session_page(&body);

        assert_eq!(
            page.tracks
                .iter()
                .map(|t| t.uri.as_str())
                .collect::<Vec<_>>(),
            [
                "spotify:track:2IilktLdCKhha2Mynoibtk",
                "spotify:track:4v5ElcHnmIUim0ezLQOyAx",
            ]
        );
        assert_eq!(
            page.next_page_url.as_deref(),
            Some("hm://lexicon-session-provider/context-resolve/v2/session/0?x=1")
        );
    }

    #[test]
    fn a_page_body_parses_the_same_way_as_a_session_body() {
        let body = serde_json::json!({
            "tracks": [{"uri": "spotify:track:1HZ552FFwv8ydigu29DKpk", "uid": "p1"}],
            "next_page_url": "hm://lexicon-session-provider/next"
        });

        let page = super::session_page(&body);

        assert_eq!(page.tracks.len(), 1);
        assert_eq!(page.tracks[0].uri, "spotify:track:1HZ552FFwv8ydigu29DKpk");
        assert_eq!(
            page.next_page_url.as_deref(),
            Some("hm://lexicon-session-provider/next")
        );
    }

    #[test]
    fn a_skip_is_answered_with_the_moving_on_line_and_an_ordinary_arrival_with_the_intro() {
        let body = serde_json::json!({"tracks": [
            {"uri": "spotify:track:2IilktLdCKhha2Mynoibtk", "metadata": {
                "narration.intro.ssml": "<speak>Up next</speak>",
                "narration.jump.ssml": "<speak>Moving on</speak>"
            }},
            {"uri": "spotify:track:4v5ElcHnmIUim0ezLQOyAx"}
        ]});
        let tracks = super::session_page(&body).tracks;

        let spoken = |index: usize, after_skip: bool| {
            tracks[index]
                .line(after_skip)
                .map(|line| line.ssml.as_str())
        };
        assert_eq!(spoken(0, false), Some("<speak>Up next</speak>"));
        assert_eq!(spoken(0, true), Some("<speak>Moving on</speak>"));
        assert_eq!(spoken(1, false), None);
        assert_eq!(spoken(1, true), None);
    }

    #[test]
    fn a_synthesis_request_matches_the_bytes_the_official_client_sends() {
        let line = super::Line {
            ssml: "<speak>hi</speak>".to_owned(),
        };

        let encoded = super::tts_request(&line, 44100);

        // Recorded from the desktop client: the ssml as field 2, then
        // MP3, VOICE1, SONANTIC_FAST, and the sample rate as a varint.
        let mut expected = vec![0x12, 17];
        expected.extend_from_slice(b"<speak>hi</speak>");
        expected.extend_from_slice(&[0x18, 0x05, 0x28, 0x01, 0x30, 0x06, 0x38, 0xc4, 0xd8, 0x02]);
        assert_eq!(encoded, expected);
    }
}
