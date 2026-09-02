//! Spotify's Home feed, read from the internal GraphQL partner endpoint.
//!
//! The Web API has no feed of curated playlists: Discover Weekly, the Daily
//! Mixes, daylists and the editorial shelves only exist on the Home page the
//! desktop client draws. That page comes from a persisted GraphQL query on
//! `api-partner.spotify.com`, and this module speaks it: it sends the
//! operation name, the query hash and the variables, and flattens the answer
//! into the `Home*` model types. Nothing here is a documented interface, so
//! a failure surfaces as an error state rather than a fallback.

use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;

use crate::model::{self, HomeCard, HomeCardKind, HomeFeed, HomeShelf, HomeShelfPage};

const PATHFINDER_URL: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
/// The persisted-query hash of the Home page document. One document serves
/// two operations, `home` (the feed) and `homeSection` (one shelf's next
/// page); `operationName` picks between them. Read from the xpui bundle of
/// desktop client 1.2.98.301 and confirmed live on 2026-08-26. Spotify
/// makes no promise about it: a new Home query retires this hash, and the
/// endpoint then answers `PersistedQueryNotFound`.
const HOME_HASH: &str = "76243c78b0e20ecdbe41b794dec8cbe73f75e585b0a7201b8d2e84578412847a";
/// The client the request claims to be. The endpoint checks these two
/// headers; the desktop values are the ones known to pass.
const APP_PLATFORM: &str = "Win32_x86_64";
const APP_VERSION: &str = "1.2.98.301";
const ORIGIN: &str = "https://xpui.app.spotify.com";
const INTEGRATION: &str = "INTEGRATION_DESKTOP";
/// The language shelf titles arrive in. Cadence has no language setting, so
/// this matches its own chrome.
const LOCALE: &str = "en";
/// Cards per shelf on the first load, the desktop default. The endpoint
/// accepts 10 to 40.
const SECTION_ITEMS_LIMIT: u32 = 10;
/// Cards per load-more page, the size the desktop's "see all" asks for.
const SHELF_PAGE_LIMIT: u32 = 20;

/// The partner endpoint client for one signed-in session.
///
/// Built per request rather than kept: the access token rotates, and the
/// session is the only place that knows the current one.
pub struct Pathfinder {
    http: oauth2_reqwest::Client,
    access_token: String,
    client_token: String,
    /// The listener's IANA zone. The greeting and the time-of-day shelves
    /// ("Soundtrack your Wednesday afternoon") follow it.
    time_zone: String,
}

impl Pathfinder {
    pub fn new(http: oauth2_reqwest::Client, access_token: String, client_token: String) -> Self {
        Self {
            http,
            access_token,
            client_token,
            time_zone: local_time_zone(),
        }
    }

    /// The feed: every shelf with its first page of cards.
    pub async fn home(&self) -> Result<HomeFeed> {
        let variables = serde_json::json!({
            "homeEndUserIntegration": INTEGRATION,
            "timeZone": self.time_zone,
            "sp_t": "",
            "facet": "",
            "sectionItemsLimit": SECTION_ITEMS_LIMIT,
            "includeEpisodeContentRatingsV2": true,
        });
        let body = self.query("home", HOME_HASH, variables).await?;
        parse_home(&body)
    }

    /// The next page of one shelf's cards from `offset`. `shelf_uri` is the
    /// shelf's own uri from the feed.
    pub async fn home_shelf(&self, shelf_uri: &str, offset: u32) -> Result<HomeShelfPage> {
        let variables = serde_json::json!({
            "uri": shelf_uri,
            "homeEndUserIntegration": INTEGRATION,
            "timeZone": self.time_zone,
            "sp_t": "",
            "sectionItemsOffset": offset,
            "sectionItemsLimit": SHELF_PAGE_LIMIT,
            "includeEpisodeContentRatingsV2": true,
        });
        let body = self.query("homeSection", HOME_HASH, variables).await?;
        parse_home_shelf(&body)
    }

    /// Runs one persisted operation of the document `hash` names and returns
    /// the raw answer. Every page of the partner endpoint (Home today, Browse
    /// when it comes) is one more call to this.
    async fn query(
        &self,
        operation: &str,
        hash: &str,
        variables: serde_json::Value,
    ) -> Result<Vec<u8>> {
        let body = serde_json::to_vec(&persisted_query(operation, hash, variables))
            .context("could not encode the Spotify Home request")?;
        let response = self
            .http
            .post(PATHFINDER_URL)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Client-Token", &self.client_token)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json;charset=UTF-8")
            .header("app-platform", APP_PLATFORM)
            .header("spotify-app-version", APP_VERSION)
            .header("accept-language", LOCALE)
            .header("origin", ORIGIN)
            .header("referer", format!("{ORIGIN}/"))
            .body(body)
            .send()
            .await
            .with_context(|| format!("Spotify {operation} request failed"))?;
        let status = response.status();
        if status == oauth2_reqwest::StatusCode::TOO_MANY_REQUESTS {
            // The partner endpoint sits outside the Web API's rate-limit
            // gate, so its 429 is read here. Nothing retries: the page shows
            // the wait and the listener comes back.
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1);
            return Err(anyhow!(
                "Spotify is rate limiting the Home feed; retry in {retry_after}s"
            ));
        }
        if !status.is_success() {
            return Err(anyhow!("Spotify {operation} request answered {status}"));
        }
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Spotify {operation} response was cut short"))?;
        Ok(bytes.to_vec())
    }
}

/// The request body of one operation: the variables, the operation name and
/// the hash of the document it lives in. The query text never leaves Spotify.
fn persisted_query(operation: &str, hash: &str, variables: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "variables": variables,
        "operationName": operation,
        "extensions": {
            "persistedQuery": { "version": 1, "sha256Hash": hash }
        }
    })
}

/// The machine's IANA zone name, or UTC when Windows reports a zone the
/// IANA table has no entry for. The zone only shapes the greeting and the
/// time-of-day shelves, so a wrong one is cosmetic.
fn local_time_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned())
}

// The response shape, mirrored only as deep as the model reads it. Every
// field is optional: shelves of different types fill different keys, and a
// key Spotify drops must cost a card, not the whole feed.

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GraphqlError>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Data {
    home: Option<Home>,
    home_sections: Option<HomeSections>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Home {
    greeting: Option<Label>,
    section_container: Option<SectionContainer>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HomeSections {
    sections: Vec<Section>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Label {
    transformed_label: Option<String>,
}

impl Label {
    /// The label's text, with Spotify's empty placeholders read as absent.
    fn text(self) -> Option<String> {
        self.transformed_label
            .filter(|text| !text.trim().is_empty())
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SectionContainer {
    sections: Page<Section>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Page<T> {
    items: Vec<T>,
    total_count: Option<u32>,
    paging_info: Option<PagingInfo>,
}

impl<T> Page<T> {
    /// Where the next page starts. Spotify sends `0` as well as `null` for
    /// a finished shelf; an offset of zero would only fetch the first page
    /// again, so it reads as done.
    fn next_offset(&self) -> Option<u32> {
        self.paging_info
            .as_ref()
            .and_then(|paging| paging.next_offset)
            .filter(|&offset| offset > 0)
    }
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PagingInfo {
    next_offset: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Section {
    uri: Option<String>,
    data: SectionData,
    section_items: Page<Card>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SectionData {
    title: Option<Label>,
    subtitle: Option<Label>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Card {
    uri: Option<String>,
    content: Option<Content>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Content {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    data: Option<ContentData>,
}

/// The union of what a playlist, album, artist and podcast card carry;
/// each kind fills its own subset.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ContentData {
    uri: Option<String>,
    name: Option<String>,
    owner_v2: Option<Wrapped<Named>>,
    content: Option<Counted>,
    attributes: Vec<Attribute>,
    /// Playlist covers.
    images: Option<Page<Image>>,
    /// Album, podcast and episode covers.
    cover_art: Option<Image>,
    /// An artist's name.
    profile: Option<Named>,
    /// An artist's picture.
    visuals: Option<Visuals>,
    /// An album's artists.
    artists: Option<Page<ArtistEntry>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Wrapped<T> {
    data: Option<T>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Named {
    name: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Counted {
    total_count: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Attribute {
    key: String,
    value: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Image {
    sources: Vec<Source>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Source {
    url: String,
    width: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Visuals {
    avatar_image: Option<Image>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ArtistEntry {
    profile: Option<Named>,
}

/// Reads an envelope, naming the GraphQL error when the answer carries one
/// instead of data. A retired query hash arrives this way.
fn parse_envelope(body: &[u8]) -> Result<Data> {
    let envelope: Envelope =
        serde_json::from_slice(body).context("Spotify Home feed returned invalid JSON")?;
    match envelope.data {
        Some(data) => Ok(data),
        None => Err(match envelope.errors.first() {
            Some(error) => anyhow!("Spotify Home feed refused: {}", error.message),
            None => anyhow!("Spotify Home feed returned no data"),
        }),
    }
}

/// Flattens a `home` answer into the feed.
pub fn parse_home(body: &[u8]) -> Result<HomeFeed> {
    let home = parse_envelope(body)?
        .home
        .context("Spotify Home feed returned no home page")?;
    let sections = home
        .section_container
        .map(|container| container.sections.items)
        .unwrap_or_default();
    Ok(HomeFeed {
        greeting: home.greeting.and_then(Label::text),
        shelves: sections.into_iter().map(shelf).collect(),
    })
}

/// Flattens a `homeSection` answer into the shelf's next page.
pub fn parse_home_shelf(body: &[u8]) -> Result<HomeShelfPage> {
    let section = parse_envelope(body)?
        .home_sections
        .and_then(|sections| sections.sections.into_iter().next())
        .context("Spotify Home feed returned no shelf")?;
    Ok(HomeShelfPage {
        next_offset: section.section_items.next_offset(),
        cards: cards(section.section_items.items),
    })
}

/// A wire section as the model's shelf.
fn shelf(section: Section) -> HomeShelf {
    // A shelf without a heading (the Shorts row) has no title key at all;
    // the subtitle is the next best label when Spotify fills it.
    let title = section
        .data
        .title
        .and_then(Label::text)
        .or_else(|| section.data.subtitle.and_then(Label::text));
    HomeShelf {
        uri: section.uri,
        title,
        next_offset: section.section_items.next_offset(),
        cards: cards(section.section_items.items),
    }
}

/// The cards of one shelf, minus the placeholders: a card whose content has
/// no data (the `UnknownType` entries) points nowhere Cadence can go.
fn cards(cards: Vec<Card>) -> Vec<HomeCard> {
    cards.into_iter().filter_map(card).collect()
}

fn card(card: Card) -> Option<HomeCard> {
    let content = card.content?;
    let data = content.data?;
    let uri = data.uri.or(card.uri)?;
    let kind = match content.typename.as_deref() {
        Some("PlaylistResponseWrapper") => HomeCardKind::Playlist,
        Some("AlbumResponseWrapper") => HomeCardKind::Album,
        Some("ArtistResponseWrapper") => HomeCardKind::Artist,
        _ => HomeCardKind::Other,
    };
    let name = data
        .name
        .or_else(|| data.profile.and_then(|profile| profile.name))
        .unwrap_or_default();
    let owner = data
        .owner_v2
        .and_then(|owner| owner.data)
        .and_then(|owner| owner.name)
        .or_else(|| {
            let artists: Vec<String> = data
                .artists?
                .items
                .into_iter()
                .filter_map(|artist| artist.profile?.name)
                .collect();
            (!artists.is_empty()).then(|| artists.join(", "))
        });
    let artwork = data
        .images
        .and_then(|images| images.items.into_iter().next())
        .or(data.cover_art)
        .or(data.visuals.and_then(|visuals| visuals.avatar_image));
    Some(HomeCard {
        uri,
        name,
        kind,
        owner,
        track_count: data.content.and_then(|content| content.total_count),
        artwork_url: artwork.and_then(|image| {
            model::pick_artwork(
                image
                    .sources
                    .iter()
                    .map(|source| (source.url.as_str(), source.width)),
            )
        }),
        made_for: data
            .attributes
            .into_iter()
            .find(|attribute| attribute.key == "madeFor.username")
            .map(|attribute| attribute.value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorded `home` answer, cut down to roughly the keys the parser
    /// reads. It is one account's page: its own playlists sit in the
    /// recently played shelf, and personal shelves carry its display name.
    /// The account ids were stripped.
    const HOME: &[u8] = include_bytes!("../tests/fixtures/home_feed.json");
    /// A recorded `homeSection` answer for the daylist shelf.
    const HOME_SECTION: &[u8] = include_bytes!("../tests/fixtures/home_section.json");

    #[test]
    fn home_parses_every_shelf_with_its_first_page() {
        let feed = parse_home(HOME).unwrap();

        assert_eq!(feed.greeting.as_deref(), Some("Добрый день"));
        assert_eq!(feed.shelves.len(), 31);

        let daylist = &feed.shelves[1];
        assert_eq!(
            daylist.title.as_deref(),
            Some("Soundtrack your Wednesday afternoon")
        );
        assert_eq!(daylist.next_offset, None);
        let card = &daylist.cards[0];
        assert_eq!(card.uri, "spotify:playlist:37i9dQZF1EP6YuccBxUcC1");
        assert_eq!(card.source_id(), "37i9dQZF1EP6YuccBxUcC1");
        assert_eq!(card.kind, HomeCardKind::Playlist);
        assert_eq!(card.owner.as_deref(), Some("Spotify"));
        assert_eq!(card.track_count, Some(50));
        assert_eq!(card.made_for.as_deref(), Some("ForeverInLaw"));
        assert!(card.artwork_url.is_some());
    }

    #[test]
    fn a_shelf_with_more_cards_carries_where_the_next_page_starts() {
        let feed = parse_home(HOME).unwrap();
        let shorts = &feed.shelves[0];

        assert_eq!(shorts.next_offset, Some(10));
        // Spotify draws this shelf without a heading and sends no title key.
        assert_eq!(shorts.title, None);
        // The playable playlists stay; only the placeholder is dropped.
        assert_eq!(shorts.cards.len(), 9);
    }

    #[test]
    fn a_zero_next_offset_reads_as_a_finished_shelf() {
        let feed = parse_home(HOME).unwrap();
        // Recorded with `nextOffset: 0` next to `totalCount: 25`.
        assert_eq!(feed.shelves[28].next_offset, None);
    }

    #[test]
    fn cards_keep_their_kind_and_placeholders_are_dropped() {
        let feed = parse_home(HOME).unwrap();
        let cards: Vec<&HomeCard> = feed.shelves.iter().flat_map(|s| &s.cards).collect();
        let count = |kind| cards.iter().filter(|card| card.kind == kind).count();

        // 272 recorded cards, one of them the `UnknownType` liked-songs tile.
        assert_eq!(cards.len(), 271);
        assert_eq!(count(HomeCardKind::Playlist), 200);
        assert_eq!(count(HomeCardKind::Album), 28);
        assert_eq!(count(HomeCardKind::Artist), 22);
        assert_eq!(count(HomeCardKind::Other), 21);
        assert!(cards.iter().all(|card| !card.uri.is_empty()));
    }

    #[test]
    fn albums_and_artists_map_to_their_pages() {
        let feed = parse_home(HOME).unwrap();
        let cards: Vec<&HomeCard> = feed.shelves.iter().flat_map(|s| &s.cards).collect();

        let album = cards
            .iter()
            .find(|card| card.kind == HomeCardKind::Album)
            .unwrap();
        assert!(album.uri.starts_with("spotify:album:"));
        assert!(!album.name.is_empty());
        assert!(album.owner.is_some(), "album cards name their artists");
        assert!(album.artwork_url.is_some());
        assert_eq!(
            album.album().unwrap().source_id.as_deref(),
            Some(album.source_id())
        );
        assert_eq!(album.playlist(), None);

        let artist = cards
            .iter()
            .find(|card| card.kind == HomeCardKind::Artist)
            .unwrap();
        assert!(artist.uri.starts_with("spotify:artist:"));
        assert!(!artist.name.is_empty());
        assert!(artist.artwork_url.is_some());
        assert_eq!(artist.artist().unwrap().name, artist.name);
    }

    #[test]
    fn a_playlist_card_opens_as_the_playlist_the_catalog_loads() {
        let feed = parse_home(HOME).unwrap();
        let playlist = feed.shelves[1].cards[0].playlist().unwrap();

        assert_eq!(playlist.source_id, "37i9dQZF1EP6YuccBxUcC1");
        assert_eq!(playlist.owner, "Spotify");
        assert_eq!(playlist.track_count, 50);
    }

    #[test]
    fn home_shelf_parses_into_the_shelf_page() {
        let page = parse_home_shelf(HOME_SECTION).unwrap();

        assert_eq!(page.cards.len(), 10);
        assert_eq!(page.next_offset, None);
        assert_eq!(page.cards[0].uri, "spotify:playlist:37i9dQZF1EP6YuccBxUcC1");
        assert_eq!(page.cards[0].kind, HomeCardKind::Playlist);
        assert_eq!(page.cards[0].made_for.as_deref(), Some("ForeverInLaw"));
    }

    #[test]
    fn a_refused_query_names_the_graphql_error() {
        let body = br#"{"errors":[{"message":"PersistedQueryNotFound"}],"data":null}"#;

        let error = parse_home(body).unwrap_err().to_string();

        assert!(error.contains("PersistedQueryNotFound"), "{error}");
        assert!(parse_home_shelf(body).is_err());
    }

    #[test]
    fn an_answer_without_a_home_page_is_an_error_not_an_empty_feed() {
        assert!(parse_home(br#"{"data":{}}"#).is_err());
        assert!(parse_home_shelf(br#"{"data":{"homeSections":{"sections":[]}}}"#).is_err());
    }
}
