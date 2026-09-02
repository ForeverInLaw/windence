use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use async_channel as async_chan;
use futures::StreamExt as _;
use keyring::Entry;
use librespot::{
    core::{
        SpotifyUri, authentication::Credentials, config::SessionConfig, dealer::Subscription,
        dealer::protocol::Message as DealerMessage, session::Session,
    },
    metadata::Metadata,
    oauth::OAuthClientBuilder,
    playback::{
        SAMPLE_RATE,
        config::{AudioFormat, PlayerConfig, VolumeCtrl},
        mixer::{self, Mixer, MixerConfig},
        player::{Player, PlayerEventChannel},
    },
    protocol::{connect::PutStateReason, playlist4_external},
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use tokio::net::TcpListener;

use crate::{
    audio::{NarrationClip, low_latency_sdl_sink},
    connect, credential_worker, dj, feed, model, narration,
    oauth_callback::receive_callback,
    oauth_page::{OAuthStep, success_page},
    proto::collection2v2::{
        CollectionItem, DeltaRequest, DeltaResponse, PageRequest, PageResponse, WriteRequest,
    },
    proto::recently_played_backend::RecentlyPlayed,
    proto_convert,
};

const PLAYBACK_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYCHAIN_SERVICE: &str = "com.cadence.spotify";
const KEYCHAIN_ACCOUNT: &str = "playback-refresh-token";
const LOGGED_OUT_CREDENTIAL: &str = "cadence-logged-out";
/// The collection set Spotify keeps the account's pins in.
const PIN_SET: &str = "ylpin";
/// The content type the collection service speaks, on the way in and on the
/// way back. It is the only one it takes: a plain protobuf type is answered
/// with 400, which is why these requests are built here rather than through
/// librespot's protobuf helper, which sets its own.
const COLLECTION_CONTENT_TYPE: &str = "application/vnd.collection-v2.spotify.proto";
/// Where Spotify announces the id of the socket this session is on, and the
/// header that announcement carries it in.
const CONNECTION_ID_TOPIC: &str = "hm://pusher/v1/connections/";
const CONNECTION_ID_HEADER: &str = "Spotify-Connection-Id";
/// How many internal-protocol track lookups overlap when resolving a DJ
/// stretch: a whole stretch resolves well inside the catalog timeout
/// without bursting one access point.
const DJ_TRACK_CONCURRENCY: usize = 4;

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
    /// Lines waiting to be spoken. The output device only exists on the
    /// player's own thread, so everything audible reaches it this way.
    narration: async_chan::Sender<NarrationClip>,
    /// Raised when the listener interrupts, so a line still being handed
    /// to the device is dropped rather than played out first. Cleared by
    /// the next line, which is a fresh intent to speak.
    narration_interrupted: Arc<AtomicBool>,
    /// Narration synthesis answers with a redirect that must be read, not
    /// followed, so this client is configured differently from the
    /// session's own.
    http: oauth2_reqwest::Client,
    /// Whether the dealer socket has been opened on this session. It takes
    /// one start, and a second one fails whatever the first one did. A lock
    /// rather than a flag because a subscription made while a start is in
    /// flight is refused: librespot has handed the builder to the start and
    /// has no socket to put it on yet.
    dealer_started: Arc<tokio::sync::Mutex<bool>>,
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
        let (narration_sender, narration) = async_chan::unbounded();
        let sink_mixer = mixer.clone();
        let narration_interrupted = Arc::new(AtomicBool::new(false));
        let sink_interrupted = narration_interrupted.clone();
        let player = Player::new(player_config, session.clone(), volume, move || {
            low_latency_sdl_sink(
                AudioFormat::default(),
                narration.clone(),
                sink_mixer.get_soft_volume(),
                sink_interrupted.clone(),
            )
        });
        Ok(Self {
            player,
            mixer,
            session,
            narration: narration_sender,
            narration_interrupted,
            http: oauth2_reqwest::ClientBuilder::new()
                .redirect(oauth2_reqwest::redirect::Policy::none())
                .build()
                .context("could not configure the narration client")?,
            dealer_started: Arc::new(tokio::sync::Mutex::new(false)),
        })
    }

    pub fn load(&self, spotify_uri: SpotifyUri, playing: bool, position_ms: u32) {
        self.player.load(spotify_uri, playing, position_ms);
    }

    pub fn play(&self) {
        // Carrying on lifts the silence a pause put on the DJ, so a line
        // queued just before it is still spoken.
        self.narration_interrupted.store(false, Ordering::Relaxed);
        self.player.play();
    }

    pub fn pause(&self) {
        self.silence_narration();
        self.player.pause();
    }

    pub fn seek(&self, position_ms: u32) {
        self.silence_narration();
        self.player.seek(position_ms);
    }

    pub fn set_volume(&self, volume: f32) {
        self.mixer
            .set_volume((volume.clamp(0., 1.) * f32::from(VolumeCtrl::MAX_VOLUME)) as u16);
    }

    pub fn stop(&self) {
        self.silence_narration();
        self.player.stop();
    }

    pub fn events(&self) -> PlayerEventChannel {
        self.player.get_player_event_channel()
    }

    pub fn is_connected(&self) -> bool {
        !self.session.is_invalid()
    }

    /// One page of the account's rootlist: the playlist set, the folder
    /// tree and each entry's Date Added. `from` counts rootlist items,
    /// group markers included, so it is the scan's own running position.
    pub(crate) async fn rootlist_page(
        &self,
        from: usize,
        length: usize,
    ) -> Result<playlist4_external::SelectedListContent> {
        use protobuf::Message as _;
        let body = self
            .session
            .spclient()
            .get_rootlist(from, Some(length))
            .await
            .context("Spotify rootlist endpoint failed")?;
        playlist4_external::SelectedListContent::parse_from_bytes(&body)
            .context("Spotify rootlist returned an undecodable message")
    }

    /// When each context was last played, on this device or any other.
    /// librespot has no helper for this endpoint, so the request is built
    /// here; the session client still supplies the access point and the
    /// credentials.
    pub(crate) async fn recently_played(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<RecentlyPlayed> {
        use protobuf::Message as _;
        let mut endpoint = format!("/recently-played/v3/recently-played?limit={limit}");
        if offset > 0 {
            endpoint.push_str(&format!("&offset={offset}"));
        }
        let body = self
            .session
            .spclient()
            .request(&http::Method::GET, &endpoint, None, None)
            .await
            .context("Spotify recently-played endpoint failed")?;
        RecentlyPlayed::parse_from_bytes(&body)
            .context("Spotify recently-played returned an undecodable message")
    }

    /// The two tokens a direct request to a Spotify service carries: the
    /// account's access token and the client token. Both come from the
    /// session and rotate, so they are read for each request. `purpose`
    /// names the request in the error.
    async fn service_tokens(&self, purpose: &str) -> Result<(String, String)> {
        let access_token = self
            .session
            .login5()
            .auth_token()
            .await
            .with_context(|| format!("no Spotify access token for {purpose}"))?
            .access_token;
        let client_token = self
            .session
            .spclient()
            .client_token()
            .await
            .with_context(|| format!("no Spotify client token for {purpose}"))?;
        Ok((access_token, client_token))
    }

    /// The partner-endpoint client for this session, built per call because
    /// the tokens it carries rotate.
    async fn pathfinder(&self) -> Result<feed::Pathfinder> {
        let (access_token, client_token) = self.service_tokens("the Home feed").await?;
        Ok(feed::Pathfinder::new(
            self.http.clone(),
            access_token,
            client_token,
        ))
    }

    /// The account's Home feed: Spotify's curated shelves (Discover Weekly,
    /// the Daily Mixes, daylists, editorial and mood shelves). The Web API
    /// has no such feed; this reads the internal GraphQL partner endpoint.
    pub(crate) async fn home_feed(&self) -> Result<model::HomeFeed> {
        self.pathfinder().await?.home().await
    }

    /// The next page of one Home shelf's cards; see `feed::Pathfinder`.
    pub(crate) async fn home_shelf(
        &self,
        shelf_uri: &str,
        offset: u32,
    ) -> Result<model::HomeShelfPage> {
        self.pathfinder().await?.home_shelf(shelf_uri, offset).await
    }

    /// One page of the pinned set. `page_token` is what the previous page
    /// handed back, or `None` for the first.
    pub(crate) async fn pin_page(
        &self,
        limit: usize,
        page_token: Option<&str>,
    ) -> Result<PageResponse> {
        let request = PageRequest {
            username: self.session.username(),
            set: PIN_SET.to_owned(),
            pagination_token: page_token.unwrap_or_default().to_owned(),
            limit: limit.try_into().unwrap_or(i32::MAX),
            ..Default::default()
        };
        self.collection_request("/collection/v2/paging", &request)
            .await
            .context("Spotify pin paging endpoint failed")
    }

    /// What has changed in the pinned set since `sync_token` was issued.
    pub(crate) async fn pin_delta(&self, sync_token: &str) -> Result<DeltaResponse> {
        let request = DeltaRequest {
            username: self.session.username(),
            set: PIN_SET.to_owned(),
            last_sync_token: sync_token.to_owned(),
            ..Default::default()
        };
        self.collection_request("/collection/v2/delta", &request)
            .await
            .context("Spotify pin delta endpoint failed")
    }

    /// Writes to the pinned set.
    ///
    /// A pin sends the whole set — Spotify replaces what it holds with what
    /// it is given — and an unpin sends one item marked removed. The server
    /// answers with an empty body, so anything but an error is a success.
    /// `client_update_id` comes back on the dealer message this write
    /// causes, which is how a client tells its own change from another
    /// device's.
    pub(crate) async fn write_pins(&self, items: Vec<CollectionItem>) -> Result<()> {
        let request = WriteRequest {
            username: self.session.username(),
            set: PIN_SET.to_owned(),
            items,
            client_update_id: format!("{:016x}", rand::random::<u64>()),
            ..Default::default()
        };
        self.collection_post("/collection/v2/write", &request)
            .await
            .context("Spotify refused the pin change")?;
        Ok(())
    }

    async fn collection_request<Request, Response>(
        &self,
        endpoint: &str,
        request: &Request,
    ) -> Result<Response>
    where
        Request: protobuf::Message,
        Response: protobuf::Message,
    {
        let body = self.collection_post(endpoint, request).await?;
        Response::parse_from_bytes(&body)
            .with_context(|| format!("Spotify {endpoint} returned an undecodable message"))
    }

    /// Posts one protobuf to the collection service and hands back the body
    /// it answered with, which for a write is empty.
    async fn collection_post<Request: protobuf::Message>(
        &self,
        endpoint: &str,
        request: &Request,
    ) -> Result<Vec<u8>> {
        let content_type = http::HeaderValue::from_static(COLLECTION_CONTENT_TYPE);
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::CONTENT_TYPE, content_type.clone());
        headers.insert(http::header::ACCEPT, content_type);
        let body = request.write_to_bytes()?;
        let response = self
            .session
            .spclient()
            .request(&http::Method::POST, endpoint, Some(headers), Some(&body))
            .await?;
        Ok(response.to_vec())
    }

    /// Tells Spotify what Cadence is playing, so the play reaches the
    /// account's history and every other client's library order.
    ///
    /// The request carries the session's connection id in a header, and
    /// that id arrives over the dealer socket — see [`Self::watch_connection_id`],
    /// which has to have run at least once before this can succeed.
    pub(crate) async fn report_state(
        &self,
        report: Option<&connect::Report>,
        reason: PutStateReason,
    ) -> Result<()> {
        let identity = connect::Identity {
            device_id: self.session.device_id().to_owned(),
            client_id: self.session.client_id(),
        };
        let request = connect::state_request(
            &identity,
            report,
            reason,
            chrono::Utc::now().timestamp_millis(),
        );
        self.session
            .spclient()
            .put_connect_state_request(&request)
            .await
            .context("Spotify refused the device state")?;
        Ok(())
    }

    /// Follows the connection ids Spotify issues for this session.
    ///
    /// Every connect-state request is tagged with the id of the socket it
    /// belongs to, and Spotify hands that id out over the socket itself. It
    /// is re-issued whenever the socket reconnects, so this is a stream and
    /// not a one-off: [`Self::apply_connection_id`] stores each one.
    pub(crate) async fn watch_connection_id(&self) -> Result<Subscription> {
        self.subscribe(CONNECTION_ID_TOPIC.to_owned())
            .await
            .context("could not follow the Spotify connection id")
    }

    /// Stores the connection id a dealer message carried, and says whether
    /// there was one. A message without the header names no connection and
    /// is nothing to report against.
    pub(crate) fn apply_connection_id(&self, message: &DealerMessage) -> bool {
        let Some(connection_id) = message.headers.get(CONNECTION_ID_HEADER) else {
            return false;
        };
        self.session.set_connection_id(connection_id);
        true
    }

    /// Opens the dealer subscription that reports pin changes.
    ///
    /// Spotify sends one of these whenever the set moves on any device. The
    /// message says that something changed, not what, so Cadence answers it
    /// with an increment against the stored sync token. It also never
    /// carries the set itself, which is why the full read comes first.
    pub(crate) async fn subscribe_pins(&self) -> Result<Subscription> {
        self.subscribe(format!(
            "hm://collection/{PIN_SET}/{}",
            self.session.username()
        ))
        .await
    }

    /// Subscribes to one dealer topic, opening the socket the first time.
    ///
    /// Both halves happen under the same lock. The socket takes one start —
    /// a second one fails whatever the first one did — and a subscription
    /// made while a start is in flight is refused, so two callers arriving
    /// at once have to take their turn rather than overlap. Nothing else in
    /// Cadence opens the socket, so every subscription comes through here.
    ///
    /// A start that fails takes the socket with it for good: librespot hands
    /// its builder to the attempt before it knows whether the attempt works,
    /// and there is no second builder. So the session is given up as well,
    /// which is the one thing that does bring the socket back — a reconnect
    /// builds a new session, and a new session has a builder again.
    async fn subscribe(&self, topic: String) -> Result<Subscription> {
        let mut started = self.dealer_started.lock().await;
        let subscription = self.session.dealer().add_listen_for(topic)?;
        if !*started {
            if let Err(error) = self.session.dealer().start().await {
                self.session.shutdown();
                return Err(anyhow::Error::new(error))
                    .context("could not open the Spotify dealer socket; reconnecting");
            }
            *started = true;
        }
        Ok(subscription)
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

    /// Fetches one stretch of a DJ session. `url` is either
    /// [`dj::session_url`], which starts the session, or the cursor a
    /// previous stretch handed back. Both are internal-protocol urls the
    /// session client authenticates for us.
    pub(crate) async fn dj_page(&self, url: &str) -> Result<dj::SessionPage> {
        let body = self
            .session
            .spclient()
            .get_next_page(url)
            .await
            .map_err(|error| match dj::refusal(error.kind) {
                // Spotify does not offer the station to this account or in
                // this region — a fact, not a transport failure to retry.
                true => anyhow!("DJ X is not available on this account"),
                false => anyhow::Error::new(error).context("Spotify DJ session endpoint failed"),
            })?;
        let value =
            serde_json::from_slice(&body).context("Spotify DJ session returned invalid JSON")?;
        Ok(dj::session_page(&value))
    }

    /// Synthesizes one of the DJ's lines. The service answers with a
    /// redirect to a pre-signed CDN url, which is why the redirect is not
    /// followed automatically and the download carries no credentials.
    pub(crate) async fn synthesize(&self, line: &dj::Line) -> Result<NarrationClip> {
        let base_url = self
            .session
            .spclient()
            .base_url()
            .await
            .context("no Spotify access point for narration")?;
        let (access_token, client_token) = self.service_tokens("narration").await?;
        let response = self
            .http
            .post(format!("{base_url}/client-tts/v1/fulfill"))
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Client-Token", client_token)
            .header("Content-Type", "application/x-protobuf")
            .body(dj::tts_request(line, SAMPLE_RATE))
            .send()
            .await
            .context("Spotify narration synthesis failed")?;
        let audio_url = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .context("Spotify narration synthesis named no audio")?
            .to_owned();
        let mp3 = self
            .http
            .get(audio_url)
            .send()
            .await
            .context("Spotify narration download failed")?
            .bytes()
            .await
            .context("Spotify narration download was cut short")?;
        // Decoding is a few tens of milliseconds of pure work; the async
        // runtime is not the place for it.
        tokio::task::spawn_blocking(move || narration::decode(mp3.to_vec()))
            .await
            .context("narration decoding did not finish")?
    }

    /// Hands a decoded line to the output device, which speaks it before
    /// the next song. Nothing waits on it: a line that cannot be queued is
    /// simply not spoken.
    pub(crate) fn speak(&self, clip: NarrationClip) {
        // A line to say clears whatever interrupted the one before it.
        self.narration_interrupted.store(false, Ordering::Relaxed);
        let _ = self.narration.try_send(clip);
    }

    /// Cuts a line short. The device is only ever a fraction of a second
    /// ahead while speaking, so the voice stops about as fast as the music
    /// does. Skipping is the caller that needs this: it moves the player
    /// without pausing or stopping it.
    pub(crate) fn silence_narration(&self) {
        self.narration_interrupted.store(true, Ordering::Relaxed);
    }

    /// Resolves track uris into the tracks a list can show. DJ sessions
    /// name songs by uri only, so everything the page and the player bar
    /// display comes from here.
    pub(crate) async fn tracks_for_uris(&self, uris: &[String]) -> Vec<model::ListedTrack> {
        use protobuf::Message as _;

        let fetched = futures::stream::iter(uris.iter().cloned().map(|uri| async move {
            let result: Result<Option<model::ListedTrack>> = async {
                let track_uri = SpotifyUri::from_uri(&uri)?;
                let message = librespot::protocol::metadata::Track::parse_from_bytes(
                    &librespot::metadata::Track::request(&self.session, &track_uri).await?,
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
