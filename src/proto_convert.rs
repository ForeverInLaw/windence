//! Conversions from librespot's internal-protocol metadata into Cadence's
//! domain model — the sibling of the Web API converters in [`crate::spotify`].
//!
//! The internal protocol speaks protobuf ([`librespot::protocol`]); everything
//! downstream of these functions sees ordinary [`crate::model`] types, so the
//! queue, shuffle machinery, and media controls cannot tell which channel a
//! context arrived through. Pure functions over hand-built messages; no
//! session or network anywhere in this module.

use anyhow::{Context as _, Result};
use chrono::{DateTime, TimeZone, Utc};
use librespot::core::{FileId, SpotifyId, SpotifyUri};
use librespot::protocol::metadata;
use librespot::protocol::playlist4_external;

use crate::model::{AlbumRef, ArtistRef, Provider, Track};

/// The artwork size the UI wants, matching the Web API converter's choice.
const TARGET_ARTWORK_SIZE: i32 = 300;

/// Domain track from the internal protocol's track metadata message.
pub fn track(message: &metadata::Track) -> Result<Track> {
    let uri = SpotifyUri::try_from(message)
        .context("internal protocol returned a track without an identity")?;
    let source_id = uri
        .to_id()
        .context("internal protocol returned an unrepresentable track id")?;
    let duration_ms = u32::try_from(message.duration())
        .context("internal protocol returned an invalid track duration")?;
    let artists: Vec<ArtistRef> = message.artist.iter().map(artist_ref).collect();
    let album = album_ref(message.album.get_or_default());
    Ok(Track {
        provider: Provider::Spotify,
        source_id,
        spotify_uri: uri.to_uri().ok(),
        isrc: message
            .external_id
            .iter()
            .find(|external_id| external_id.type_() == "isrc")
            .map(|external_id| external_id.id().to_owned()),
        title: message.name().to_owned(),
        artist: artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        artists,
        album: album.name.clone(),
        album_ref: Some(album.clone()),
        duration_ms,
        artwork_url: album.artwork_url,
    })
}

/// When this listing added the track. Playlist items carry one timestamp per
/// listing; zero or absent means the server never recorded one.
pub fn added_at(item: &playlist4_external::Item) -> Option<DateTime<Utc>> {
    let milliseconds = item.attributes.get_or_default().timestamp();
    if milliseconds <= 0 {
        return None;
    }
    Utc.timestamp_millis_opt(milliseconds).single()
}

/// Whether a playlist item URI names a plain track. Lineups can carry
/// episodes and other non-track entries, which Cadence's pages cannot show.
pub fn is_track_item(uri: &str) -> bool {
    uri.starts_with("spotify:track:")
}

/// The tracks a playlist lists, in order, each with its Date Added: the
/// items of a `playlist/v2` answer minus the episodes and other entries
/// Cadence's pages cannot show.
pub fn playlist_entries(
    content: &playlist4_external::SelectedListContent,
) -> Vec<(String, Option<DateTime<Utc>>)> {
    content
        .contents
        .items
        .iter()
        .filter(|item| is_track_item(item.uri()))
        .map(|item| (item.uri().to_owned(), added_at(item)))
        .collect()
}

/// The playlist artwork URL from the internal protocol's list attributes:
/// a ready-made URL when the server decorated the list with one, otherwise
/// the raw picture file id.
pub fn playlist_artwork(attributes: &playlist4_external::ListAttributes) -> Option<String> {
    attributes
        .picture_size
        .iter()
        .find(|picture| !picture.url().is_empty())
        .map(|picture| picture.url().to_owned())
        .or_else(|| {
            let picture = attributes.picture();
            if picture.is_empty() {
                return None;
            }
            file_url(FileId::from(picture))
        })
}

fn artist_ref(artist: &metadata::Artist) -> ArtistRef {
    let source_id = base62(artist.gid());
    ArtistRef {
        name: artist.name().to_owned(),
        spotify_uri: source_id
            .as_deref()
            .map(|id| format!("spotify:artist:{id}")),
        source_id,
    }
}

fn album_ref(album: &metadata::Album) -> AlbumRef {
    let source_id = base62(album.gid());
    AlbumRef {
        name: album.name().to_owned(),
        spotify_uri: source_id.as_deref().map(|id| format!("spotify:album:{id}")),
        source_id,
        artwork_url: cover_artwork(album),
    }
}

/// The best cover URL: smallest image at least the target size, falling
/// back to the largest known size, then to whatever image exists.
fn cover_artwork(album: &metadata::Album) -> Option<String> {
    let mut images = album.cover_group.get_or_default().image.clone();
    if images.is_empty() {
        images = album.cover.clone();
    }
    let size = |image: &metadata::Image| image.width.or(image.height);
    let sized = || {
        images
            .iter()
            .filter_map(|image| size(image).map(|pixels| (image, pixels)))
    };
    sized()
        .filter(|(_, pixels)| *pixels >= TARGET_ARTWORK_SIZE)
        .min_by_key(|(_, pixels)| *pixels)
        .or_else(|| sized().max_by_key(|(_, pixels)| *pixels))
        .map(|(image, _)| image)
        .or_else(|| images.first())
        .and_then(|image| file_url(FileId::from(image.file_id())))
}

fn file_url(file_id: FileId) -> Option<String> {
    let hexadecimal = file_id.to_base16().ok()?;
    Some(format!("https://i.scdn.co/image/{hexadecimal}"))
}

/// The base62 id for a raw gid; empty or malformed gids have none.
fn base62(gid: &[u8]) -> Option<String> {
    SpotifyId::from_raw(gid).ok()?.to_base62().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        added_at, album_ref, artist_ref, base62, cover_artwork, file_url, is_track_item,
        playlist_artwork, playlist_entries, track,
    };
    use crate::model::{AlbumRef, ArtistRef};
    use librespot::core::FileId;
    use librespot::protocol::metadata::{self, Image, ImageGroup};
    use librespot::protocol::playlist4_external::{self, ItemAttributes};

    /// A 16-byte gid; the internal protocol identifies items by raw bytes.
    const GID_A: [u8; 16] = [0x11; 16];
    const GID_B: [u8; 16] = [0x22; 16];

    fn file_url_of(gid: &[u8]) -> String {
        file_url(FileId::from(gid)).expect("a valid file id always converts")
    }

    fn track_message() -> metadata::Track {
        metadata::Track {
            gid: Some(GID_A.to_vec()),
            name: Some("Lineup Star".to_owned()),
            duration: Some(201_234),
            artist: vec![
                metadata::Artist {
                    gid: Some(GID_A.to_vec()),
                    name: Some("One".to_owned()),
                    ..Default::default()
                },
                metadata::Artist {
                    gid: Some(GID_B.to_vec()),
                    name: Some("Two".to_owned()),
                    ..Default::default()
                },
            ],
            external_id: vec![metadata::ExternalId {
                type_: Some("isrc".to_owned()),
                id: Some("USUM71703861".to_owned()),
                ..Default::default()
            }],
            album: Some(metadata::Album {
                gid: Some(GID_B.to_vec()),
                name: Some("Nightline".to_owned()),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn converts_a_full_track_message_into_the_domain_model() {
        let converted = track(&track_message()).expect("full metadata converts");

        assert_eq!(converted.provider, crate::model::Provider::Spotify);
        assert_eq!(converted.title, "Lineup Star");
        assert_eq!(converted.duration_ms, 201_234);
        assert_eq!(converted.isrc.as_deref(), Some("USUM71703861"));
        assert_eq!(converted.artist, "One, Two");
        assert_eq!(
            converted.artists,
            vec![
                ArtistRef {
                    name: "One".to_owned(),
                    source_id: base62(&GID_A),
                    spotify_uri: base62(&GID_A).map(|id| format!("spotify:artist:{id}")),
                },
                ArtistRef {
                    name: "Two".to_owned(),
                    source_id: base62(&GID_B),
                    spotify_uri: base62(&GID_B).map(|id| format!("spotify:artist:{id}")),
                },
            ]
        );
        assert_eq!(converted.album, "Nightline");
        assert_eq!(converted.album_ref.as_ref().unwrap().name, "Nightline");

        // Identity round-trips: the 22-character base62 form rebuilds the URI.
        assert_eq!(converted.source_id.len(), 22);
        assert_eq!(
            converted.spotify_uri.as_deref(),
            Some(format!("spotify:track:{}", converted.source_id)).as_deref()
        );
    }

    #[test]
    fn tracks_without_an_identity_are_rejected() {
        let anonymous = metadata::Track {
            name: Some("Ghost".to_owned()),
            ..Default::default()
        };
        assert!(track(&anonymous).is_err());
    }

    #[test]
    fn negative_durations_are_rejected() {
        let backwards = metadata::Track {
            gid: Some(GID_A.to_vec()),
            duration: Some(-1),
            ..Default::default()
        };
        assert!(track(&backwards).is_err());
    }

    #[test]
    fn added_at_reads_the_listing_timestamp_and_treats_zero_as_unknown() {
        let item = |timestamp: i64| playlist4_external::Item {
            attributes: Some(ItemAttributes {
                timestamp: Some(timestamp),
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };

        let dated = added_at(&item(1_693_580_800_000)).expect("a real timestamp converts");
        assert_eq!(dated.timestamp_millis(), 1_693_580_800_000);
        assert_eq!(added_at(&item(0)), None);
        assert_eq!(
            added_at(&playlist4_external::Item::default()),
            None,
            "missing attributes mean unknown"
        );
    }

    #[test]
    fn only_plain_tracks_pass_the_item_filter() {
        [
            ("spotify:track:0123456789ABCDEFGHIJKL", true),
            ("spotify:episode:0123456789ABCDEFGHIJKL", false),
            ("spotify:local:Artist:Album:Song:200", false),
            ("hm://playlist/v2/playlist/x/track/3", false),
            ("", false),
        ]
        .into_iter()
        .for_each(|(uri, expected)| assert_eq!(is_track_item(uri), expected, "{uri}"));
    }

    #[test]
    fn playlist_entries_keep_tracks_in_order_and_drop_the_rest() {
        let item = |uri: &str, timestamp: Option<i64>| playlist4_external::Item {
            uri: Some(uri.to_owned()),
            attributes: timestamp
                .map(|timestamp| ItemAttributes {
                    timestamp: Some(timestamp),
                    ..Default::default()
                })
                .into(),
            ..Default::default()
        };
        let content = playlist4_external::SelectedListContent {
            contents: Some(playlist4_external::ListItems {
                items: vec![
                    item("spotify:track:b", Some(2_000)),
                    item("spotify:episode:x", Some(3_000)),
                    item("spotify:track:a", None),
                ],
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };

        let entries = playlist_entries(&content);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "spotify:track:b");
        assert_eq!(entries[0].1.map(|at| at.timestamp_millis()), Some(2_000));
        assert_eq!(entries[1], ("spotify:track:a".to_owned(), None));
    }

    #[test]
    fn playlist_artwork_prefers_decorated_urls_then_the_picture_file_id() {
        let with_url = playlist4_external::ListAttributes {
            picture_size: vec![playlist4_external::PictureSize {
                target_name: Some("xl".to_owned()),
                url: Some("https://i.scdn.co/image/decorated".to_owned()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            playlist_artwork(&with_url).as_deref(),
            Some("https://i.scdn.co/image/decorated")
        );

        let with_bytes = playlist4_external::ListAttributes {
            picture: Some(vec![0xAB; 20]),
            ..Default::default()
        };
        let url = playlist_artwork(&with_bytes).expect("raw picture becomes a URL");
        assert_eq!(url, file_url_of(&[0xAB; 20]));

        assert_eq!(playlist_artwork(&Default::default()), None);
    }

    #[test]
    fn cover_artwork_picks_the_smallest_image_at_least_the_target_size() {
        let image = |gid: [u8; 16], width: Option<i32>| Image {
            file_id: Some(gid.to_vec()),
            width,
            ..Default::default()
        };
        let album = |covers: Vec<Image>| metadata::Album {
            cover_group: Some(ImageGroup {
                image: covers,
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };

        // 212px is under target, so the 640px cover wins despite being larger.
        let sized = album(vec![image(GID_B, Some(212)), image(GID_A, Some(640))]);
        assert_eq!(cover_artwork(&sized), Some(file_url_of(&GID_A)));

        // All candidates under target: the largest known size falls back in.
        let small = album(vec![image(GID_A, Some(64)), image(GID_B, Some(128))]);
        assert_eq!(cover_artwork(&small), Some(file_url_of(&GID_B)));

        // Sizes unknown entirely: whatever image exists is used.
        let unknown_size = album(vec![image(GID_A, None)]);
        assert_eq!(cover_artwork(&unknown_size), Some(file_url_of(&GID_A)));
    }

    #[test]
    fn albums_without_any_cover_convert_without_artwork() {
        let bare = metadata::Album::default();
        assert_eq!(cover_artwork(&bare), None);
        let converted = album_ref(&bare);
        assert_eq!(converted, AlbumRef::default());

        let named = metadata::Album {
            gid: Some(GID_B.to_vec()),
            name: Some("Bare".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            album_ref(&named),
            AlbumRef {
                name: "Bare".to_owned(),
                source_id: base62(&GID_B),
                spotify_uri: base62(&GID_B).map(|id| format!("spotify:album:{id}")),
                artwork_url: None,
            }
        );

        // The artist helper shares the identity rules.
        let artist = metadata::Artist {
            gid: Some(GID_B.to_vec()),
            name: Some("Solo".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            artist_ref(&artist),
            ArtistRef {
                name: "Solo".to_owned(),
                source_id: base62(&GID_B),
                spotify_uri: base62(&GID_B).map(|id| format!("spotify:artist:{id}")),
            }
        );
        assert_eq!(artist_ref(&metadata::Artist::default()).source_id, None);
    }

    #[test]
    fn empty_gids_yield_no_identity() {
        assert_eq!(base62(&[]), None);
    }
}
