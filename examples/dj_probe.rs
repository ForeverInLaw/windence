//! Live diagnostic probe for the DJ X lineup fetch (debug tooling).
//!
//! Connects a headless librespot session with the stored playback refresh
//! token and replays every step of `Playback::dj_lineup`, reporting which
//! branch produces its outcome. When the lineup comes back empty it also
//! walks the context skeleton (see go-librespot#352): following the
//! context's own hm:// url and each page's page_url / next_page_url to
//! wherever the tracks actually live. Run: `cargo run --example dj_probe`.
//!
//! Optional env overrides:
//!   DJ_PROBE_PLAYLIST_ID — base62 playlist id (default: the DJ X id)
//!   DJ_PROBE_TRACK_LIMIT — track metadata lookups after the list fetch

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures::StreamExt;
use librespot::core::{
    SpotifyId, SpotifyUri,
    authentication::Credentials,
    config::{DeviceType, SessionConfig},
    dealer::{
        manager::Reply,
        protocol::{Command, Message, PayloadValue},
    },
    session::Session,
};
use librespot::metadata::Metadata;
use librespot::oauth::OAuthClientBuilder;
use librespot::protocol::connect::{
    Capabilities, ClusterUpdate, Device, DeviceInfo, MemberType, PutStateReason, PutStateRequest,
};
use librespot::protocol::player::{ContextPlayerOptions, PlayOrigin, PlayerState, Suppressions};
use librespot::protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message as _, MessageField};

const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYCHAIN_SERVICE: &str = "com.cadence.spotify";
const KEYCHAIN_ACCOUNT: &str = "playback-refresh-token";

/// Prints a compact summary of a context/page JSON body: top-level keys,
/// every `spotify:` uri found anywhere in the payload, and a raw excerpt.
/// The probe reports shapes instead of parsing them; learning what the
/// server really sends back is the point of this tool.
fn summarize_body(label: &str, body: &[u8], uri_limit: usize) -> usize {
    let text = String::from_utf8_lossy(body);
    println!("[probe]     {label}: {} bytes", body.len());
    let parsed = serde_json::from_str::<serde_json::Value>(text.trim()).ok();
    let Some(value) = parsed else {
        println!(
            "[probe]       not json, head: {}",
            &text[..text.len().min(300)]
        );
        return 0;
    };
    if let Some(map) = value.as_object() {
        println!(
            "[probe]       keys: {:?}",
            map.keys().cloned().collect::<Vec<_>>()
        );
    }
    fn collect_uris<'a>(value: &'a serde_json::Value, uris: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(uri)) = map.get("uri")
                    && uri.starts_with("spotify:")
                {
                    uris.push(uri);
                }
                map.values().for_each(|child| collect_uris(child, uris));
            }
            serde_json::Value::Array(items) => {
                items.iter().for_each(|item| collect_uris(item, uris));
            }
            _ => {}
        }
    }
    let mut uris = Vec::new();
    collect_uris(&value, &mut uris);
    println!("[probe]       uris: total={}", uris.len());
    for uri in uris.iter().take(uri_limit) {
        println!("[probe]         {uri}");
    }
    println!("[probe]       raw head: {}", &text[..text.len().min(500)]);
    uris.len()
}

const PROBE_DEVICE_NAME: &str = "Cadence-Probe";

/// Prints a context skeleton and follows every resolvable pointer it offers,
/// reporting what each hop returns. Returns how many spotify uris surfaced.
async fn inspect_and_follow_context(session: &Session, dj_uri: &str, uri_limit: usize) -> usize {
    let sp = session.spclient();
    let context = match sp.get_context(dj_uri).await {
        Ok(context) => context,
        Err(error) => {
            println!("[probe]   get_context FAILED: {error}");
            return 0;
        }
    };
    println!(
        "[probe]   get_context OK: pages={} uri={:?} loading={} ctx_url={:?}",
        context.pages.len(),
        context.uri(),
        context.loading(),
        context.url(),
    );
    for (index, page) in context.pages.iter().enumerate() {
        println!(
            "[probe]     page {index}: tracks={} page_url={:?} next_page_url={:?}",
            page.tracks.len(),
            page.page_url(),
            page.next_page_url(),
        );
    }

    let mut found = context
        .pages
        .iter()
        .map(|page| page.tracks.len())
        .sum::<usize>();
    if context.url().starts_with("hm://") {
        match sp.get_next_page(context.url()).await {
            Ok(body) => found += summarize_body("context-url body", &body, uri_limit),
            Err(error) => println!("[probe]     context-url fetch FAILED: {error}"),
        }
    }
    for page in context.pages.iter().take(3) {
        for pointer in [page.page_url(), page.next_page_url()] {
            if !pointer.starts_with("hm://") {
                continue;
            }
            match sp.get_next_page(pointer).await {
                Ok(body) => {
                    found += summarize_body(&format!("page fetch {pointer}"), &body, uri_limit)
                }
                Err(error) => println!("[probe]     page fetch FAILED {pointer}: {error}"),
            }
        }
    }
    found
}

/// Reports a dealer player command and follows the context it delivers.
/// Returns whether track uris surfaced from the DJ context.
async fn report_request(
    session: &Session,
    command: Command,
    dj_uri: &str,
    uri_limit: usize,
) -> bool {
    println!("[probe]   COMMAND arrived: {command}");
    let sp = session.spclient();
    match command {
        Command::Play(play) => {
            let context = &play.context;
            println!(
                "[probe]     play context uri={:?} ctx_url={:?} pages={}",
                context.uri(),
                context.url(),
                context.pages.len(),
            );
            for (index, page) in context.pages.iter().enumerate() {
                println!(
                    "[probe]       page {index}: tracks={} page_url={:?} next_page_url={:?}",
                    page.tracks.len(),
                    page.page_url(),
                    page.next_page_url(),
                );
            }
            let mut found = context
                .pages
                .iter()
                .map(|page| page.tracks.len())
                .sum::<usize>();
            for page in context.pages.iter().take(5) {
                for pointer in [page.page_url(), page.next_page_url()] {
                    if !pointer.starts_with("hm://") {
                        continue;
                    }
                    match sp.get_next_page(pointer).await {
                        Ok(body) => {
                            found +=
                                summarize_body(&format!("command page {pointer}"), &body, uri_limit)
                        }
                        Err(error) => println!("[probe]       fetch FAILED: {error}"),
                    }
                }
            }
            found > 0 && context.uri().contains(dj_uri)
        }
        Command::Transfer(transfer) => {
            let Some(state) = transfer.data else {
                println!("[probe]     bare transfer without state");
                return false;
            };
            let session_state = state.current_session.get_or_default();
            let context = session_state.context.get_or_default();
            let tracks = context
                .pages
                .iter()
                .map(|page| page.tracks.len())
                .sum::<usize>();
            println!(
                "[probe]     transfer context uri={:?} url={:?} pages={} tracks={tracks}",
                context.uri,
                context.url,
                context.pages.len(),
            );
            for (index, page) in context.pages.iter().enumerate() {
                println!(
                    "[probe]       page {index}: tracks={} page_url={:?} next_page_url={:?}",
                    page.tracks.len(),
                    page.page_url,
                    page.next_page_url,
                );
            }
            for page in context.pages.iter().take(3) {
                let pointers = [page.page_url.as_deref(), page.next_page_url.as_deref()];
                for pointer in pointers.into_iter().flatten() {
                    if !pointer.starts_with("hm://") {
                        continue;
                    }
                    match sp.get_next_page(pointer).await {
                        Ok(body) => {
                            summarize_body(&format!("transfer page {pointer}"), &body, uri_limit);
                        }
                        Err(error) => println!("[probe]       fetch FAILED: {error}"),
                    }
                }
            }
            false
        }
        other => {
            println!("[probe]     not a play or transfer command; ignoring");
            let _ = other;
            false
        }
    }
}

/// Reports a cluster update, calling out DJ queue material when present.
fn report_cluster(bytes: &[u8], dj_uri: &str) {
    let Ok(update) = ClusterUpdate::parse_from_bytes(bytes) else {
        return;
    };
    let cluster = update.cluster.get_or_default();
    let player_state = cluster.player_state.get_or_default();
    println!(
        "[probe]   cluster update: active={:?} context={:?} next={} prev={}",
        cluster.active_device_id,
        player_state.context_uri,
        player_state.next_tracks.len(),
        player_state.prev_tracks.len(),
    );
    if !player_state.context_uri.contains(dj_uri) {
        return;
    }
    for track in player_state.next_tracks.iter().take(10) {
        println!(
            "[probe]     next uri={:?} canonical={:?}",
            track.uri,
            track.metadata.get("canonical_track_uri")
        );
    }
}

/// Registers the probe session as a Connect device named
/// [`PROBE_DEVICE_NAME`] and retries resolution from inside a registered
/// session: the plain context resolve, an active-device state naming the DJ
/// context, and a dealer listen window during which a handover from an
/// official client can be observed live. Returns how many track uris any
/// step surfaced.
async fn connect_device_stage(session: &Session, dj_uri: &str, uri_limit: usize) -> Result<usize> {
    println!("[probe] STAGE C: registering '{PROBE_DEVICE_NAME}' as a Connect device");

    // Subscriptions must exist before the socket comes up, or early
    // messages race them. Player commands are dealer REQUESTS that need a
    // reply: listening to them as messages sees nothing, and an unanswered
    // request leaves the sending client hanging in "connecting" until it
    // gives up.
    let mut commands = session
        .dealer()
        .handle_for("hm://connect-state/v1/player/command")?;
    let mut clusters =
        session
            .dealer()
            .listen_for(
                "hm://connect-state/v1/cluster",
                |message: Message| match message.payload {
                    PayloadValue::Raw(bytes) => Ok(bytes),
                    PayloadValue::Json(text) => Ok(text.into_bytes()),
                    PayloadValue::Empty => Ok(Vec::new()),
                },
            )?;
    let mut connection_ids =
        session
            .dealer()
            .listen_for("hm://pusher/v1/connections/", |message: Message| {
                Ok(message
                    .headers
                    .get("Spotify-Connection-Id")
                    .cloned()
                    .unwrap_or_default())
            })?;
    // Catch-all: every other dealer message gets logged, so silence in the
    // listen window means the network was silent, not that a subscription
    // missed its topic.
    let mut everything = session.dealer().listen_for("hm://", |message: Message| {
        let text = match message.payload {
            PayloadValue::Json(text) => text,
            PayloadValue::Raw(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            PayloadValue::Empty => String::new(),
        };
        Ok((message.uri, text))
    })?;

    session
        .dealer()
        .start()
        .await
        .context("could not start the dealer websocket")?;

    let connection_id = tokio::time::timeout(Duration::from_secs(15), connection_ids.next())
        .await
        .context("timed out waiting for the dealer connection id")?
        .transpose()?
        .filter(|id| !id.is_empty())
        .context("the dealer hello carried no connection id")?;
    session.set_connection_id(&connection_id);
    println!(
        "[probe]   dealer up; connection_id {}…",
        &connection_id[..connection_id.len().min(8)]
    );

    let device_info = DeviceInfo {
        can_play: true,
        name: PROBE_DEVICE_NAME.to_owned(),
        device_id: session.device_id().to_string(),
        device_type: EnumOrUnknown::new(DeviceType::Speaker.into()),
        device_software_version: format!("cadence-probe {}", env!("CARGO_PKG_VERSION")),
        spirc_version: "3.2.0".to_owned(),
        client_id: session.client_id(),
        capabilities: MessageField::some(Capabilities {
            can_be_player: true,
            is_observable: true,
            is_controllable: true,
            needs_full_player_state: true,
            supports_gzip_pushes: true,
            supports_playlist_v2: true,
            supports_transfer_command: true,
            supports_command_request: true,
            supports_set_options_command: true,
            command_acks: true,
            volume_steps: 64,
            supported_types: vec!["audio/track".to_owned(), "audio/episode".to_owned()],
            // The experiment: advertise what official clients advertise for
            // the AI DJ. librespot hardcodes this off.
            supports_dj: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    let player_state = PlayerState {
        session_id: session.session_id(),
        is_system_initiated: true,
        playback_speed: 1.,
        play_origin: MessageField::some(PlayOrigin::new()),
        suppressions: MessageField::some(Suppressions::new()),
        options: MessageField::some(ContextPlayerOptions::new()),
        ..Default::default()
    };

    let mut request = PutStateRequest {
        member_type: EnumOrUnknown::new(MemberType::SPIRC_V3),
        put_state_reason: EnumOrUnknown::new(PutStateReason::NEW_DEVICE),
        device: MessageField::some(Device {
            device_info: MessageField::some(device_info),
            player_state: MessageField::some(player_state),
            ..Default::default()
        }),
        ..Default::default()
    };

    match session.spclient().put_connect_state_request(&request).await {
        Ok(_) => println!("[probe]   NEW_DEVICE state PUT accepted"),
        Err(error) => println!("[probe]   NEW_DEVICE state PUT FAILED: {error}"),
    }

    println!("[probe]   resolving again as a registered device");
    let mut found = inspect_and_follow_context(session, dj_uri, uri_limit).await;

    println!("[probe]   resolving again as the active device naming the DJ context");
    request.is_active = true;
    request.put_state_reason = EnumOrUnknown::new(PutStateReason::PLAYER_STATE_CHANGED);
    request
        .device
        .mut_or_insert_default()
        .player_state
        .mut_or_insert_default()
        .context_uri = dj_uri.to_owned();
    match session.spclient().put_connect_state_request(&request).await {
        Ok(_) => println!("[probe]   ACTIVE state PUT accepted"),
        Err(error) => println!("[probe]   ACTIVE state PUT FAILED: {error}"),
    }
    found += inspect_and_follow_context(session, dj_uri, uri_limit).await;

    // Handover window: an official client starting DJ and casting here is the
    // one channel all reference implementations agree works.
    let listen_secs: u64 = std::env::var("DJ_PROBE_LISTEN_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .unwrap_or(25);
    println!("[probe]   listening {listen_secs}s for dealer traffic");
    println!("[probe]   HANDOVER TEST: start DJ X in your official Spotify app,");
    println!("[probe]   then pick '{PROBE_DEVICE_NAME}' as the playback device.");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            command = commands.next() => match command {
                Some((request, sender)) => {
                    // Ack before anything else so the sending client stops
                    // waiting in "connecting".
                    let _ = sender.send(Reply::Success);
                    if report_request(session, request.command, dj_uri, uri_limit).await {
                        found += 1;
                    }
                }
                None => {
                    println!("[probe]   command stream closed");
                    break;
                }
            },
            cluster = clusters.next() => match cluster {
                Some(Ok(bytes)) => report_cluster(&bytes, dj_uri),
                Some(Err(error)) => println!("[probe]   cluster stream error: {error}"),
                None => {
                    println!("[probe]   cluster stream closed");
                    break;
                }
            },
            traffic = everything.next() => match traffic {
                Some(Ok((uri, text))) => {
                    if uri.starts_with("hm://pusher/v1/connections/") {
                        continue;
                    }
                    println!(
                        "[probe]   dealer traffic on {uri}: {}",
                        &text[..text.len().min(200)]
                    );
                }
                Some(Err(error)) => println!("[probe]   dealer stream error: {error}"),
                None => {
                    println!("[probe]   dealer stream closed");
                    break;
                }
            },
        }
    }

    Ok(found)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let playlist_id = std::env::var("DJ_PROBE_PLAYLIST_ID")
        .unwrap_or_else(|_| spotify_gpui_client::dj::SOURCE_ID.to_owned());
    let track_limit: usize = std::env::var("DJ_PROBE_TRACK_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
        .unwrap_or(3);

    let refresh_token = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?
        .get_password()
        .context("no stored playback refresh token in the OS keychain")?;
    let oauth = OAuthClientBuilder::new(CLIENT_ID, REDIRECT_URI, vec!["streaming"])
        .build()
        .context("could not configure librespot authorization")?;
    let token = oauth
        .refresh_token_async(&refresh_token)
        .await
        .context("could not refresh the stored credentials")?;
    // Spotify rotates the refresh token here; the stored one dies unless the
    // replacement is persisted, exactly like the app's own connect flow.
    if !token.refresh_token.is_empty() {
        keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?
            .set_password(&token.refresh_token)
            .context("could not store the rotated refresh token")?;
    }
    let access_token = token.access_token;

    let session = Session::new(SessionConfig::default(), None);
    session
        .connect(Credentials::with_access_token(access_token), false)
        .await
        .context("could not connect to Spotify")?;
    println!(
        "[probe] connected; country={:?} canonical_name={:?}",
        session.country(),
        session.username()
    );

    let id = SpotifyId::from_base62(&playlist_id)
        .with_context(|| format!("invalid base62 playlist id {playlist_id}"))?;
    let uri = SpotifyUri::Playlist { user: None, id };
    println!("[probe] requesting {uri:?}");

    let response =
        <librespot::metadata::playlist::Playlist as Metadata>::request(&session, &uri).await;
    let bytes = match response {
        Ok(bytes) => {
            println!("[probe] list fetch OK ({} bytes)", bytes.len());
            bytes
        }
        Err(error) => {
            println!(
                "[probe] LIST FETCH FAILED: kind={:?} message=\"{error}\"",
                error.kind
            );
            return Err(anyhow!("list fetch failed"));
        }
    };

    let content = SelectedListContent::parse_from_bytes(&bytes)
        .context("could not parse the playlist protobuf")?;
    println!(
        "[probe] parsed: length={} revision={:?} attributes_name={:?}",
        content.length(),
        content.revision,
        content.attributes.get_or_default().name
    );
    let contents = content.contents.get_or_default();
    println!(
        "[probe] contents: items={} truncated={} has_diff={}",
        contents.items.len(),
        contents.truncated(),
        content.diff.is_some()
    );
    for item in contents.items.iter().take(track_limit) {
        println!("[probe]   item uri={}", item.uri());
    }
    if content.length() <= 0 || contents.items.is_empty() {
        println!("[probe] EMPTY LINEUP: trying alternate channels before giving up");

        // Stage B: anonymous skeleton walk (go-librespot#352). The empty
        // page may name where the tracks live via hm:// pointers — unless
        // materialization needs a registered Connect session.
        let dj_uri = format!("spotify:playlist:{playlist_id}");
        let mut found = inspect_and_follow_context(&session, &dj_uri, track_limit).await;

        match session
            .spclient()
            .get_apollo_station("tracks", &dj_uri, Some(30), Vec::new(), true)
            .await
        {
            Ok(body) => {
                let text = String::from_utf8_lossy(&body);
                let count = text.matches("\"uri\"").count();
                println!(
                    "[probe]   apollo station OK ({} bytes, ~{count} uris): {}",
                    body.len(),
                    &text[..text.len().min(400)]
                );
            }
            Err(error) => println!("[probe]   apollo station FAILED: {error}"),
        }

        // Stage C: register as a Connect device and retry from inside a
        // registered session, ending with a handover listen window.
        match connect_device_stage(&session, &dj_uri, track_limit).await {
            Ok(stage_found) if stage_found > 0 => found += stage_found,
            Ok(_) => {}
            Err(error) => println!("[probe]   stage C failed: {error:#}"),
        }

        if found > 0 {
            println!(
                "[probe] VERDICT: the Connect channel resolves the DJ lineup ({found} track uris seen)"
            );
            return Ok(());
        }

        return Err(anyhow!(
            "EMPTY LINEUP: dj_lineup would return NotOffered here"
        ));
    }

    for item in contents
        .items
        .iter()
        .filter(|item| item.uri().starts_with("spotify:track:"))
        .take(track_limit)
    {
        let track_uri = match SpotifyUri::from_uri(item.uri()) {
            Ok(track_uri) => track_uri,
            Err(error) => {
                println!("[probe]   unparseable uri {}: {error}", item.uri());
                continue;
            }
        };
        match <librespot::metadata::Track as Metadata>::request(&session, &track_uri).await {
            Ok(message) => {
                let parsed = librespot::protocol::metadata::Track::parse_from_bytes(&message);
                match parsed {
                    Ok(track) => println!(
                        "[probe]   track {} duration={}ms name={:?}",
                        item.uri(),
                        track.duration(),
                        track.name()
                    ),
                    Err(error) => println!("[probe]   track parse failed: {error}"),
                }
            }
            Err(error) => println!(
                "[probe]   TRACK FETCH FAILED uri={} kind={:?} message=\"{error}\"",
                item.uri(),
                error.kind
            ),
        }
    }

    println!(
        "[probe] VERDICT: list resolves with {} items; dj_lineup should return Fresh",
        contents.items.len()
    );
    Ok(())
}
