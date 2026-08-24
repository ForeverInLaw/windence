use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use async_channel as async_chan;
use futures::StreamExt as _;
use keyring::Entry;
use librespot::{
    core::{
        SpotifyUri,
        authentication::Credentials,
        config::{DeviceType, SessionConfig},
        dealer::{
            manager::Reply,
            protocol::{Command, Message, PayloadValue, Request},
        },
        session::Session,
    },
    metadata::Metadata,
    oauth::OAuthClientBuilder,
    playback::{
        config::{AudioFormat, PlayerConfig, VolumeCtrl},
        mixer::{self, Mixer, MixerConfig},
        player::{Player, PlayerEvent, PlayerEventChannel},
    },
    protocol::{
        connect::{Capabilities, Device, DeviceInfo, MemberType, PutStateReason, PutStateRequest},
        player::{
            ContextIndex, ContextPlayerOptions, PlayOrigin, PlayerState, ProvidedTrack,
            Restrictions, Suppressions,
        },
    },
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use protobuf::{EnumOrUnknown, MessageField};
use tokio::net::TcpListener;

use crate::{
    audio::low_latency_sdl_sink,
    credential_worker, dj, model,
    oauth_callback::receive_callback,
    oauth_page::{OAuthStep, success_page},
    proto_convert,
};

const PLAYBACK_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYCHAIN_SERVICE: &str = "com.cadence.spotify";
const KEYCHAIN_ACCOUNT: &str = "playback-refresh-token";
const LOGGED_OUT_CREDENTIAL: &str = "cadence-logged-out";
/// How many internal-protocol track lookups overlap when resolving the DJ
/// lineup: a full page resolves well inside the catalog timeout without
/// bursting one access point.
const DJ_TRACK_CONCURRENCY: usize = 4;
/// The device name Cadence presents as on Spotify Connect. Casts target it.
const DJ_DEVICE_NAME: &str = "Cadence";

type PlaybackOAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

pub(crate) struct PlaybackAuthorization {
    url: String,
    csrf_token: CsrfToken,
    pkce_verifier: PkceCodeVerifier,
    listener: TcpListener,
}

impl PlaybackAuthorization {
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    async fn authorize(self) -> Result<PlaybackOAuthToken> {
        let mut callback = receive_callback(&self.listener, "/login")
            .await
            .context("could not receive the Spotify playback callback")?;
        let parameter = |name| {
            callback
                .url()
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        if let Some(error) = parameter("error") {
            return Err(anyhow!("Spotify playback authorization failed: {error}"));
        }
        let state = parameter("state").context("Spotify playback callback omitted state")?;
        if state != self.csrf_token.secret().as_str() {
            return Err(anyhow!("Spotify playback callback state did not match"));
        }
        let code = parameter("code").context("Spotify playback callback omitted code")?;

        let http_client = oauth2_reqwest::ClientBuilder::new()
            .redirect(oauth2_reqwest::redirect::Policy::none())
            .build()?;
        let response = playback_oauth_client()?
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(self.pkce_verifier)
            .request_async(&http_client)
            .await
            .map_err(|error| anyhow!("could not exchange Spotify playback code: {error}"))?;
        let body = success_page(OAuthStep::Playback, None);
        callback
            .respond_html(&body)
            .await
            .context("could not write the Spotify playback callback response")?;
        Ok(PlaybackOAuthToken {
            access_token: response.access_token().secret().to_owned(),
            refresh_token: response
                .refresh_token()
                .map(|token| token.secret().to_owned())
                .unwrap_or_default(),
        })
    }
}

#[derive(Clone)]
pub struct Playback {
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,
    session: Session,
    /// Delivers lineups materialized by live DJ sessions (ADR 0004): the
    /// background service sends one per accepted handover.
    pub(crate) dj_lineups: async_chan::Receiver<dj::Lineup>,
    /// Delivers what the handover service started playing, so the player
    /// bar adopts the DJ context like any other play request.
    pub(crate) dj_now_playing: async_chan::Receiver<dj::DjNowPlaying>,
}

struct PlaybackOAuthToken {
    access_token: String,
    refresh_token: String,
}

impl Playback {
    pub(crate) async fn prepare_authorization() -> Result<PlaybackAuthorization> {
        let listener = TcpListener::bind("127.0.0.1:8898")
            .await
            .context("could not listen for the Spotify playback callback")?;
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf_token) = playback_oauth_client()?
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("streaming".to_owned()))
            .set_pkce_challenge(pkce_challenge)
            .url();
        Ok(PlaybackAuthorization {
            url: url.to_string(),
            csrf_token,
            pkce_verifier,
            listener,
        })
    }

    pub(crate) async fn connect(
        load_saved_token: bool,
        authorization: Option<PlaybackAuthorization>,
    ) -> Result<Self> {
        let callback_page = success_page(OAuthStep::Playback, None);
        let oauth =
            OAuthClientBuilder::new(PLAYBACK_CLIENT_ID, PLAYBACK_REDIRECT_URI, vec!["streaming"])
                .open_in_browser()
                .with_custom_message(&callback_page)
                .build()
                .context("could not configure librespot authorization")?;
        let saved_refresh_token = if load_saved_token {
            load_playback_refresh_token().await?
        } else {
            None
        };
        let token = match saved_refresh_token {
            Some(refresh_token) => {
                let mut token = match oauth.refresh_token_async(&refresh_token).await {
                    Ok(token) => token,
                    Err(_) => match authorization {
                        Some(authorization) => {
                            let token = authorization.authorize().await?;
                            return Self::connect_with_oauth_token(token).await;
                        }
                        None => oauth
                            .get_access_token_async()
                            .await
                            .context("could not reauthorize librespot playback")?,
                    },
                };
                if token.refresh_token.is_empty() {
                    token.refresh_token = refresh_token;
                }
                token
            }
            None => match authorization {
                Some(authorization) => {
                    let token = authorization.authorize().await?;
                    return Self::connect_with_oauth_token(token).await;
                }
                None => oauth
                    .get_access_token_async()
                    .await
                    .context("could not authorize librespot playback")?,
            },
        };
        if token.refresh_token.is_empty() {
            return Err(anyhow!("Spotify returned an empty playback refresh token"));
        }
        let access_token = token.access_token;
        persist_playback_refresh_token(token.refresh_token).await?;
        Self::connect_with_access_token(access_token).await
    }

    pub async fn reconnect() -> Result<Self> {
        let refresh_token = load_playback_refresh_token()
            .await?
            .context("Spotify playback credentials are not available")?;
        let oauth =
            OAuthClientBuilder::new(PLAYBACK_CLIENT_ID, PLAYBACK_REDIRECT_URI, vec!["streaming"])
                .build()
                .context("could not configure librespot reconnection")?;
        let mut token = oauth
            .refresh_token_async(&refresh_token)
            .await
            .context("could not refresh librespot playback credentials")?;
        if token.refresh_token.is_empty() {
            token.refresh_token = refresh_token;
        }
        let access_token = token.access_token;
        persist_playback_refresh_token(token.refresh_token).await?;
        Self::connect_with_access_token(access_token).await
    }

    async fn connect_with_oauth_token(token: PlaybackOAuthToken) -> Result<Self> {
        let PlaybackOAuthToken {
            access_token,
            refresh_token,
        } = token;
        validate_refresh_token(&refresh_token)?;
        persist_playback_refresh_token(refresh_token).await?;
        Self::connect_with_access_token(access_token).await
    }

    async fn connect_with_access_token(access_token: String) -> Result<Self> {
        let session = Session::new(SessionConfig::default(), None);
        session
            .connect(Credentials::with_access_token(access_token), false)
            .await
            .context("librespot could not connect to Spotify")?;
        let mixer = mixer::find(None).context("no supported audio mixer is available")?(
            MixerConfig::default(),
        )?;
        let volume = mixer.get_soft_volume();
        let player_config = PlayerConfig {
            position_update_interval: Some(Duration::from_millis(250)),
            ..PlayerConfig::default()
        };
        let player = Player::new(player_config, session.clone(), volume, move || {
            low_latency_sdl_sink(None, AudioFormat::default())
        });
        let (dj_sender, dj_lineups) = async_chan::unbounded();
        let (dj_notice_sender, dj_now_playing) = async_chan::unbounded();
        tokio::spawn(run_dj_service(
            session.clone(),
            player.clone(),
            dj_sender,
            dj_notice_sender,
        ));
        Ok(Self {
            player,
            mixer,
            session,
            dj_lineups,
            dj_now_playing,
        })
    }

    pub fn load(&self, spotify_uri: SpotifyUri, playing: bool, position_ms: u32) {
        self.player.load(spotify_uri, playing, position_ms);
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn seek(&self, position_ms: u32) {
        self.player.seek(position_ms);
    }

    pub fn set_volume(&self, volume: f32) {
        self.mixer
            .set_volume((volume.clamp(0., 1.) * f32::from(VolumeCtrl::MAX_VOLUME)) as u16);
    }

    pub fn stop(&self) {
        self.player.stop();
    }

    pub fn events(&self) -> PlayerEventChannel {
        self.player.get_player_event_channel()
    }

    pub fn is_connected(&self) -> bool {
        !self.session.is_invalid()
    }

    pub async fn radio_track_uris(&self, seed_uri: &str) -> Result<Vec<String>> {
        let seed = SpotifyUri::from_uri(seed_uri).context("invalid radio seed track URI")?;
        let response = self
            .session
            .spclient()
            .get_apollo_station("tracks", &seed.to_uri()?, Some(30), Vec::new(), true)
            .await
            .context("Spotify track radio endpoint failed")?;
        extract_track_uris(&response).context("Spotify track radio returned invalid JSON")
    }

    /// Returns the freshest DJ lineup the handover service has received, if
    /// any. The lineup is materialized by a live session (ADR 0004): until a
    /// cast happens there is nothing to show, and the honest empty state
    /// covers that case.
    pub fn dj_lineup(&self) -> dj::Lineup {
        match self.dj_lineups.try_recv() {
            Ok(lineup) => lineup,
            Err(_) => dj::Lineup::NotOffered,
        }
    }

    /// Loads a track by uri straight into the player: how the service starts
    /// playback for an accepted DJ handover without round-tripping through
    /// the app layer.
    pub fn load_uri(&self, spotify_uri: SpotifyUri, playing: bool, position_ms: u32) {
        self.player.load(spotify_uri, playing, position_ms);
    }

    /// Receives what the handover service started playing, for bridging
    /// into backend events.
    pub(crate) fn dj_now_playings(&self) -> async_chan::Receiver<dj::DjNowPlaying> {
        self.dj_now_playing.clone()
    }
}

/// How long the service waits for each dealer handshake step.
const DJ_DEALER_TIMEOUT: Duration = Duration::from_secs(15);

/// Registers Cadence as a `CONNECT_STATE` device on the live session and
/// watches dealer commands for DJ handovers (ADR 0004). A handover carries
/// the session-bound lexicon url; fetching it yields the materialized lineup,
/// which is sent to [`Playback::dj_lineup`] consumers while the first track
/// loads into the player — completing the cast handshake with real audio.
///
/// The task exits when Spotify closes the dealer socket, which happens as
/// soon as the same device id reconnects elsewhere in the app.
async fn run_dj_service(
    session: Session,
    player: Arc<Player>,
    events: async_chan::Sender<dj::Lineup>,
    notices: async_chan::Sender<dj::DjNowPlaying>,
) {
    // Subscriptions must exist before the socket comes up, or early messages
    // race them.
    let mut commands = match session
        .dealer()
        .handle_for("hm://connect-state/v1/player/command")
    {
        Ok(commands) => commands,
        Err(error) => {
            log::warn!("dj service: dealer handle unavailable: {error}");
            return;
        }
    };
    let mut connection_ids =
        match session
            .dealer()
            .listen_for("hm://pusher/v1/connections/", |message| {
                Ok(message
                    .headers
                    .get("Spotify-Connection-Id")
                    .cloned()
                    .unwrap_or_default())
            }) {
            Ok(ids) => ids,
            Err(error) => {
                log::warn!("dj service: cannot watch connections: {error}");
                return;
            }
        };
    // Cluster broadcasts show how real devices publish their state; casting
    // to an official client next to a cast to Cadence gives a diffable pair.
    let mut clusters =
        match session
            .dealer()
            .listen_for(
                "hm://connect-state/v1/cluster",
                |message: Message| match message.payload {
                    PayloadValue::Raw(bytes) => Ok(bytes),
                    PayloadValue::Json(text) => Ok(text.into_bytes()),
                    PayloadValue::Empty => Ok(Vec::new()),
                },
            ) {
            Ok(clusters) => clusters,
            Err(error) => {
                log::warn!("dj service: cannot watch clusters: {error}");
                return;
            }
        };

    if let Err(error) = session.dealer().start().await {
        log::warn!("dj service: dealer websocket failed: {error}");
        return;
    }
    let connection_id = match tokio::time::timeout(DJ_DEALER_TIMEOUT, connection_ids.next()).await {
        Ok(Some(Ok(id))) if !id.is_empty() => id,
        _ => {
            log::warn!("dj service: no connection id from the dealer hello");
            return;
        }
    };
    session.set_connection_id(&connection_id);

    let device_info = DeviceInfo {
        can_play: true,
        volume: 65535,
        name: DJ_DEVICE_NAME.to_owned(),
        device_id: session.device_id().to_string(),
        device_type: EnumOrUnknown::new(DeviceType::Speaker.into()),
        device_software_version: format!("cadence {}", env!("CARGO_PKG_VERSION")),
        spirc_version: "3.2.6".to_owned(),
        client_id: session.client_id(),
        brand: "spotify".to_owned(),
        model: "cadence".to_owned(),
        license: "premium".to_owned(),
        capabilities: MessageField::some(Capabilities {
            can_be_player: true,
            gaia_eq_connect_id: true,
            is_observable: true,
            volume_steps: 64,
            supported_types: vec![
                "audio/track".to_owned(),
                "audio/episode".to_owned(),
                "audio/media".to_owned(),
            ],
            command_acks: true,
            supports_playlist_v2: true,
            is_controllable: true,
            supports_transfer_command: true,
            supports_command_request: true,
            supports_gzip_pushes: true,
            supports_set_options_command: true,
            supports_dj: true,
            ..Default::default()
        }),
        metadata_map: std::collections::HashMap::from([("tier1_port".to_owned(), "0".to_owned())]),
        ..Default::default()
    };
    let request = PutStateRequest {
        client_side_timestamp: now_millis(),
        member_type: EnumOrUnknown::new(MemberType::CONNECT_STATE),
        put_state_reason: EnumOrUnknown::new(PutStateReason::NEW_DEVICE),
        device: MessageField::some(Device {
            device_info: MessageField::some(device_info.clone()),
            player_state: MessageField::some(PlayerState {
                session_id: session.session_id(),
                is_system_initiated: true,
                playback_speed: 1.,
                play_origin: MessageField::some(PlayOrigin::new()),
                suppressions: MessageField::some(Suppressions::new()),
                options: MessageField::some(ContextPlayerOptions::new()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    if let Err(error) = session.spclient().put_connect_state_request(&request).await {
        log::warn!("dj service: connect-state registration failed: {error}");
        return;
    }
    log::info!("dj service: registered as '{DJ_DEVICE_NAME}' on Spotify Connect");

    let mut player_events = player.get_player_event_channel();
    let mut state_ticker = tokio::time::interval(Duration::from_secs(5));
    state_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut active: Option<DjActive> = None;
    let mut reported_devices: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            Some((request, sender)) = commands.next() => {
                // Ack before anything else so the sending client stops
                // waiting in "connecting".
                let _ = sender.send(Reply::Success);
                if process_dj_command(
                    &session,
                    &player,
                    &device_info,
                    &events,
                    &notices,
                    &mut active,
                    request,
                )
                .await
                .is_none()
                {
                    break;
                }
            }
            Some(Ok(bytes)) = clusters.next() => report_cluster(&bytes, &mut reported_devices),
            Some(event) = player_events.recv() => {
                handle_player_event(
                    &session,
                    &player,
                    &device_info,
                    &notices,
                    &mut active,
                    event,
                )
                .await;
            }
            // A real player's state keeps moving; a frozen one makes the
            // sending client reclaim the cast.
            _ = state_ticker.tick() => {
                if let Some(state) = &active
                    && state.is_playing
                {
                    publish_active(&session, &device_info, state).await;
                }
            }
            else => break,
        }
    }
    log::info!("dj service: stopped");
}

/// The living playback state of an accepted DJ handover. Every change is
/// republished to connect-state so the sending client sees a player that
/// behaves like the official ones. The transfer-derived fields (origin,
/// restrictions, context metadata) mirror what the sender's own state
/// carried — the reference spirc warns that a player without them is
/// treated as inactive.
struct DjActive {
    context_uri: String,
    /// The context url exactly as the sender carried it: official clients
    /// publish the resolver url here, not a `context://` reconstruction.
    context_url: String,
    playback_id: String,
    tracks: Vec<dj::SessionTrack>,
    /// Per track: (context page index, track position inside that page) —
    /// the index a coherent connect-state publication reports.
    positions: Vec<(u32, u32)>,
    /// Metadata-fetched tracks aligned by uri with `tracks`; failures drop
    /// entries, so lookups go by uri, never by position.
    listed: Vec<model::ListedTrack>,
    track_index: usize,
    position_ms: u32,
    position_at: Instant,
    is_playing: bool,
    is_buffering: bool,
    play_origin: Option<PlayOrigin>,
    suppressions: Option<Suppressions>,
    options: Option<ContextPlayerOptions>,
    restrictions: Option<Restrictions>,
    context_metadata: HashMap<String, String>,
    last_command_sent_by_device_id: String,
    last_command_message_id: u32,
}

impl DjActive {
    /// The position right now: the last event position plus the time since.
    fn live_position_ms(&self) -> u32 {
        if self.is_playing {
            self.position_ms
                .saturating_add(self.position_at.elapsed().as_millis() as u32)
        } else {
            self.position_ms
        }
    }

    fn current_track(&self) -> &dj::SessionTrack {
        &self.tracks[self.track_index.min(self.tracks.len() - 1)]
    }

    /// Finds the metadata track for a session track uri.
    fn listed_for(&self, uri: &str) -> Option<&model::ListedTrack> {
        self.listed
            .iter()
            .find(|listed| listed.track.spotify_uri.as_deref() == Some(uri))
    }

    fn duration_of_current(&self) -> u32 {
        self.listed_for(&self.current_track().uri)
            .map(|listed| listed.track.duration_ms)
            .unwrap_or_default()
    }

    /// The player-bar notice for what is playing now and what follows.
    fn now_playing(&self) -> Option<dj::DjNowPlaying> {
        let current = self.listed_for(&self.current_track().uri)?.track.clone();
        let next = self.tracks[self.track_index + 1..]
            .iter()
            .filter_map(|track| self.listed_for(&track.uri).map(|l| l.track.clone()))
            .collect();
        Some(dj::DjNowPlaying { current, next })
    }
}

/// A fresh connect-state playback id: sixteen random bytes, hex, like the
/// official clients generate.
fn fresh_playback_id() -> String {
    use rand::RngCore as _;

    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Publishes the active DJ playback state, including the command pair from
/// the accepted transfer so the sending client sees its request honored.
async fn publish_active(session: &Session, device_info: &DeviceInfo, state: &DjActive) {
    let track = state.current_track();
    let position_ms = state.live_position_ms();
    // The context url must always carry a value: the reference spirc notes
    // a player without one is treated as inactive by the server.
    let provided = |session_track: &dj::SessionTrack| ProvidedTrack {
        uri: session_track.uri.clone(),
        uid: session_track.uid.clone(),
        provider: "context".to_owned(),
        metadata: session_track.metadata.clone(),
        ..Default::default()
    };
    let index = state.track_index.min(state.tracks.len() - 1);
    let (page, track_in_page) = state
        .positions
        .get(index)
        .copied()
        .unwrap_or((0, index as u32));
    let player_state = PlayerState {
        session_id: session.session_id(),
        context_uri: state.context_uri.clone(),
        context_url: if state.context_url.is_empty() {
            format!("context://{}", state.context_uri)
        } else {
            state.context_url.clone()
        },
        playback_id: state.playback_id.clone(),
        timestamp: now_millis() as i64,
        position_as_of_timestamp: position_ms as i64,
        duration: state.duration_of_current() as i64,
        playback_speed: 1.,
        is_playing: state.is_playing,
        is_paused: !state.is_playing && !state.is_buffering,
        is_buffering: state.is_buffering,
        play_origin: MessageField::some(state.play_origin.clone().unwrap_or_default()),
        suppressions: MessageField::some(state.suppressions.clone().unwrap_or_default()),
        options: MessageField::some(state.options.clone().unwrap_or_default()),
        restrictions: MessageField::from(state.restrictions.clone()),
        context_metadata: state.context_metadata.clone(),
        track: MessageField::some(provided(track)),
        // History in listening order, bounded like the reference spirc's.
        prev_tracks: state.tracks[index.saturating_sub(10)..index]
            .iter()
            .map(provided)
            .collect(),
        next_tracks: state.tracks[index + 1..].iter().map(provided).collect(),
        index: MessageField::some(ContextIndex {
            page,
            track: track_in_page,
            ..Default::default()
        }),
        ..Default::default()
    };
    let active = PutStateRequest {
        client_side_timestamp: now_millis(),
        member_type: EnumOrUnknown::new(MemberType::CONNECT_STATE),
        put_state_reason: EnumOrUnknown::new(PutStateReason::PLAYER_STATE_CHANGED),
        is_active: true,
        last_command_sent_by_device_id: state.last_command_sent_by_device_id.clone(),
        last_command_message_id: state.last_command_message_id,
        device: MessageField::some(Device {
            device_info: MessageField::some(device_info.clone()),
            player_state: MessageField::some(player_state),
            ..Default::default()
        }),
        ..Default::default()
    };
    match session.spclient().put_connect_state_request(&active).await {
        Ok(_) => log::info!(
            "dj service: published playback state (pos {position_ms} ms, buffering={})",
            state.is_buffering
        ),
        Err(error) => log::warn!("dj service: playback state PUT failed: {error}"),
    }
}

/// Keeps the published state aligned with the real player: buffering and
/// playing transitions republish immediately, position corrections only
/// update the tracked position, and a finished DJ track advances the lineup.
/// Events about foreign tracks are ignored — the app can play anything else
/// through the same player without disturbing the cast state.
async fn handle_player_event(
    session: &Session,
    player: &Arc<Player>,
    device_info: &DeviceInfo,
    notices: &async_chan::Sender<dj::DjNowPlaying>,
    active: &mut Option<DjActive>,
    event: PlayerEvent,
) {
    let Some(state) = active else {
        return;
    };
    let is_dj_track = |uri: &SpotifyUri| uri.to_string() == state.current_track().uri;
    match event {
        PlayerEvent::Loading {
            track_id,
            position_ms,
            ..
        } if is_dj_track(&track_id) => {
            state.is_buffering = true;
            state.is_playing = true;
            state.position_ms = position_ms;
            state.position_at = Instant::now();
            publish_active(session, device_info, state).await;
        }
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } if is_dj_track(&track_id) => {
            state.is_buffering = false;
            state.is_playing = true;
            state.position_ms = position_ms;
            state.position_at = Instant::now();
            publish_active(session, device_info, state).await;
        }
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } if is_dj_track(&track_id) => {
            state.is_playing = false;
            state.is_buffering = false;
            state.position_ms = position_ms;
            state.position_at = Instant::now();
            publish_active(session, device_info, state).await;
        }
        PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionChanged {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        } if is_dj_track(&track_id) => {
            state.position_ms = position_ms;
            state.position_at = Instant::now();
        }
        PlayerEvent::EndOfTrack { track_id, .. } if is_dj_track(&track_id) => {
            advance_dj_track(session, player, device_info, notices, active).await;
        }
        _ => {}
    }
}

/// Loads the next lineup track when the current one ends. The DJ queue is
/// server-driven, but the materialized snapshot carries the whole first
/// page, which keeps the music going until the next handover refreshes it.
async fn advance_dj_track(
    session: &Session,
    player: &Arc<Player>,
    device_info: &DeviceInfo,
    notices: &async_chan::Sender<dj::DjNowPlaying>,
    active: &mut Option<DjActive>,
) {
    let next_index;
    let notice;
    {
        let Some(state) = active.as_mut() else {
            return;
        };
        next_index = state.track_index + 1;
        let Some(track) = state.tracks.get(next_index) else {
            log::info!("dj service: lineup finished");
            *active = None;
            return;
        };
        let Ok(uri) = SpotifyUri::from_uri(&track.uri) else {
            return;
        };
        player.load(uri, true, 0);
        let state = active.as_mut().expect("checked above");
        state.track_index = next_index;
        state.is_buffering = true;
        state.is_playing = true;
        state.position_ms = 0;
        state.position_at = Instant::now();
        notice = state.now_playing();
        publish_active(session, device_info, state).await;
    }
    if let Some(notice) = notice {
        let _ = notices.send(notice).await;
    }
}

/// Handles one dealer command: acks it and, when it is a DJ handover,
/// resolves the session body into a lineup and starts playback from the
/// track the sender was on. Returns None when the service must stop
/// (nobody consumes lineups anymore).
async fn process_dj_command(
    session: &Session,
    player: &Arc<Player>,
    device_info: &DeviceInfo,
    events: &async_chan::Sender<dj::Lineup>,
    notices: &async_chan::Sender<dj::DjNowPlaying>,
    active: &mut Option<DjActive>,
    request: Request,
) -> Option<()> {
    let Command::Transfer(transfer) = request.command else {
        return Some(());
    };
    let Some(state) = transfer.data else {
        return Some(());
    };
    let session_state = state.current_session.get_or_default();
    let context = session_state.context.get_or_default();
    let dj_context_uri = context.uri.clone().unwrap_or_default();
    let session_url = context
        .url
        .as_deref()
        .filter(|url| url.starts_with("hm://"))
        .filter(|_| dj_context_uri.contains(dj::SOURCE_ID));

    // The sender tells us exactly where it is: which track plays, at what
    // position, paused or not. Continuing from there is what makes the cast
    // seamless — starting over from the first lineup track is what made
    // every cast replay the same song.
    let playback = state.playback.get_or_default();
    let transfer_track_uri = playback
        .current_track
        .get_or_default()
        .uri
        .clone()
        .unwrap_or_default();
    let is_paused = playback.is_paused.unwrap_or(true);
    let transfer_position = playback.position_as_of_timestamp.unwrap_or_default();
    let position_ms = if !is_paused && playback.timestamp.unwrap_or_default() > 0 {
        transfer_position.max(0) as u32
            + now_millis().saturating_sub(playback.timestamp.unwrap_or_default() as u64) as u32
    } else {
        transfer_position.max(0) as u32
    };

    log::info!("dj service: accepting a DJ handover");
    // Materialize the live queue from the transfer's own context: pages
    // that already carry tracks are used as they are, skeleton pages are
    // fetched from their page_url. The session resolver url returns the
    // canonical set rather than this session's queue, so it is only the
    // fallback when the transfer carries nothing usable.
    let mut tracks: Vec<dj::SessionTrack> = Vec::new();
    // Per track: (context page index, track position inside that page) —
    // the index a coherent connect-state publication reports.
    let mut positions: Vec<(u32, u32)> = Vec::new();
    for (page_index, page) in context.pages.iter().enumerate() {
        let page_index = page_index as u32;
        if page.tracks.is_empty() {
            let Some(page_url) = page
                .page_url
                .as_deref()
                .filter(|url| url.starts_with("hm://"))
            else {
                continue;
            };
            let body = match session.spclient().get_next_page(page_url).await {
                Ok(body) => body,
                Err(error) => {
                    log::warn!("dj service: page fetch failed: {error}");
                    continue;
                }
            };
            let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
                log::warn!("dj service: page body is not json");
                continue;
            };
            for track in dj::session_tracks(&parsed) {
                if !tracks.iter().any(|existing| existing.uri == track.uri) {
                    positions.push((page_index, u32::try_from(tracks.len()).unwrap_or(u32::MAX)));
                    tracks.push(track);
                }
            }
        } else {
            let mut position_in_page = 0u32;
            for track in page.tracks.iter() {
                let uri = track.uri.clone().unwrap_or_default();
                let canonical = track
                    .metadata
                    .get("canonical_track_uri")
                    .cloned()
                    .unwrap_or_default();
                let chosen = if !canonical.is_empty() {
                    canonical
                } else {
                    uri
                };
                if !chosen.starts_with("spotify:track:")
                    || tracks.iter().any(|existing| existing.uri == chosen)
                {
                    continue;
                }
                positions.push((page_index, position_in_page));
                position_in_page += 1;
                tracks.push(dj::SessionTrack {
                    uri: chosen.clone(),
                    uid: track.uid.clone().unwrap_or_default(),
                    metadata: track.metadata.clone(),
                });
            }
        }
    }
    if tracks.is_empty() {
        let Some(session_url) = session_url else {
            log::warn!("dj service: the handover carried no usable pages and no resolver url");
            return Some(());
        };
        let body = match session.spclient().get_next_page(session_url).await {
            Ok(body) => body,
            Err(error) => {
                log::warn!("dj service: lexicon session fetch failed: {error}");
                return Some(());
            }
        };
        let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&body) else {
            log::warn!("dj service: lexicon session body is not json");
            return Some(());
        };
        for track in dj::session_tracks(&parsed) {
            positions.push((0, u32::try_from(tracks.len()).unwrap_or(u32::MAX)));
            tracks.push(track);
        }
    }
    if tracks.is_empty() {
        log::warn!("dj service: the handover carried no tracks");
        return Some(());
    }
    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();

    let listed = listed_tracks_for_uris(session, &uris).await;
    if listed.is_empty() {
        log::warn!("dj service: none of the {} tracks resolved", uris.len());
        return Some(());
    }
    let playlist = dj::refreshed_playlist(uris.len() as u32, None);
    if events
        .send(dj::Lineup::Fresh(playlist, listed.clone()))
        .await
        .is_err()
    {
        log::info!("dj service: nobody is listening for lineups; stopping");
        return None;
    }

    // The transfer's own state carries the sender's playback identity; a
    // player without it is treated as inactive, so it rides along into
    // every publication.
    let transfer_state = DjActive {
        context_uri: dj_context_uri.clone(),
        context_url: context.url.clone().unwrap_or_default(),
        playback_id: fresh_playback_id(),
        positions,
        listed,
        play_origin: session_state
            .play_origin
            .clone()
            .into_option()
            .and_then(|origin| convert_proto(&origin)),
        suppressions: session_state
            .suppressions
            .clone()
            .into_option()
            .and_then(|suppressions| convert_proto(&suppressions)),
        options: state
            .options
            .clone()
            .into_option()
            .and_then(|options| convert_proto(&options)),
        restrictions: context
            .restrictions
            .clone()
            .into_option()
            .and_then(|restrictions| convert_proto(&restrictions)),
        context_metadata: context.metadata.clone(),
        last_command_sent_by_device_id: request.sent_by_device_id.clone(),
        last_command_message_id: request.message_id,
        tracks,
        track_index: 0,
        position_ms: 0,
        position_at: Instant::now(),
        is_playing: !is_paused,
        is_buffering: true,
    };
    let start_index = transfer_state
        .tracks
        .iter()
        .position(|track| track.uri == transfer_track_uri);

    // Reloading on every retry would restart the same track; only start the
    // context when it is not the one already playing.
    if active.as_ref().map(|state| state.context_uri.as_str()) != Some(dj_context_uri.as_str()) {
        let mut started = transfer_state;
        started.track_index = start_index.unwrap_or(0);
        started.position_ms = position_ms;
        started.position_at = Instant::now();
        if let Some(uri) = started
            .tracks
            .get(started.track_index)
            .and_then(|track| SpotifyUri::from_uri(&track.uri).ok())
        {
            log::info!(
                "dj service: starting from lineup position {} at {position_ms} ms",
                started.track_index
            );
            player.load(uri, !is_paused, position_ms);
        }
        if let Some(notice) = started.now_playing()
            && notices.send(notice).await.is_err()
        {
            log::info!("dj service: nobody is listening for playback notices; stopping");
            return None;
        }
        publish_active(session, device_info, &started).await;
        *active = Some(started);
    } else if let Some(state) = active {
        state.last_command_sent_by_device_id = request.sent_by_device_id;
        state.last_command_message_id = request.message_id;
        publish_active(session, device_info, state).await;
    }
    Some(())
}

/// The transfer-state protos duplicate the player protos with identical
/// field numbers; a byte round-trip converts between the two copies.
fn convert_proto<From: protobuf::Message, To: protobuf::Message>(from: &From) -> Option<To> {
    To::parse_from_bytes(&from.write_to_bytes().ok()?).ok()
}
async fn listed_tracks_for_uris(session: &Session, uris: &[String]) -> Vec<model::ListedTrack> {
    use protobuf::Message as _;

    let fetched = futures::stream::iter(uris.iter().cloned().map(|uri| async move {
        let result: Result<Option<model::ListedTrack>> = async {
            let track_uri = SpotifyUri::from_uri(&uri)?;
            let message = librespot::protocol::metadata::Track::parse_from_bytes(
                &librespot::metadata::Track::request(session, &track_uri).await?,
            )
            .map_err(anyhow::Error::from)?;
            Ok(proto_convert::track(&message)
                .ok()
                .filter(|track| track.is_displayable())
                .map(model::ListedTrack::undated))
        }
        .await;
        result
    }))
    .buffered(DJ_TRACK_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    fetched.into_iter().flatten().flatten().collect()
}

/// Logs the parts of a cluster update that reveal how the active device
/// publishes itself: casting to an official client and casting to Cadence
/// produces a diffable pair of these. The first state seen from an active
/// device is dumped in full — that raw reference is how missing fields in
/// our own publications get found.
fn report_cluster(bytes: &[u8], reported_devices: &mut HashSet<String>) {
    use protobuf::Message as _;

    let Ok(update) = librespot::protocol::connect::ClusterUpdate::parse_from_bytes(bytes) else {
        return;
    };
    let cluster = update.cluster.get_or_default();
    log::info!(
        "dj service: CLUSTER active={:?} reason={} devices={}",
        cluster.active_device_id,
        update.update_reason.value(),
        cluster.device.len(),
    );
    if !cluster.active_device_id.is_empty()
        && reported_devices.insert(cluster.active_device_id.clone())
    {
        log::info!(
            "dj service: CLUSTER full state from {:?}: {:?}",
            cluster.active_device_id,
            cluster.player_state
        );
    }
    let player = cluster.player_state.get_or_default();
    if player.context_uri.is_empty() {
        return;
    }
    let track = player.track.get_or_default();
    log::info!(
        "dj service:   state ctx={:?} playing={} paused={} speed={} pos={}",
        player.context_uri,
        player.is_playing,
        player.is_paused,
        player.playback_speed,
        player.position_as_of_timestamp,
    );
    log::info!(
        "dj service:   track uri={:?} provider={:?} uid={:?} metadata_keys={:?}",
        track.uri,
        track.provider,
        track.uid,
        track.metadata.keys().collect::<Vec<_>>(),
    );
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn playback_oauth_client() -> Result<PlaybackOAuthClient> {
    Ok(
        BasicClient::new(ClientId::new(PLAYBACK_CLIENT_ID.to_owned()))
            .set_auth_uri(AuthUrl::new(
                "https://accounts.spotify.com/authorize".to_owned(),
            )?)
            .set_token_uri(TokenUrl::new(
                "https://accounts.spotify.com/api/token".to_owned(),
            )?)
            .set_redirect_uri(RedirectUrl::new(PLAYBACK_REDIRECT_URI.to_owned())?),
    )
}

fn extract_track_uris(json: &[u8]) -> serde_json::Result<Vec<String>> {
    fn visit(value: &serde_json::Value, seen: &mut HashSet<String>, uris: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, seen, uris);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    visit(value, seen, uris);
                }
            }
            serde_json::Value::String(value) => {
                let mut rest = value.as_str();
                while let Some(offset) = rest.find("spotify:track:") {
                    let candidate = &rest[offset..];
                    let id = candidate["spotify:track:".len()..]
                        .chars()
                        .take_while(char::is_ascii_alphanumeric)
                        .collect::<String>();
                    if id.len() == 22 {
                        let uri = format!("spotify:track:{id}");
                        if seen.insert(uri.clone()) {
                            uris.push(uri);
                        }
                    }
                    rest = &candidate["spotify:track:".len()..];
                }
            }
            _ => {}
        }
    }

    let value = serde_json::from_slice(json)?;
    let mut seen = HashSet::new();
    let mut uris = Vec::new();
    visit(&value, &mut seen, &mut uris);
    Ok(uris)
}

fn playback_token_entry() -> Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(Into::into)
}

/// Why a failed credential read most likely failed, per platform: the
/// macOS keychain may prompt for access and need an explicit allow.
fn playback_credential_error_context() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "could not read playback credentials from Keychain; choose Always Allow when macOS asks"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "could not read playback credentials from the system credential store"
    }
}

fn playback_refresh_token() -> Result<Option<String>> {
    match playback_token_entry()?.get_password() {
        Ok(token) if token == LOGGED_OUT_CREDENTIAL => Ok(None),
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(anyhow!(error)),
    }
}

fn save_playback_refresh_token(refresh_token: &str) -> Result<()> {
    let refresh_token = validate_refresh_token(refresh_token)?;
    playback_token_entry()?
        .set_password(refresh_token)
        .map_err(Into::into)
}

async fn load_playback_refresh_token() -> Result<Option<String>> {
    credential_worker::run(playback_refresh_token)
        .await
        .context(playback_credential_error_context())
}

async fn persist_playback_refresh_token(refresh_token: String) -> Result<()> {
    credential_worker::run(move || save_playback_refresh_token(&refresh_token)).await
}

fn validate_refresh_token(refresh_token: &str) -> Result<&str> {
    if refresh_token.is_empty() {
        Err(anyhow!("Spotify returned an empty playback refresh token"))
    } else {
        Ok(refresh_token)
    }
}

pub async fn delete_playback_refresh_token() -> Result<()> {
    credential_worker::run(|| match playback_token_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(delete_error) => playback_token_entry()?
            .set_password(LOGGED_OUT_CREDENTIAL)
            .with_context(|| format!("could not invalidate credential after {delete_error}")),
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        PLAYBACK_CLIENT_ID, PLAYBACK_REDIRECT_URI, Playback, extract_track_uris,
        validate_refresh_token,
    };

    #[test]
    fn empty_refresh_tokens_are_rejected() {
        assert!(validate_refresh_token("").is_err());
        assert_eq!(
            validate_refresh_token("refresh-token").unwrap(),
            "refresh-token"
        );
    }

    #[tokio::test]
    async fn prepared_authorization_has_pkce_state_and_expected_redirect() {
        let authorization = Playback::prepare_authorization().await.unwrap();
        let url = url::Url::parse(authorization.url()).unwrap();
        let parameters = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(url.host_str(), Some("accounts.spotify.com"));
        assert_eq!(parameters.get("client_id").unwrap(), PLAYBACK_CLIENT_ID);
        assert_eq!(
            parameters.get("redirect_uri").unwrap(),
            PLAYBACK_REDIRECT_URI
        );
        assert_eq!(parameters.get("scope").unwrap(), "streaming");
        assert_eq!(parameters.get("code_challenge_method").unwrap(), "S256");
        assert!(parameters.contains_key("code_challenge"));
        assert!(parameters.contains_key("state"));
    }

    #[test]
    fn extracts_unique_track_uris_from_nested_radio_json() {
        let json = br#"{
            "items":[
                {"uri":"spotify:track:0123456789ABCDEFGHIJKL"},
                {"metadata":{"context":"before spotify:track:abcdefghijklmnopqrstuv after"}},
                {"uri":"spotify:track:0123456789ABCDEFGHIJKL"},
                {"uri":"spotify:album:0123456789ABCDEFGHIJKL"},
                {"uri":"spotify:track:short"}
            ]
        }"#;

        assert_eq!(
            extract_track_uris(json).unwrap(),
            vec![
                "spotify:track:0123456789ABCDEFGHIJKL",
                "spotify:track:abcdefghijklmnopqrstuv"
            ]
        );
    }
}
