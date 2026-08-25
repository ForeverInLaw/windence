use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow};
use async_channel as async_chan;
use futures::StreamExt as _;
use keyring::Entry;
use librespot::{
    core::{SpotifyUri, authentication::Credentials, config::SessionConfig, session::Session},
    metadata::Metadata,
    oauth::OAuthClientBuilder,
    playback::{
        SAMPLE_RATE,
        config::{AudioFormat, PlayerConfig, VolumeCtrl},
        mixer::{self, Mixer, MixerConfig},
        player::{Player, PlayerEventChannel},
    },
};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
    basic::BasicClient,
};
use tokio::net::TcpListener;

use crate::{
    audio::{NarrationClip, low_latency_sdl_sink},
    credential_worker, dj, model, narration,
    oauth_callback::receive_callback,
    oauth_page::{OAuthStep, success_page},
    proto_convert,
};

const PLAYBACK_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const PLAYBACK_REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
const KEYCHAIN_SERVICE: &str = "com.cadence.spotify";
const KEYCHAIN_ACCOUNT: &str = "playback-refresh-token";
const LOGGED_OUT_CREDENTIAL: &str = "cadence-logged-out";
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
    /// Narration synthesis answers with a redirect that must be read, not
    /// followed, so this client is configured differently from the
    /// session's own.
    http: oauth2_reqwest::Client,
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
        let player = Player::new(player_config, session.clone(), volume, move || {
            low_latency_sdl_sink(AudioFormat::default(), narration.clone())
        });
        Ok(Self {
            player,
            mixer,
            session,
            narration: narration_sender,
            http: oauth2_reqwest::ClientBuilder::new()
                .redirect(oauth2_reqwest::redirect::Policy::none())
                .build()
                .context("could not configure the narration client")?,
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
        let access_token = self
            .session
            .login5()
            .auth_token()
            .await
            .context("no Spotify access token for narration")?
            .access_token;
        let client_token = self
            .session
            .spclient()
            .client_token()
            .await
            .context("no Spotify client token for narration")?;
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
        let _ = self.narration.try_send(clip);
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
