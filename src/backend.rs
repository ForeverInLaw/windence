use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender as StdSender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::StreamExt as _;
use librespot::{
    core::SpotifyUri, playback::player::PlayerEvent, protocol::connect::PutStateReason,
};
use tokio::sync::mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender};

use crate::{
    audio::NarrationClip,
    connect, dj,
    library_index::{self, LibraryIndex, RootlistScan},
    model::{Album, Artist, ListedTrack, Playlist, Track, UserProfile},
    pins::{self, Pins},
    playback::{Playback, PlaybackAuthorization, delete_playback_refresh_token},
    shuffle::{
        ContextKind, Origin, ShuffleMode, ShuffleRng, ShuffleState, injection_target,
        smart_admissible,
    },
    spotify::{
        ClientIdSource, Spotify, SpotifyConfiguration, resolve_configuration, valid_client_id,
    },
    storage::{LibraryFingerprint, PlaybackSnapshot, Store},
};

const CATALOG_TIMEOUT_SECONDS: u64 = 30;
const LIBRARY_TIMEOUT_SECONDS: u64 = 60;
const COMMAND_CAPACITY: usize = 256;
const CONTROL_CAPACITY: usize = 8;

struct AuthorizationSuccess {
    playback: Option<PlaybackConnectionRequest>,
}

struct PlaybackConnectionRequest {
    load_saved_token: bool,
    authorization: Option<PlaybackAuthorization>,
}

#[derive(Clone)]
struct BlockingStore {
    jobs: std::sync::mpsc::Sender<StoreJob>,
}

type StoreJob = Box<dyn FnOnce(&mut Store) + Send>;
type RadioTask = tokio::task::JoinHandle<(u64, Result<Vec<Track>>)>;

impl BlockingStore {
    async fn open_default() -> Result<Self> {
        let (jobs, receiver) = std::sync::mpsc::channel::<StoreJob>();
        let (initialized, initialization) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("cadence-storage".to_owned())
            .spawn(move || {
                let mut store = match Store::open_default() {
                    Ok(store) => store,
                    Err(error) => {
                        let _ = initialized.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = initialized.send(Ok(()));
                while let Ok(job) = receiver.recv() {
                    job(&mut store);
                }
            })
            .context("could not start storage worker")?;
        initialization
            .await
            .context("storage worker stopped during initialization")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { jobs })
    }

    #[cfg(test)]
    fn from_store(mut store: Store) -> Self {
        let (jobs, receiver) = std::sync::mpsc::channel::<StoreJob>();
        std::thread::spawn(move || {
            while let Ok(job) = receiver.recv() {
                job(&mut store);
            }
        });
        Self { jobs }
    }

    async fn call<T>(
        &self,
        operation: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |store| {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(store)))
                        .map_err(|_| anyhow!("storage operation panicked"))
                        .and_then(|result| result);
                let _ = sender.send(result);
            }))
            .map_err(|_| anyhow!("storage worker is unavailable"))?;
        receiver
            .await
            .context("storage operation did not complete")?
    }

    async fn playback_state(&self) -> Result<Option<PlaybackSnapshot>> {
        self.call(|store| store.playback_state()).await
    }

    async fn local_state(&self) -> Result<(Pins, Vec<Track>, LibraryIndex)> {
        self.call(|store| {
            Ok((
                store.pins()?,
                store.recent_tracks(100)?,
                store.library_index()?,
            ))
        })
        .await
    }

    async fn pins(&self) -> Result<Pins> {
        self.call(|store| store.pins()).await
    }

    async fn pin_sync_token(&self) -> Result<Option<String>> {
        self.call(|store| store.pin_sync_token()).await
    }

    async fn set_pins(&self, pins: Pins, sync_token: Option<String>) -> Result<()> {
        self.call(move |store| store.set_pins(&pins, sync_token.as_deref()))
            .await
    }

    async fn library_index(&self) -> Result<LibraryIndex> {
        self.call(|store| store.library_index()).await
    }

    async fn rootlist_revision(&self) -> Result<Option<String>> {
        self.call(|store| store.rootlist_revision()).await
    }

    async fn replace_rootlist(
        &self,
        entries: Vec<library_index::IndexEntry>,
        revision: Option<String>,
        complete: bool,
    ) -> Result<()> {
        self.call(move |store| store.replace_rootlist(&entries, revision.as_deref(), complete))
            .await
    }

    async fn recently_played_watermark(&self) -> Result<Option<i64>> {
        self.call(|store| store.recently_played_watermark()).await
    }

    async fn apply_recently_played(&self, plays: Vec<(String, i64)>) -> Result<()> {
        self.call(move |store| store.apply_recently_played(&plays))
            .await
    }

    async fn set_last_played(&self, uri: String, played_at_ms: i64) -> Result<()> {
        self.call(move |store| store.set_last_played(&uri, played_at_ms))
            .await
    }

    async fn liked_tracks(&self) -> Result<Vec<ListedTrack>> {
        self.call(|store| store.liked_tracks()).await
    }

    /// The persisted library cache: what the last completed load stored,
    /// served when a boot probe proves it still matches Spotify.
    async fn library_cache(&self) -> Result<LibraryContents> {
        self.call(|store| Ok((store.liked_tracks()?, store.cached_playlists()?)))
            .await
    }

    async fn saved_library_fingerprint(&self) -> Result<Option<LibraryFingerprint>> {
        self.call(|store| store.saved_library_fingerprint()).await
    }

    async fn remove_spotify_client_id(&self) -> Result<()> {
        self.call(|store| store.remove_spotify_client_id()).await
    }

    async fn configure_spotify(&self, client_id: String) -> Result<()> {
        self.call(move |store| store.configure_spotify(&client_id))
            .await
    }

    async fn reset_spotify_configuration(&self) -> Result<()> {
        self.call(|store| store.reset_spotify_configuration()).await
    }

    async fn set_oauth_credentials_invalidated(&self, invalidated: bool) -> Result<()> {
        self.call(move |store| store.set_spotify_oauth_credentials_invalidated(invalidated))
            .await
    }

    async fn set_playback_credentials_invalidated(&self, invalidated: bool) -> Result<()> {
        self.call(move |store| store.set_spotify_playback_credentials_invalidated(invalidated))
            .await
    }

    /// Persists the whole library cache atomically, unless a newer account
    /// generation superseded this load. The fingerprint goes in the same
    /// transaction as the contents it vouches for.
    async fn replace_library_cache_if_current(
        &self,
        liked_tracks: Vec<ListedTrack>,
        playlists: Vec<Playlist>,
        fingerprint: LibraryFingerprint,
        current_generation: Arc<AtomicU64>,
        generation: u64,
    ) -> Result<Option<LibraryContents>> {
        self.call(move |store| {
            if current_generation.load(Ordering::Acquire) != generation {
                return Ok(None);
            }
            store.replace_library_cache(&liked_tracks, &playlists, &fingerprint)?;
            Ok(Some((liked_tracks, playlists)))
        })
        .await
    }

    async fn clear_library_cache(&self) -> Result<()> {
        self.call(|store| store.clear_library_cache()).await
    }

    async fn clear_playback_state(&self) -> Result<()> {
        self.call(|store| store.clear_playback_state()).await
    }

    async fn set_playback_state(
        &self,
        tracks: Vec<Track>,
        index: usize,
        position_ms: u32,
        shuffle: ShuffleState,
        radio: bool,
        kind: ContextKind,
    ) -> Result<()> {
        self.call(move |store| {
            store.set_playback_state(&tracks, index, position_ms, &shuffle, radio, kind)
        })
        .await
    }

    async fn update_playback_position(&self, position_ms: u32) -> Result<()> {
        self.call(move |store| store.update_playback_position(position_ms))
            .await
    }

    async fn add_history(&self, track: Track) -> Result<()> {
        self.call(move |store| store.add_history(&track)).await
    }

    async fn smart_shuffle_seen(&self) -> Result<Vec<String>> {
        self.call(|store| store.smart_shuffle_seen()).await
    }

    async fn add_smart_shuffle_seen(&self, ids: Vec<String>) -> Result<()> {
        self.call(move |store| store.add_smart_shuffle_seen(&ids))
            .await
    }

    async fn clear_smart_shuffle_seen(&self) -> Result<()> {
        self.call(|store| store.clear_smart_shuffle_seen()).await
    }
}

/// Where a catalog request sends its answer. Dropping the receiving half
/// cancels the request: the reply simply goes nowhere.
pub type Reply<T> = tokio::sync::oneshot::Sender<Result<T>>;
#[derive(Debug)]
/// The payload of a catalog page load.
pub enum PlaylistContents {
    Loaded {
        /// A refreshed playlist object when the load learned something the
        /// page could not know up front; pages keep their own copy
        /// otherwise.
        playlist: Option<Playlist>,
        tracks: Vec<ListedTrack>,
    },
}

/// Tracks and playlists, as returned by search.
pub type TrackAndPlaylistResults = (Vec<Track>, Vec<Playlist>);

/// A library's liked tracks and playlists, as loaded from Spotify or its
/// cache. The liked side carries each track's date added.
pub type LibraryContents = (Vec<ListedTrack>, Vec<Playlist>);

/// A library reload's answer: fresh contents, or proof nothing changed for
/// the price of the two head requests.
#[derive(Debug)]
pub enum LibraryReload {
    Unchanged,
    Fresh(LibraryContents),
}

type SharedFingerprint = Arc<std::sync::Mutex<Option<LibraryFingerprint>>>;

fn commit_fingerprint(fingerprint: &SharedFingerprint, value: LibraryFingerprint) {
    *fingerprint.lock().expect("library fingerprint lock") = Some(value);
}

fn clear_fingerprint(fingerprint: &SharedFingerprint) {
    *fingerprint.lock().expect("library fingerprint lock") = None;
}
pub type ArtistDetails = (Artist, Vec<Track>, Vec<Album>);
pub type AlbumDetails = (Album, Vec<Track>);

#[derive(Debug)]
pub enum BackendCommand {
    Authenticate {
        generation: u64,
    },
    Logout {
        generation: u64,
    },
    ConfigureSpotify {
        generation: u64,
        client_id: String,
    },
    ResetSpotifyConfiguration {
        generation: u64,
    },
    ReloadLibrary {
        respond: Reply<LibraryReload>,
    },
    SearchCatalog {
        query: String,
        respond: Reply<TrackAndPlaylistResults>,
    },
    LoadPlaylist {
        playlist: Playlist,
        respond: Reply<PlaylistContents>,
    },
    LoadArtist {
        source_id: String,
        respond: Reply<ArtistDetails>,
    },
    LoadAlbum {
        source_id: String,
        respond: Reply<AlbumDetails>,
    },
    StartRadio {
        request_id: u64,
        seed: Track,
    },
    PlayContext {
        tracks: Vec<Track>,
        index: usize,
        /// Start this context shuffled and move the global toggle to
        /// Shuffle, as the playlist and album shuffle-play controls do.
        shuffled: bool,
        /// Where this context was started from, which gates Smart Shuffle.
        kind: ContextKind,
        /// The Spotify uri of what was started, when it has one. Naming it
        /// moves the playlist to the top of the library list at once.
        context_uri: Option<String>,
    },
    /// Starts the DJ station: Cadence resolves the session itself, so the
    /// caller hands over no tracks.
    PlayDj,
    PlayNext(Track),
    AppendToQueue(Track),
    SetShuffleMode(ShuffleMode),
    RestorePlayback {
        position_ms: u32,
        playing: bool,
    },
    /// Adds or removes a track in the account's Spotify Liked Songs.
    SetLiked {
        track: Track,
        liked: bool,
    },
    /// Pins or unpins one library item on the account, so every device the
    /// listener has shows the change.
    SetPinned {
        uri: String,
        pinned: bool,
    },
    /// Moves a pin to where another one sits, which is what dropping a row
    /// on another row in the pinned section means.
    MovePin {
        uri: String,
        target: String,
    },
    Resume,
    Pause,
    Next,
    Previous,
    Seek(u32),
    SavePlaybackPosition {
        spotify_uri: String,
        position_ms: u32,
    },
    SetVolume(f32),
    Shutdown {
        acknowledged: StdSender<()>,
    },
}

#[derive(Debug)]
pub enum BackendEvent {
    SetupRequired,
    SpotifyConfigured {
        generation: u64,
        client_id: String,
        source: ClientIdSource,
    },
    SpotifyConfigurationFailed {
        generation: u64,
        error: String,
    },
    SpotifyConfigurationResetFailed(String),
    AuthorizationRequired,
    LoggedOut,
    CatalogReady {
        generation: u64,
    },
    PlaybackReady,
    PlaybackReconnecting,
    PlaybackReconnected,
    PlaybackRestored {
        position_ms: u32,
        playing: bool,
    },
    PlaybackSettled,
    QueueEnded,
    /// Whether the DJ station is what the player is playing. The station
    /// page follows the live queue while it is, instead of asking the
    /// session for a stretch that has already moved on.
    StationChanged(bool),
    LibraryLoaded {
        generation: u64,
        liked_tracks: Vec<ListedTrack>,
        playlists: Vec<Playlist>,
    },
    ProfileLoaded {
        generation: u64,
        profile: UserProfile,
    },
    CachedLikedTracks {
        generation: u64,
        tracks: Vec<ListedTrack>,
    },
    LocalStateLoaded {
        /// The pins as last read from Spotify, so the section is drawn
        /// before the network answers and still drawn without a session.
        pins: Pins,
        recently_played: Vec<Track>,
        /// The stored playlist order, so the list paints from what Cadence
        /// already knows instead of waiting on the network.
        library_index: LibraryIndex,
    },
    /// A refreshed pin set from the internal protocol.
    PinsLoaded {
        generation: u64,
        pins: Pins,
    },
    /// A refreshed playlist order from the internal protocol.
    LibraryOrderLoaded {
        generation: u64,
        index: LibraryIndex,
    },
    /// Playback started from a context that names itself, so the list can
    /// move it to the top before the next refresh confirms it.
    ContextPlayed {
        uri: String,
        played_at_ms: i64,
    },
    Playing {
        spotify_uri: String,
    },
    Loading {
        spotify_uri: String,
    },
    Paused {
        spotify_uri: String,
    },
    EndOfTrack {
        spotify_uri: String,
    },
    PositionChanged {
        spotify_uri: String,
        position_ms: u32,
    },
    PlaybackContext {
        current: Track,
        next: Vec<Track>,
        /// Which upcoming tracks are Smart Shuffle injections, aligned with
        /// `next`: the queue UI marks them with a distinct icon.
        injected: Vec<bool>,
    },
    ShuffleChanged {
        mode: ShuffleMode,
        supported: bool,
        smart_supported: bool,
    },
    PlaybackSnapshotLoaded {
        current: Track,
        next: Vec<Track>,
        /// Injected flags aligned with `next`, as [`BackendEvent::PlaybackContext`].
        injected: Vec<bool>,
        position_ms: u32,
    },
    AuthorizationFailed(String),
    CatalogFailed {
        generation: u64,
        error: String,
    },
    PlaybackFailed(String),
    TrackFailed {
        spotify_uri: String,
        error: String,
    },
    RadioFailed {
        request_id: u64,
        error: String,
    },
    RadioStarted {
        request_id: u64,
    },
    RadioCancelled {
        request_id: u64,
    },
    FatalError(String),
    Error(String),
}

struct Senders {
    commands: Sender<BackendCommand>,
    controls: Sender<BackendCommand>,
    volume: tokio::sync::watch::Sender<f32>,
}

/// The sending half of the backend. Holders keep it for the life of the process:
/// restarting the worker redirects the senders in place, so a handle taken
/// before a restart still reaches the worker running after it.
#[derive(Clone)]
pub struct BackendHandle {
    senders: Arc<std::sync::Mutex<Senders>>,
}

impl BackendHandle {
    pub fn send(&self, command: BackendCommand) -> bool {
        let senders = self
            .senders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        send_command(
            &senders.commands,
            &senders.controls,
            &senders.volume,
            command,
        )
    }

    fn redirect(&self, senders: Senders) {
        *self
            .senders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = senders;
    }
}

/// Owns the backend worker thread. Dropping this stops playback, so it is held
/// by a process-wide service rather than by a window.
pub struct Backend {
    handle: BackendHandle,
    /// This worker's own command channel, kept separate from `handle` because a
    /// restart redirects the handle at the replacement worker. Shutting down
    /// through the handle would stop the new worker instead of this one.
    commands: Sender<BackendCommand>,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Backend {
    pub fn start() -> (Self, UnboundedReceiver<BackendEvent>) {
        let (senders, shutdown, thread, events) = Self::spawn_worker();
        let commands = senders.commands.clone();
        (
            Self {
                handle: BackendHandle {
                    senders: Arc::new(std::sync::Mutex::new(senders)),
                },
                commands,
                shutdown,
                thread: Some(thread),
            },
            events,
        )
    }

    /// Starts a replacement worker and points `handle`, and every clone of it
    /// already handed out, at the new one.
    pub fn restart(handle: &BackendHandle) -> (Self, UnboundedReceiver<BackendEvent>) {
        let (senders, shutdown, thread, events) = Self::spawn_worker();
        let commands = senders.commands.clone();
        handle.redirect(senders);
        (
            Self {
                handle: handle.clone(),
                commands,
                shutdown,
                thread: Some(thread),
            },
            events,
        )
    }

    fn spawn_worker() -> (
        Senders,
        tokio::sync::watch::Sender<bool>,
        thread::JoinHandle<()>,
        UnboundedReceiver<BackendEvent>,
    ) {
        let (commands, command_receiver) = tokio::sync::mpsc::channel(COMMAND_CAPACITY);
        let (controls, control_receiver) = tokio::sync::mpsc::channel(CONTROL_CAPACITY);
        let (event_sender, events) = tokio::sync::mpsc::unbounded_channel();
        let (volume, volume_receiver) = tokio::sync::watch::channel(0.72);
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let thread = thread::Builder::new()
            .name("cadence-backend".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("could not start Tokio");
                let acknowledged = runtime.block_on(run(
                    command_receiver,
                    control_receiver,
                    event_sender,
                    volume_receiver,
                    shutdown_receiver,
                ));
                runtime.shutdown_timeout(Duration::from_secs(1));
                if let Some(acknowledged) = acknowledged {
                    let _ = acknowledged.send(());
                }
            })
            .expect("could not start the Cadence backend");
        (
            Senders {
                commands,
                controls,
                volume,
            },
            shutdown,
            thread,
            events,
        )
    }

    pub fn handle(&self) -> BackendHandle {
        self.handle.clone()
    }
}

fn send_command(
    commands: &Sender<BackendCommand>,
    controls: &Sender<BackendCommand>,
    volume_sender: &tokio::sync::watch::Sender<f32>,
    command: BackendCommand,
) -> bool {
    if matches!(
        &command,
        BackendCommand::Authenticate { .. } | BackendCommand::Logout { .. }
    ) {
        return controls.try_send(command).is_ok();
    }
    match command {
        BackendCommand::SetVolume(volume) => volume_sender.send(volume).is_ok(),
        command => commands.try_send(command).is_ok(),
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let (acknowledged, acknowledgment) = mpsc::channel();
        let _ = self.shutdown.send(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut command = BackendCommand::Shutdown { acknowledged };
        let sent = loop {
            match self.commands.try_send(command) {
                Ok(()) => break true,
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned))
                    if Instant::now() < deadline =>
                {
                    command = returned;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break false,
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let stopped = sent && acknowledgment.recv_timeout(remaining).is_ok();
        if stopped && let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Everything `run` needs once Spotify is usable.
struct Startup {
    store: BlockingStore,
    spotify: Spotify,
    configuration: Option<SpotifyConfiguration>,
    playback_snapshot: Option<PlaybackSnapshot>,
    playback_credentials_invalidated: bool,
}

/// The error carries the shutdown acknowledgment when the app quit before
/// setup finished, so the caller can hand it back to whoever asked to stop.
async fn start(
    commands: &mut Receiver<BackendCommand>,
    events: &UnboundedSender<BackendEvent>,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Startup, Option<StdSender<()>>> {
    let store = match BlockingStore::open_default().await {
        Ok(store) => store,
        Err(error) => {
            send_fatal_error(events, error);
            return Err(None);
        }
    };
    send_local_state(&store, events).await;
    let playback_snapshot = match store.playback_state().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            send_error(events, error);
            None
        }
    };
    if let Some(snapshot) = &playback_snapshot
        && let Some(current) = snapshot.tracks.get(snapshot.index)
    {
        let _ = events.send(BackendEvent::PlaybackSnapshotLoaded {
            current: current.clone(),
            next: snapshot
                .tracks
                .get(snapshot.index + 1..)
                .unwrap_or_default()
                .to_vec(),
            injected: injected_flags(&snapshot.shuffle, snapshot.index, snapshot.tracks.len()),
            position_ms: snapshot.position_ms,
        });
        // The restored queue may be shuffled; the toggle reflects that from
        // the first frame.
        let _ = events.send(BackendEvent::ShuffleChanged {
            mode: snapshot.shuffle.mode,
            supported: !snapshot.radio,
            smart_supported: smart_admissible(
                snapshot.context_kind,
                snapshot.radio,
                snapshot.shuffle.context.len(),
            ),
        });
    }
    let environment_client_id = std::env::var("SPOTIFY_CLIENT_ID").ok();
    let saved_client_id = match store.call(|store| store.spotify_client_id()).await {
        Ok(client_id) => client_id,
        Err(error) => {
            send_fatal_error(events, error);
            return Err(None);
        }
    };
    let oauth_credentials_invalidated = match store
        .call(|store| store.spotify_oauth_credentials_invalidated())
        .await
    {
        Ok(invalidated) => invalidated,
        Err(error) => {
            send_fatal_error(events, error);
            return Err(None);
        }
    };
    let mut playback_credentials_invalidated = match store
        .call(|store| store.spotify_playback_credentials_invalidated())
        .await
    {
        Ok(invalidated) => invalidated,
        Err(error) => {
            send_fatal_error(events, error);
            return Err(None);
        }
    };
    let mut configuration =
        resolve_configuration(environment_client_id.as_deref(), saved_client_id.as_deref());
    let mut configuration_generation = 0;
    if let Some(configured) = configuration.clone()
        && !valid_client_id(&configured.client_id)
    {
        if configured.source == ClientIdSource::Environment {
            let _ = events.send(BackendEvent::SpotifyConfigured {
                generation: 0,
                client_id: configured.client_id,
                source: ClientIdSource::Environment,
            });
            let _ = events.send(BackendEvent::SpotifyConfigurationFailed {
                generation: 0,
                error: "SPOTIFY_CLIENT_ID must contain 32 hexadecimal characters".to_owned(),
            });
            while let Some(command) = commands.recv().await {
                if let BackendCommand::Shutdown { acknowledged } = command {
                    return Err(Some(acknowledged));
                }
            }
            return Err(None);
        }
        if let Err(error) = store.call(|store| store.remove_spotify_client_id()).await {
            send_fatal_error(events, error);
            return Err(None);
        }
        configuration = None;
    }
    let spotify = loop {
        if let Some(configured) = &configuration {
            let spotify = tokio::select! {
                result = Spotify::from_client_id(
                    &configured.client_id,
                    !oauth_credentials_invalidated,
                ) => result,
                _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
            };
            match spotify {
                Ok(spotify) => break spotify,
                Err(error) => {
                    let _ = events.send(BackendEvent::SpotifyConfigurationFailed {
                        generation: configuration_generation,
                        error: error.to_string(),
                    });
                    if configured.source == ClientIdSource::Saved {
                        let _ = store.remove_spotify_client_id().await;
                        configuration = None;
                        continue;
                    }
                    return Err(None);
                }
            }
        }

        let _ = events.send(BackendEvent::SetupRequired);
        let command = tokio::select! {
            command = commands.recv() => command.ok_or(None)?,
            _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
        };
        match command {
            BackendCommand::ConfigureSpotify {
                generation,
                client_id,
            } if valid_client_id(&client_id) => {
                let client_id = client_id.trim().to_owned();
                let candidate = tokio::select! {
                    result = Spotify::from_client_id(&client_id, false) => result,
                    _ = wait_for_shutdown(shutdown) => return Err(receive_shutdown_acknowledgment(commands).await),
                };
                let candidate = match candidate {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        let _ = events.send(BackendEvent::SpotifyConfigurationFailed {
                            generation,
                            error: error.to_string(),
                        });
                        continue;
                    }
                };
                if let Err(error) = store.configure_spotify(client_id.clone()).await {
                    let _ = events.send(BackendEvent::SpotifyConfigurationFailed {
                        generation,
                        error: error.to_string(),
                    });
                    continue;
                }
                configuration = Some(SpotifyConfiguration {
                    client_id,
                    source: ClientIdSource::Saved,
                });
                configuration_generation = generation;
                playback_credentials_invalidated = true;
                break candidate;
            }
            BackendCommand::ConfigureSpotify { generation, .. } => {
                let _ = events.send(BackendEvent::SpotifyConfigurationFailed {
                    generation,
                    error: "Spotify Client ID must contain 32 hexadecimal characters".to_owned(),
                });
            }
            BackendCommand::Shutdown { acknowledged } => return Err(Some(acknowledged)),
            _ => {}
        }
    };
    let configured = configuration
        .as_ref()
        .expect("Spotify configuration must exist after setup");
    let _ = events.send(BackendEvent::SpotifyConfigured {
        generation: configuration_generation,
        client_id: configured.client_id.clone(),
        source: configured.source,
    });
    Ok(Startup {
        store,
        spotify,
        configuration,
        playback_snapshot,
        playback_credentials_invalidated,
    })
}

async fn run(
    mut commands: Receiver<BackendCommand>,
    mut controls: Receiver<BackendCommand>,
    events: UnboundedSender<BackendEvent>,
    mut volume: tokio::sync::watch::Receiver<f32>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Option<StdSender<()>> {
    let startup = match start(&mut commands, &events, &mut shutdown).await {
        Ok(startup) => startup,
        Err(acknowledged) => return acknowledged,
    };
    let (unavailable, mut unavailable_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (connected, mut connected_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut worker = Worker::new(startup, events, unavailable, connected);
    if let Err(acknowledged) = worker.boot(&mut commands, &mut shutdown).await {
        return acknowledged;
    }
    let mut playback_health = tokio::time::interval(std::time::Duration::from_secs(5));
    playback_health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let command = tokio::select! {
            command = controls.recv() => command?,
            command = commands.recv() => command?,
            _ = wait_for_shutdown(&mut shutdown) => {
                worker.session.finish_pending_logout().await;
                return receive_shutdown_acknowledgment(&mut commands).await;
            }
            changed = volume.changed() => {
                if changed.is_ok() && let Some(player) = &worker.connection.player {
                    player.set_volume(*volume.borrow_and_update());
                }
                continue;
            }
            _ = playback_health.tick() => {
                worker.connection.reconnect_if_dead(&worker.events);
                continue;
            }
            reconnected = finished(&mut worker.connection.reconnect) => {
                worker.connection.finish_reconnect(reconnected, &worker.events, &worker.unavailable);
                continue;
            }
            connected = finished(&mut worker.connection.connect) => {
                worker.finish_connect(connected).await;
                continue;
            }
            extended = finished(&mut worker.autoplay.task) => {
                worker.finish_autoplay(extended).await;
                continue;
            }
            stretch = finished(&mut worker.dj.task) => {
                worker.finish_dj_stretch(stretch).await;
                continue;
            }
            ready = finished(&mut worker.dj.voice) => {
                worker.finish_voice(ready);
                continue;
            }
            fetched = finished(&mut worker.injections.task) => {
                worker.finish_injection_fetch(fetched).await;
                continue;
            }
            radio = finished(&mut worker.radio.task) => {
                worker.finish_radio(radio).await;
                continue;
            }
            unavailable = unavailable_rx.recv() => {
                // The worker holds a sender, so the channel never closes.
                if let Some(spotify_uri) = unavailable {
                    worker.skip_unavailable_track(&spotify_uri).await;
                }
                continue;
            }
            connected = connected_rx.recv() => {
                if connected.is_some() {
                    worker.announce_device();
                }
                continue;
            }
            authorization = finished(&mut worker.session.authorization) => {
                worker.finish_authorization(authorization).await;
                continue;
            }
            logout = finished(&mut worker.session.logout) => {
                worker.finish_logout(logout);
                continue;
            }
        };
        if let Some(acknowledged) = worker.handle_command(command, &mut shutdown).await {
            return Some(acknowledged);
        }
    }
}

/// What a polled task slot produced: `None` when the slot was empty, otherwise
/// the task's output or its cancellation/panic error.
type Finished<T> = Option<Result<T, tokio::task::JoinError>>;

/// The running backend once Spotify is configured: the session state plus the
/// services that own the in-flight work.
struct Worker {
    events: UnboundedSender<BackendEvent>,
    /// Reports unplayable tracks from the player observer, so a dead
    /// current entry can auto-skip to the next one.
    unavailable: UnboundedSender<String>,
    /// Says that Spotify has issued a connection id, which is what a device
    /// state is tagged with: the first one is when Cadence can announce
    /// itself, and each later one follows a socket that reconnected.
    connected: UnboundedSender<()>,
    store: BlockingStore,
    spotify: Spotify,
    configuration: Option<SpotifyConfiguration>,
    playback_credentials_invalidated: bool,
    account_generation: u64,
    catalog_generation: Arc<AtomicU64>,
    queue: PlayQueue,
    connection: PlaybackConnection,
    catalog: CatalogFetches,
    radio: Radio,
    autoplay: Autoplay,
    dj: Dj,
    injections: Injections,
    /// Track ids Smart Shuffle has ever offered, persisted across restarts
    /// and toggle cycles; an offered track is never repeated within this set.
    smart_shuffle_seen: HashSet<String>,
    /// When the playlist order was last fetched, so switching windows every
    /// half minute does not turn into a rootlist walk every half minute.
    order_refreshed_at: Option<Instant>,
    session: SessionTasks,
}

impl Worker {
    fn new(
        startup: Startup,
        events: UnboundedSender<BackendEvent>,
        unavailable: UnboundedSender<String>,
        connected: UnboundedSender<()>,
    ) -> Self {
        Self {
            events,
            unavailable,
            connected,
            store: startup.store,
            spotify: startup.spotify,
            configuration: startup.configuration,
            playback_credentials_invalidated: startup.playback_credentials_invalidated,
            account_generation: 0,
            catalog_generation: Arc::new(AtomicU64::new(0)),
            queue: PlayQueue::from_snapshot(startup.playback_snapshot),
            connection: PlaybackConnection::default(),
            catalog: CatalogFetches::default(),
            radio: Radio::default(),
            autoplay: Autoplay::default(),
            dj: Dj::default(),
            injections: Injections::default(),
            smart_shuffle_seen: HashSet::new(),
            order_refreshed_at: None,
            session: SessionTasks::default(),
        }
    }

    /// Kicks off the signed-in account's loads and the playback connection.
    /// The error carries the shutdown acknowledgment when the app quit first.
    async fn boot(
        &mut self,
        commands: &mut Receiver<BackendCommand>,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), Option<StdSender<()>>> {
        let is_authorized = tokio::select! {
            authorized = self.spotify.is_authorized() => match authorized {
                Ok(authorized) => authorized,
                Err(error) => {
                    send_fatal_error(&self.events, error);
                    return Err(None);
                }
            },
            _ = wait_for_shutdown(shutdown) => {
                return Err(receive_shutdown_acknowledgment(commands).await);
            }
        };
        if !is_authorized {
            let _ = self.events.send(BackendEvent::AuthorizationRequired);
            return Ok(());
        }
        match self.store.liked_tracks().await {
            Ok(tracks) if !tracks.is_empty() => {
                let _ = self.events.send(BackendEvent::CachedLikedTracks {
                    generation: self.account_generation,
                    tracks,
                });
            }
            Ok(_) => {}
            Err(error) => send_error(&self.events, error),
        }
        self.start_account_loads().await;
        match self.store.smart_shuffle_seen().await {
            Ok(seen) => self.smart_shuffle_seen = seen.into_iter().collect(),
            Err(error) => send_error(&self.events, error),
        }
        self.connection.begin_connect(PlaybackConnectionRequest {
            load_saved_token: true,
            authorization: None,
        });
        Ok(())
    }

    /// Returns the shutdown acknowledgment once a `Shutdown` command arrives;
    /// every other command is handled in place.
    async fn handle_command(
        &mut self,
        command: BackendCommand,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Option<StdSender<()>> {
        if self.session.logout.is_some()
            && !matches!(
                &command,
                BackendCommand::Logout { .. } | BackendCommand::Shutdown { .. }
            )
        {
            send_error(&self.events, "Spotify logout is still finishing");
            return None;
        }
        let result = match command {
            BackendCommand::ResetSpotifyConfiguration { generation } => {
                self.reset_spotify_configuration(generation).await
            }
            BackendCommand::ConfigureSpotify {
                generation,
                client_id,
            } => {
                self.configure_spotify(generation, client_id, shutdown)
                    .await
            }
            BackendCommand::Authenticate { generation } => {
                self.authenticate(generation);
                Ok(())
            }
            BackendCommand::Logout { generation } => {
                self.logout(generation);
                Ok(())
            }
            BackendCommand::ReloadLibrary { respond } => {
                self.reload_library(respond);
                self.refresh_library_order(false);
                // A dropped socket ends the pin watch. Picking it back up
                // here costs one increment when it is already running.
                self.watch_pins();
                self.watch_connection_id();
                Ok(())
            }
            BackendCommand::SearchCatalog { query, respond } => {
                self.catalog.search(self.spotify.clone(), query, respond);
                Ok(())
            }
            BackendCommand::LoadPlaylist { playlist, respond } => {
                if dj::matches(&playlist.source_id) {
                    self.catalog.dj_lineup(
                        self.connection.player.clone(),
                        self.store.clone(),
                        respond,
                    );
                } else {
                    self.catalog
                        .playlist(self.spotify.clone(), playlist, respond);
                }
                Ok(())
            }
            BackendCommand::LoadArtist { source_id, respond } => {
                self.catalog
                    .artist(self.spotify.clone(), source_id, respond);
                Ok(())
            }
            BackendCommand::LoadAlbum { source_id, respond } => {
                self.catalog.album(self.spotify.clone(), source_id, respond);
                Ok(())
            }
            BackendCommand::StartRadio { request_id, seed } => {
                self.radio.start(
                    request_id,
                    seed,
                    self.connection.player.clone(),
                    self.spotify.clone(),
                    &self.events,
                );
                Ok(())
            }
            BackendCommand::PlayContext {
                tracks,
                index,
                shuffled,
                kind,
                context_uri,
            } => {
                // What was opened is also what the reported state names as
                // the context, so a play here reads on other devices the
                // way the same play from a phone would.
                self.queue.context_uri = context_uri.clone();
                if let Some(uri) = context_uri {
                    self.note_context_played(uri).await;
                }
                self.play_context(tracks, index, shuffled, kind).await
            }
            BackendCommand::PlayDj => self.start_dj(),
            BackendCommand::PlayNext(track) => self.play_next(track).await,
            BackendCommand::AppendToQueue(track) => self.append_to_queue(track).await,
            BackendCommand::SetShuffleMode(mode) => self.set_shuffle_mode(mode).await,
            BackendCommand::RestorePlayback {
                position_ms,
                playing,
            } => self.restore_playback(position_ms, playing).await,
            BackendCommand::SetLiked { track, liked } => self.set_liked(&track, liked).await,
            BackendCommand::SetPinned { uri, pinned } => self.set_pinned(uri, pinned).await,
            BackendCommand::MovePin { uri, target } => self.move_pin(uri, target).await,
            BackendCommand::Resume => self.resume().await,
            BackendCommand::Pause => self.pause().await,
            BackendCommand::Next => {
                self.dj.skipped = true;
                self.silence_narration();
                self.next_track(true).await
            }
            BackendCommand::Previous => {
                self.silence_narration();
                self.previous_track().await
            }
            BackendCommand::Seek(position_ms) => self.seek(position_ms).await,
            BackendCommand::SavePlaybackPosition {
                spotify_uri,
                position_ms,
            } => self.save_playback_position(spotify_uri, position_ms).await,
            // Unreachable in practice: send_command diverts SetVolume into the
            // volume watch, which the select loop applies directly.
            BackendCommand::SetVolume(volume) => self
                .connected_player()
                .map(|player| player.set_volume(volume)),
            BackendCommand::Shutdown { acknowledged } => {
                self.shutdown().await;
                return Some(acknowledged);
            }
        };
        if let Err(error) = result {
            send_error(&self.events, error);
        }
        None
    }

    async fn reset_spotify_configuration(&mut self, generation: u64) -> Result<()> {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        self.catalog_generation.store(generation, Ordering::Release);
        if self.configuration_is_from_environment() {
            let _ = self.events.send(BackendEvent::SpotifyConfigurationResetFailed(
                "SPOTIFY_CLIENT_ID is configured by the environment and cannot be changed in Cadence".to_owned(),
            ));
            return Ok(());
        }
        if let Err(error) = self.store.reset_spotify_configuration().await {
            let _ = self
                .events
                .send(BackendEvent::SpotifyConfigurationResetFailed(
                    error.to_string(),
                ));
            return Ok(());
        }
        self.abort_account_work();
        self.stop_playback_session();

        self.playback_credentials_invalidated = true;
        self.configuration = None;
        if let Err(error) = self.spotify.logout().await {
            send_error(&self.events, error);
        }
        if let Err(error) = delete_playback_refresh_token().await {
            send_error(&self.events, error);
        }
        if let Err(error) = self.store.clear_library_cache().await {
            send_error(&self.events, error);
        }
        if let Err(error) = self.store.clear_playback_state().await {
            send_error(&self.events, error);
        }
        if let Err(error) = self.store.clear_smart_shuffle_seen().await {
            send_error(&self.events, error);
        }
        let _ = self.events.send(BackendEvent::LoggedOut);
        let _ = self.events.send(BackendEvent::SetupRequired);
        Ok(())
    }

    async fn configure_spotify(
        &mut self,
        generation: u64,
        client_id: String,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        if self.configuration_is_from_environment() {
            self.send_configuration_failed(
                generation,
                "SPOTIFY_CLIENT_ID is configured by the environment and cannot be changed in Cadence",
            );
            return Ok(());
        }
        if self.configuration.is_some() {
            self.send_configuration_failed(
                generation,
                "Remove the current Spotify configuration before replacing it.",
            );
            return Ok(());
        }
        if !valid_client_id(&client_id) {
            self.send_configuration_failed(
                generation,
                "Spotify Client ID must contain 32 hexadecimal characters",
            );
            return Ok(());
        }
        let client_id = client_id.trim().to_owned();
        let candidate = tokio::select! {
            result = Spotify::from_client_id(&client_id, false) => result,
            _ = wait_for_shutdown(shutdown) => return Ok(()),
        };
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.send_configuration_failed(generation, error);
                return Ok(());
            }
        };
        if let Err(error) = self.store.configure_spotify(client_id.clone()).await {
            self.send_configuration_failed(generation, error);
            return Ok(());
        }
        self.spotify = candidate;
        self.playback_credentials_invalidated = true;
        self.configuration = Some(SpotifyConfiguration {
            client_id: client_id.clone(),
            source: ClientIdSource::Saved,
        });
        let _ = self.events.send(BackendEvent::SpotifyConfigured {
            generation,
            client_id,
            source: ClientIdSource::Saved,
        });
        let _ = self.events.send(BackendEvent::AuthorizationRequired);
        Ok(())
    }

    fn authenticate(&mut self, generation: u64) {
        abort_task(&mut self.connection.connect);
        let needs_playback = self
            .connection
            .player
            .as_ref()
            .is_none_or(|player| !player.is_connected());
        self.session.begin_authorization(
            generation,
            self.spotify.clone(),
            needs_playback,
            self.playback_credentials_invalidated,
        );
    }

    fn logout(&mut self, generation: u64) {
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        self.catalog_generation.store(generation, Ordering::Release);
        self.abort_account_work();
        self.stop_playback_session();
        self.playback_credentials_invalidated = true;
        self.session
            .begin_logout(self.store.clone(), self.spotify.clone());
    }

    /// Refetches the playlist order over the playback session. Without one
    /// there is nothing to fetch with: the stored order stays on screen,
    /// which is the documented fallback for an account that cannot stream.
    ///
    /// `force` skips the freshness gate, for a session that has only just
    /// come up and so has never fetched an order at all.
    fn refresh_library_order(&mut self, force: bool) {
        let Some(playback) = self.connection.player.clone() else {
            return;
        };
        if self
            .catalog
            .order
            .as_ref()
            .is_some_and(|running| !running.is_finished())
        {
            return;
        }
        let fresh = self
            .order_refreshed_at
            .is_some_and(|refreshed| refreshed.elapsed() < ORDER_REFRESH_INTERVAL);
        if fresh && !force {
            return;
        }
        self.order_refreshed_at = Some(Instant::now());
        let store = self.store.clone();
        let events = self.events.clone();
        let generation = self.account_generation;
        let current_generation = self.catalog_generation.clone();
        self.catalog.order = Some(tokio::spawn(async move {
            let refreshed = run_with_timeout(
                LIBRARY_TIMEOUT_SECONDS,
                "Spotify library order request",
                refresh_library_order(&playback, &store),
            )
            .await;
            if current_generation.load(Ordering::Acquire) != generation {
                return;
            }
            match refreshed {
                // Every source is behind a fallback: a failure here leaves
                // the stored order on screen rather than emptying the list.
                Ok(index) => {
                    let _ = events.send(BackendEvent::LibraryOrderLoaded { generation, index });
                }
                Err(error) => log::warn!("library order: refresh failed: {error:#}"),
            }
        }));
    }

    /// Follows the connection ids Spotify issues, and announces the device
    /// on each one.
    ///
    /// A device state is tagged with the id of the socket it belongs to, so
    /// there is nothing to report until the first id arrives, and a socket
    /// that reconnects issues another. Announcing again on each is how the
    /// account keeps knowing this device is here.
    fn watch_connection_id(&mut self) {
        let Some(playback) = self.connection.player.clone() else {
            return;
        };
        if self
            .catalog
            .connection_id
            .as_ref()
            .is_some_and(|running| !running.is_finished())
        {
            return;
        }
        let connected = self.connected.clone();
        self.catalog.connection_id = Some(tokio::spawn(async move {
            let mut ids = match playback.watch_connection_id().await {
                Ok(ids) => ids,
                Err(error) => {
                    log::warn!("connect: no connection id to report against: {error:#}");
                    return;
                }
            };
            while let Some(message) = ids.next().await {
                if playback.apply_connection_id(&message) {
                    let _ = connected.send(());
                }
            }
        }));
    }

    /// Announces the device to the account, with whatever it is playing.
    fn announce_device(&self) {
        self.report_playback(PutStateReason::NEW_DEVICE);
    }

    /// Tells Spotify what Cadence is playing.
    ///
    /// This is what puts a play into the account's history and moves the
    /// playlist up the library list on the listener's other devices. It
    /// runs on its own so a slow or refused report never holds up playback,
    /// and a failure is logged rather than shown: nothing the listener can
    /// hear depends on it.
    fn report_playback(&self, reason: PutStateReason) {
        let Some(playback) = self.connection.player.clone() else {
            return;
        };
        let report = self.queue.report();
        tokio::spawn(async move {
            if let Err(error) = playback.report_state(report.as_ref(), reason).await {
                log::warn!("connect: could not report what is playing: {error:#}");
            }
        });
    }

    /// Reads the pinned set over the playback session, then follows it for
    /// as long as the session lives.
    ///
    /// One task owns pins from end to end: the full read has to land before
    /// the subscription is opened, because the subscription reports what
    /// changes and never the set itself. Without a session there is nothing
    /// to read with and the stored pins stay on screen.
    fn watch_pins(&mut self) {
        let Some(playback) = self.connection.player.clone() else {
            return;
        };
        if self
            .catalog
            .pins
            .as_ref()
            .is_some_and(|running| !running.is_finished())
        {
            return;
        }
        let store = self.store.clone();
        let events = self.events.clone();
        let generation = self.account_generation;
        let current_generation = self.catalog_generation.clone();
        self.catalog.pins = Some(tokio::spawn(async move {
            let refreshed = |events: &UnboundedSender<BackendEvent>, pins| {
                let _ = events.send(BackendEvent::PinsLoaded { generation, pins });
            };
            // In full, not as an increment: the order pins are held in is
            // hand-made, and only a full read carries it.
            match run_with_timeout(
                LIBRARY_TIMEOUT_SECONDS,
                "Spotify pin request",
                read_pins(&playback, &store),
            )
            .await
            {
                Ok((pins, _)) => refreshed(&events, pins),
                // Pins sit behind a fallback like the rest of the order: a
                // failure leaves the stored set on screen.
                Err(error) => log::warn!("pins: first read failed: {error:#}"),
            }
            let mut updates = match playback.subscribe_pins().await {
                Ok(updates) => updates,
                Err(error) => {
                    log::warn!("pins: could not follow changes from other devices: {error:#}");
                    return;
                }
            };
            // Spotify says that the set moved, not how, so each message is
            // answered with an increment against the stored sync token.
            while updates.next().await.is_some() {
                if current_generation.load(Ordering::Acquire) != generation {
                    return;
                }
                match refresh_pins(&playback, &store).await {
                    Ok(pins) => refreshed(&events, pins),
                    Err(error) => log::warn!("pins: refresh failed: {error:#}"),
                }
            }
        }));
    }

    /// Records that Cadence started a context, in the index and on screen.
    /// The event goes out whether or not the write landed, so the row moves
    /// even when storage is failing; the next refresh settles the rest.
    async fn note_context_played(&mut self, uri: String) {
        let played_at_ms = chrono::Utc::now().timestamp_millis();
        if let Err(error) = self.store.set_last_played(uri.clone(), played_at_ms).await {
            log::warn!("library order: could not record the play: {error:#}");
        }
        let _ = self
            .events
            .send(BackendEvent::ContextPlayed { uri, played_at_ms });
    }

    fn reload_library(&mut self, respond: Reply<LibraryReload>) {
        // The boot load owns the first fetch; racing it would walk the whole
        // library twice. Answering Unchanged leaves the boot result standing.
        if self
            .catalog
            .library
            .as_ref()
            .is_some_and(|boot| !boot.is_finished())
        {
            let _ = respond.send(Ok(LibraryReload::Unchanged));
            return;
        }
        let spotify = self.spotify.clone();
        let store = self.store.clone();
        let generation = self.account_generation;
        let current_generation = self.catalog_generation.clone();
        let fingerprint = self.catalog.library_fingerprint.clone();
        abort_task(&mut self.catalog.reload);
        self.catalog.reload = Some(tokio::spawn(async move {
            let loaded =
                run_with_timeout(LIBRARY_TIMEOUT_SECONDS, "Spotify library request", async {
                    probe_and_load_library(&spotify, &fingerprint).await
                })
                .await;
            // Keep the on-disk copy in step so the next launch paints
            // the refreshed list before the network answers.
            let loaded = match loaded {
                Ok(ProbedLibrary::Unchanged) => Ok(LibraryReload::Unchanged),
                Ok(ProbedLibrary::Changed {
                    contents,
                    fingerprint: probed,
                }) => {
                    match persist_library_cache(
                        &store,
                        contents,
                        probed.clone(),
                        current_generation,
                        generation,
                    )
                    .await
                    {
                        Ok(Some(contents)) => {
                            commit_fingerprint(&fingerprint, probed);
                            Ok(LibraryReload::Fresh(contents))
                        }
                        Ok(None) => Err(anyhow!("Spotify account changed while loading")),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            let _ = respond.send(loaded);
        }));
    }

    async fn play_context(
        &mut self,
        mut tracks: Vec<Track>,
        index: usize,
        shuffled: bool,
        kind: ContextKind,
    ) -> Result<()> {
        self.stand_down_dj();
        self.radio.cancel(&self.events);
        let spotify_uri = tracks
            .get(index)
            .and_then(|track| track.spotify_uri.clone())
            .unwrap_or_default();
        // A fresh context inherits the global toggle: starting while Shuffle
        // is on begins shuffled, with the original order snapshotted first.
        let mut mode = self.queue.shuffle.mode;
        if shuffled && mode == ShuffleMode::Off {
            mode = ShuffleMode::Shuffle;
        }
        // An album, or a list too short to weave through, inherits plain
        // shuffle where the global toggle said Smart.
        if mode == ShuffleMode::Smart && !smart_admissible(kind, false, tracks.len()) {
            mode = ShuffleMode::Shuffle;
        }
        let mut shuffle = ShuffleState::for_context(&tracks, mode);
        if mode.shuffles() {
            shuffle.set_mode(
                ShuffleMode::Shuffle,
                &mut tracks,
                index,
                &mut ShuffleRng::from_entropy(),
            );
        }
        match load_context_track(
            &self.connection.player,
            &shuffle,
            &tracks,
            index,
            true,
            &self.store,
            &self.events,
        )
        .await
        {
            Ok(()) => {
                self.queue.tracks = tracks;
                self.queue.shuffle = shuffle;
                self.queue.radio = false;
                self.queue.kind = kind;
                // Injections belong to one session; a fresh context starts
                // its own, fetched only after this queue is playing.
                self.injections.reset();
                self.commit_loaded_queue(index).await;
            }
            Err(error) => {
                let _ = self.events.send(BackendEvent::TrackFailed {
                    spotify_uri,
                    error: error.to_string(),
                });
                let _ = self.events.send(BackendEvent::PlaybackSettled);
            }
        }
        Ok(())
    }

    async fn play_next(&mut self, track: Track) -> Result<()> {
        let index = self.queue.index.context("Nothing is currently playing")?;
        // Anchored straight after the playing track: shuffling and
        // unshuffling never move it.
        self.queue.tracks.insert(index + 1, track);
        self.queue.shuffle.insert_anchor(index + 1);
        self.commit_queue_change(index).await
    }

    async fn append_to_queue(&mut self, track: Track) -> Result<()> {
        let index = self.queue.index.context("Nothing is currently playing")?;
        self.queue.tracks.push(track);
        self.queue.shuffle.push_anchor();
        self.commit_queue_change(index).await
    }

    async fn set_shuffle_mode(&mut self, mode: ShuffleMode) -> Result<()> {
        let index = self.queue.index.context("Nothing is currently playing")?;
        if self.queue.radio {
            // A radio is already a recommendation stream; ignore the toggle.
            return Ok(());
        }
        if mode == ShuffleMode::Smart && !self.smart_supported() {
            // Albums and short lists have nothing for Smart to weave
            // through; the UI never offers it, so this is just defense.
            return Ok(());
        }
        if mode != ShuffleMode::Smart {
            // Leaving Smart drops upcoming injections first, so the
            // unshuffle below restores exactly the context order around
            // surviving anchors. An injected track already playing finishes.
            self.queue
                .shuffle
                .remove_upcoming_injections(&mut self.queue.tracks, index);
            self.injections.reset();
        }
        self.queue.shuffle.set_mode(
            mode,
            &mut self.queue.tracks,
            index,
            &mut ShuffleRng::from_entropy(),
        );
        self.commit_queue_change(index).await?;
        if mode == ShuffleMode::Smart {
            // Activation: the upcoming tail has no injections yet, so this
            // starts the first fetch right away.
            self.maybe_prefetch_injections().await;
        }
        Ok(())
    }

    /// Whether Smart Shuffle could act right now: a live, non-radio,
    /// playlist-like context long enough to weave through.
    fn smart_supported(&self) -> bool {
        self.queue.index.is_some()
            && smart_admissible(
                self.queue.kind,
                self.queue.radio,
                self.queue.shuffle.context.len(),
            )
    }

    /// Whether Smart Shuffle is currently weaving injections into this
    /// queue's tail.
    fn smart_active(&self) -> bool {
        self.queue.shuffle.mode == ShuffleMode::Smart && !self.queue.radio
    }

    /// Saves the queue as it stands and tells the UI about both halves of
    /// the change: the play order and the toggle behind it.
    async fn commit_queue_change(&mut self, index: usize) -> Result<()> {
        self.store
            .set_playback_state(
                self.queue.tracks.clone(),
                index,
                self.queue.position_ms,
                self.queue.shuffle.clone(),
                self.queue.radio,
                self.queue.kind,
            )
            .await?;
        send_playback_context(&self.queue.shuffle, &self.queue.tracks, index, &self.events);
        send_shuffle_changed(&self.queue, &self.events);
        Ok(())
    }

    async fn restore_playback(&mut self, position_ms: u32, playing: bool) -> Result<()> {
        let result = restore_context_track(
            &self.connection.player,
            &self.queue.shuffle,
            &self.queue.tracks,
            self.queue.index,
            position_ms,
            playing,
            &self.events,
        );
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled);
            return result;
        }
        let _ = self.events.send(BackendEvent::PlaybackRestored {
            position_ms,
            playing,
        });
        // The restore performed a fresh load; the ended special-casing must
        // not survive it, or seeks would be swallowed against a live player.
        self.queue.ended = false;
        self.queue.play_requested = playing;
        self.queue.current_unavailable = false;
        self.queue.position_ms = position_ms;
        if let Err(error) = self.store.update_playback_position(position_ms).await {
            send_error(&self.events, error);
        }
        if playing {
            self.top_up_queue();
            self.maybe_prefetch_injections().await;
        }
        result
    }

    /// Likes or unlikes a track on Spotify, which owns the collection. The
    /// window has already moved its heart, so a failure here surfaces as an
    /// error and the next library refresh puts the heart back.
    async fn set_liked(&mut self, track: &Track, liked: bool) -> Result<()> {
        self.spotify.set_liked(&track.source_id, liked).await
    }

    /// Pins or unpins one item on the account, then tells the window what
    /// the set now holds.
    async fn set_pinned(&mut self, uri: String, pinned: bool) -> Result<()> {
        let written = if pinned {
            self.write_whole_set(|pins| pins.pin(&uri)).await
        } else {
            self.write_removal(&uri).await
        };
        self.settle_pins(written).await
    }

    /// Moves a pin to where another one sits.
    async fn move_pin(&mut self, uri: String, target: String) -> Result<()> {
        let written = self
            .write_whole_set(|pins| pins.move_onto(&uri, &target))
            .await;
        self.settle_pins(written).await
    }

    /// Tells the window what the pinned set holds now.
    ///
    /// The window has already moved the button or the row, so a refusal has
    /// to put it back: the pins that go out on the way to the error are the
    /// ones Spotify actually holds, not the ones the gesture hoped for.
    async fn settle_pins(&mut self, written: Result<Pins>) -> Result<()> {
        let pins = match written {
            Ok(pins) => pins,
            Err(error) => {
                // Only a set that was actually read can roll the window
                // back. Storage failing too is no reason to send a window
                // full of pins away as an empty section.
                match self.store.pins().await {
                    Ok(stored) => self.send_pins(stored),
                    Err(error) => log::warn!("pins: nothing to roll back to: {error:#}"),
                }
                return Err(error);
            }
        };
        self.send_pins(pins);
        Ok(())
    }

    /// Applies `change` to the account's pins and writes the whole set.
    ///
    /// The set is re-read and merged first: a write replaces what Spotify
    /// holds, and between Cadence's last read and this gesture another
    /// device may have pinned something the write would otherwise unpin.
    /// Pinning and reordering both come through here, because both send the
    /// whole set.
    async fn write_whole_set(&mut self, change: impl FnOnce(&mut Pins)) -> Result<Pins> {
        let playback = self.pin_writer()?;
        let local = self.store.pins().await?;
        let (mut pins, complete) = read_pins(&playback, &self.store).await?;
        // Half a set must never go: what did not arrive would be unpinned.
        if !complete {
            bail!("Spotify sent only part of the pinned set, so nothing was written");
        }
        pins.merge(&local);
        change(&mut pins);
        playback.write_pins(pins.write_items(now_seconds())).await?;
        self.store_pins(pins).await
    }

    /// Unpins one item: a single removal rather than a replacement, so
    /// nothing has to be read first. The official client does the same.
    async fn write_removal(&mut self, uri: &str) -> Result<Pins> {
        let playback = self.pin_writer()?;
        playback.write_pins(Pins::removal_items(uri)).await?;
        let mut pins = self.store.pins().await?;
        pins.unpin(uri);
        self.store_pins(pins).await
    }

    fn pin_writer(&self) -> Result<Playback> {
        self.connection
            .player
            .clone()
            .context("Cadence is not connected to Spotify")
    }

    /// Keeps the set a write left behind.
    ///
    /// Spotify has the change already, so storage failing here is a stale
    /// cache and not a refusal: the next read settles it, and rolling the
    /// window back would be a lie. The sync token stays as the last read
    /// left it — a write is not a read, and Spotify hands none back.
    async fn store_pins(&mut self, pins: Pins) -> Result<Pins> {
        if let Err(error) = self.store.set_pins(pins.clone(), None).await {
            log::warn!("pins: the change landed but could not be stored: {error:#}");
        }
        Ok(pins)
    }

    fn send_pins(&self, pins: Pins) {
        let _ = self.events.send(BackendEvent::PinsLoaded {
            generation: self.account_generation,
            pins,
        });
    }

    async fn resume(&mut self) -> Result<()> {
        if self.queue.ended {
            // play() is a no-op in librespot's EndOfTrack state: reload the
            // current track at the seeker's position instead.
            let result = restore_context_track(
                &self.connection.player,
                &self.queue.shuffle,
                &self.queue.tracks,
                self.queue.index,
                self.queue.position_ms,
                true,
                &self.events,
            );
            match &result {
                Ok(()) => {
                    self.queue.ended = false;
                    self.queue.play_requested = true;
                    self.queue.current_unavailable = false;
                    // Replaying the still-last track re-arms the prefetch a
                    // failed earlier fetch may have left unarmed.
                    self.top_up_queue();
                }
                Err(_) => {
                    let _ = self.events.send(BackendEvent::PlaybackSettled);
                }
            }
            return result;
        }
        if self.queue.current_unavailable {
            // The current track already failed to load; play() against the
            // dead loader would silently do nothing. Advance instead, and
            // the never-heard track stays out of the history.
            self.queue.current_unavailable = false;
            return self.next_track(false).await;
        }
        let result = self.connected_player().map(|player| player.play());
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled);
        } else {
            // Playback continues from the original playing load.
            self.queue.play_requested = true;
            self.report_playback(PutStateReason::PLAYER_STATE_CHANGED);
            self.top_up_queue();
        }
        result
    }

    async fn pause(&mut self) -> Result<()> {
        let result = self.connected_player().map(|player| player.pause());
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled);
        } else {
            // A paused queue must not auto-skip on a late failure report.
            self.queue.play_requested = false;
            self.report_playback(PutStateReason::PLAYER_STATE_CHANGED);
        }
        result
    }

    /// Moves to the next queue entry; `record_history` gates whether the
    /// entry being left counts as heard. Auto-advance paths pass `false`
    /// so tracks that never played a second are not logged as listened.
    /// Cuts short whatever the DJ is saying. Skipping moves the player
    /// without pausing or stopping it, so the voice would otherwise finish
    /// the line over the song the listener asked for.
    fn silence_narration(&self) {
        if let Ok(player) = self.connected_player() {
            player.silence_narration();
        }
    }

    async fn next_track(&mut self, record_history: bool) -> Result<()> {
        self.radio.cancel(&self.events);
        let Some(current) = self.queue.index else {
            let _ = self.events.send(BackendEvent::QueueEnded);
            return Ok(());
        };
        let next = current
            .checked_add(1)
            .filter(|index| *index < self.queue.tracks.len());
        let Some(index) = next else {
            // Nothing left to play: keep the last track current, but rewind
            // the seeker so Play visibly means "from the start". Skipping
            // forward mid-song lands here too, so silence the player rather
            // than showing a stopped UI over audio that keeps going.
            if let Ok(player) = self.connected_player() {
                player.stop();
            }
            self.queue.ended = true;
            self.queue.play_requested = false;
            self.queue.current_unavailable = false;
            self.queue.position_ms = 0;
            if let Err(error) = self.store.update_playback_position(0).await {
                send_error(&self.events, error);
            }
            let _ = self.events.send(BackendEvent::QueueEnded);
            // Nothing is playing here any more, and the account should not
            // go on thinking otherwise.
            self.report_playback(PutStateReason::PLAYER_STATE_CHANGED);
            // Late fallback: if the song outran the autoplay prefetch (or
            // none ran), this fetch resumes playback on arrival.
            self.top_up_queue();
            return Ok(());
        };
        self.load_queue_track(index, record_history).await
    }

    /// Advances past the playing track when Spotify reports it unplayable,
    /// so one dead entry cannot stall the rest of the queue. When auto-skip
    /// does not apply — a paused restore — the dead current entry is noted
    /// so Resume can escape it.
    async fn skip_unavailable_track(&mut self, spotify_uri: &str) {
        if !self.queue.should_auto_skip_unavailable(spotify_uri) {
            self.queue.note_unavailable_while_paused(spotify_uri);
            return;
        }
        log::info!("playback: skipping unavailable track {spotify_uri}");
        if let Err(error) = self.next_track(false).await {
            send_error(&self.events, error);
        }
    }

    async fn previous_track(&mut self) -> Result<()> {
        self.radio.cancel(&self.events);
        if let Some(index) = self.queue.index.and_then(|index| index.checked_sub(1)) {
            return self.load_queue_track(index, true).await;
        }
        let result = self.connected_player().map(|player| player.seek(0));
        let _ = self.events.send(BackendEvent::PlaybackSettled);
        if result.is_ok() {
            self.queue.position_ms = 0;
            if let Err(error) = self.store.update_playback_position(0).await {
                send_error(&self.events, error);
            }
        }
        result
    }

    /// Loads the queue entry at `index` and persists it as the playing track.
    async fn load_queue_track(&mut self, index: usize, record_history: bool) -> Result<()> {
        self.speak_before(index);
        let result = load_context_track(
            &self.connection.player,
            &self.queue.shuffle,
            &self.queue.tracks,
            index,
            record_history,
            &self.store,
            &self.events,
        )
        .await;
        if result.is_err() {
            let _ = self.events.send(BackendEvent::PlaybackSettled);
            return result;
        }
        self.commit_loaded_queue(index).await;
        result
    }

    /// The invariant after any successful queue-track load: current index,
    /// rewound position, a live (not ended) queue, a persisted snapshot,
    /// armed prefetches — autoplay when the track is the queue's last, and
    /// an injection refill whenever Smart has thinned the upcoming tail.
    async fn commit_loaded_queue(&mut self, index: usize) {
        self.queue.index = Some(index);
        self.queue.position_ms = 0;
        self.queue.ended = false;
        self.queue.current_unavailable = false;
        // Queue loads always start playing.
        self.queue.play_requested = true;
        // A new track is a new play, and a play of its own to report.
        self.queue.playback_id = format!("{:032x}", rand::random::<u128>());
        self.report_playback(PutStateReason::PLAYER_STATE_CHANGED);
        if let Err(error) = self.commit_queue_change(index).await {
            send_error(&self.events, error);
        }
        self.top_up_queue();
        self.prepare_next_line(index);
        // The advance consumed an injection from the upcoming tail; this
        // refills before it runs dry.
        self.maybe_prefetch_injections().await;
    }

    async fn seek(&mut self, position_ms: u32) -> Result<()> {
        // While ended, seek() would be a no-op too; remember the position for
        // the reload that Resume performs.
        if !self.queue.ended {
            self.connected_player()
                .map(|player| player.seek(position_ms))?;
        }
        self.queue.position_ms = position_ms;
        if let Err(error) = self.store.update_playback_position(position_ms).await {
            send_error(&self.events, error);
        }
        Ok(())
    }

    async fn save_playback_position(
        &mut self,
        spotify_uri: String,
        position_ms: u32,
    ) -> Result<()> {
        if self.queue.current_uri() != Some(spotify_uri.as_str()) {
            return Ok(());
        }
        self.queue.position_ms = position_ms;
        self.store.update_playback_position(position_ms).await
    }

    async fn shutdown(&mut self) {
        self.abort_account_work();
        abort_task(&mut self.session.authorization);
        abort_task(&mut self.connection.connect);
        if let Some(task) = self.session.logout.take()
            && let Err(error) = task.await
        {
            send_error(&self.events, error);
        }
        self.connection.abort_attempts();
        self.connection.disconnect();
    }

    async fn finish_connect(&mut self, connected: Finished<Result<Playback>>) {
        self.connection.connect = None;
        match connected {
            Some(Ok(Ok(player))) => {
                log::info!("playback: connected");
                self.connection
                    .adopt(player, &self.events, &self.unavailable);
                self.playback_credentials_invalidated = false;
                if let Err(error) = self.store.set_playback_credentials_invalidated(false).await {
                    send_error(&self.events, error);
                }
                let _ = self.events.send(BackendEvent::PlaybackReady);
                // The order and the pins need this session: it is the only
                // thing that can read the rootlist, the play history and
                // the pinned set. Reporting what Cadence plays needs it too.
                self.refresh_library_order(true);
                self.watch_pins();
                self.watch_connection_id();
                if self.connection.connect_restoring {
                    let _ = self.events.send(BackendEvent::PlaybackReconnected);
                } else if let Err(error) = restore_saved_playback(
                    &self.connection.player,
                    &self.queue.shuffle,
                    &self.queue.tracks,
                    self.queue.index,
                    self.queue.position_ms,
                    &self.events,
                ) {
                    send_error(&self.events, error);
                }
            }
            Some(Ok(Err(error))) => {
                let _ = self
                    .events
                    .send(BackendEvent::PlaybackFailed(error.to_string()));
            }
            Some(Err(error)) => send_error(&self.events, error),
            None => {}
        }
        self.connection.connect_restoring = false;
    }

    async fn finish_radio(&mut self, radio: Finished<(u64, Result<Vec<Track>>)>) {
        self.radio.task = None;
        self.radio.request_id = None;
        match radio {
            Some(Ok((request_id, Ok(tracks)))) => {
                // A radio queue carries no injections, so its context event
                // goes out with empty bookkeeping.
                match load_context_track(
                    &self.connection.player,
                    &ShuffleState::default(),
                    &tracks,
                    0,
                    true,
                    &self.store,
                    &self.events,
                )
                .await
                {
                    Ok(()) => {
                        self.stand_down_dj();
                        self.queue.tracks = tracks;
                        // A radio ignores shuffle, but the global mode
                        // survives underneath so a later context inherits it.
                        self.queue.shuffle =
                            ShuffleState::for_context(&self.queue.tracks, self.queue.shuffle.mode);
                        self.queue.radio = true;
                        self.commit_loaded_queue(0).await;
                        let _ = self.events.send(BackendEvent::RadioStarted { request_id });
                    }
                    Err(error) => {
                        let _ = self.events.send(BackendEvent::RadioFailed {
                            request_id,
                            error: error.to_string(),
                        });
                        let _ = self.events.send(BackendEvent::PlaybackSettled);
                    }
                }
            }
            Some(Ok((request_id, Err(error)))) => {
                let _ = self.events.send(BackendEvent::RadioFailed {
                    request_id,
                    error: error.to_string(),
                });
                let _ = self.events.send(BackendEvent::PlaybackSettled);
            }
            Some(Err(error)) => send_error(&self.events, error),
            None => {}
        }
    }

    async fn finish_authorization(
        &mut self,
        authorization: Finished<(u64, Result<AuthorizationSuccess>)>,
    ) {
        self.session.authorization = None;
        match authorization {
            Some(Ok((generation, Ok(success)))) => {
                if let Err(error) = self.store.set_oauth_credentials_invalidated(false).await {
                    send_error(&self.events, error);
                }
                self.account_generation = generation;
                self.catalog_generation.store(generation, Ordering::Release);
                self.abort_account_work();
                self.start_account_loads().await;
                if let Some(request) = success.playback {
                    self.connection.begin_connect(request);
                }
            }
            Some(Ok((_, Err(error)))) => {
                let _ = self
                    .events
                    .send(BackendEvent::AuthorizationFailed(error.to_string()));
            }
            Some(Err(error)) => send_error(&self.events, error),
            None => {}
        }
    }

    fn finish_logout(&mut self, logout: Finished<Result<()>>) {
        self.session.logout = None;
        match logout {
            Some(Ok(Ok(()))) => {
                let _ = self.events.send(BackendEvent::LoggedOut);
            }
            Some(Ok(Err(error))) => {
                let _ = self.events.send(BackendEvent::LoggedOut);
                send_error(&self.events, error);
            }
            Some(Err(error)) => send_error(&self.events, error),
            None => {}
        }
    }

    /// Starts the library load for the signed-in account.
    async fn start_account_loads(&mut self) {
        self.catalog.load_library(
            self.spotify.clone(),
            self.store.clone(),
            self.account_generation,
            self.catalog_generation.clone(),
            self.events.clone(),
        );
    }

    /// Stops every task tied to the signed-in account: catalog loads and radio.
    fn abort_account_work(&mut self) {
        self.catalog.abort_all();
        self.radio.cancel(&self.events);
        abort_task(&mut self.autoplay.task);
        self.injections.reset();
        // The next account's order has not been fetched at all.
        self.order_refreshed_at = None;
    }

    /// Drops the live playback session and forgets the queue.
    fn stop_playback_session(&mut self) {
        self.connection.disconnect();
        abort_task(&mut self.connection.reconnect);
        self.connection.reconnect_pending = false;
        self.queue.tracks.clear();
        self.queue.shuffle = ShuffleState::default();
        self.queue.radio = false;
        self.queue.kind = ContextKind::default();
        self.queue.index = None;
        // A fresh session must not inherit the ended special-casing.
        self.queue.ended = false;
        // Offered history belongs to the signed-in account; the store copy
        // is cleared by the logout/config-reset flows.
        self.smart_shuffle_seen.clear();
    }

    /// Keeps the queue from running out. The DJ station follows its own
    /// cursor, which is where the session decides what comes next; every
    /// other context falls back to autoplay radio.
    fn top_up_queue(&mut self) {
        if self.dj.playing {
            self.maybe_extend_dj();
        } else {
            self.maybe_prefetch_autoplay();
        }
    }

    /// Starts the DJ station. The persisted cursor is followed when there
    /// is one, so a restart continues where the listener left off rather
    /// than replaying the opening stretch — starting a session afresh
    /// always returns that same stretch, however far the station has moved.
    fn start_dj(&mut self) -> Result<()> {
        abort_task(&mut self.dj.task);
        self.dj.playing = false;
        let store = self.store.clone();
        self.dj.task = Some(fetch_dj_stretch(
            self.connection.player.clone(),
            async move { store.call(|store| store.dj_cursor()).await },
        ));
        Ok(())
    }

    /// Fetches the next stretch as the queued one runs low, so the station
    /// never stops to wait for the network.
    fn maybe_extend_dj(&mut self) {
        let Some(index) = self.queue.index else {
            return;
        };
        if index + DJ_TOP_UP_LEAD < self.queue.tracks.len() || self.dj.task.is_some() {
            return;
        }
        let cursor = self.dj.next_page_url.clone();
        self.dj.task = Some(fetch_dj_stretch(
            self.connection.player.clone(),
            async move { Ok(cursor) },
        ));
    }

    /// Takes a fetched stretch: the opening one starts the station, a
    /// later one extends the queue behind whatever is playing.
    async fn finish_dj_stretch(&mut self, stretch: Finished<Result<DjStretch>>) {
        self.dj.task = None;
        let mut stretch = match stretch {
            Some(Ok(Ok(stretch))) => stretch,
            Some(Ok(Err(error))) => {
                send_error(&self.events, error);
                return;
            }
            Some(Err(error)) => {
                send_error(&self.events, error);
                return;
            }
            None => return,
        };
        // Fetching a stretch is what moves the session's cursor, so the
        // one it hands back is what a restart must resume from.
        let cursor = stretch.next_page_url.clone();
        let stored = stretch.next_page_url.take();
        let store = self.store.clone();
        if let Err(error) = store
            .call(move |store| store.set_dj_cursor(stored.as_deref()))
            .await
        {
            send_error(&self.events, error);
        }
        if stretch.tracks.is_empty() {
            return;
        }
        if self.dj.playing {
            self.dj.next_page_url = cursor;
            self.dj.lines.extend(stretch.lines);
            let Some(index) = self.queue.index else {
                return;
            };
            let known: HashSet<&str> = self
                .queue
                .tracks
                .iter()
                .map(|track| track.source_id.as_str())
                .collect();
            let additions: Vec<Track> = stretch
                .tracks
                .iter()
                .filter(|track| !known.contains(track.source_id.as_str()))
                .cloned()
                .collect();
            log::info!(
                "dj: stretch of {} songs, {} new for the queue",
                stretch.tracks.len(),
                additions.len(),
            );
            if additions.is_empty() {
                return;
            }
            let ended = self.queue.ended;
            for track in additions {
                self.queue.tracks.push(track);
                self.queue.shuffle.push_anchor();
            }
            if let Err(error) = self.commit_queue_change(index).await {
                send_error(&self.events, error);
                return;
            }
            if ended {
                // Skipping outran the fetch and left the player parked on
                // the last song. The stretch that just landed is what it
                // was waiting for.
                if let Err(error) = self.load_queue_track(index + 1, true).await {
                    send_error(&self.events, error);
                }
            }
            // One stretch may not be the whole runway. Asking again now
            // that this fetch has landed keeps to one request at a time.
            self.maybe_extend_dj();
            return;
        }
        log::info!(
            "dj: station starting with {} songs, cursor {:?}",
            stretch.tracks.len(),
            cursor,
        );
        // Every later song has the one before it to be prepared during;
        // the opening line has nothing, so it is synthesized here and
        // queued before the first song reaches the device.
        let opening = self.opening_line(&stretch).await;
        if let (Some(clip), Ok(player)) = (opening, self.connected_player()) {
            player.speak(clip);
        }
        // The station takes the queue over the way a track radio does,
        // rather than through `play_context`: the running order is the
        // DJ's, so it must not inherit the shuffle toggle the way an
        // ordinary context does.
        self.radio.cancel(&self.events);
        if let Err(error) = load_context_track(
            &self.connection.player,
            &ShuffleState::default(),
            &stretch.tracks,
            0,
            true,
            &self.store,
            &self.events,
        )
        .await
        {
            send_error(&self.events, error);
            let _ = self.events.send(BackendEvent::PlaybackSettled);
            return;
        }
        self.queue.tracks = stretch.tracks;
        // The global mode survives underneath without reordering anything,
        // so a later context still inherits whatever the listener chose.
        self.queue.shuffle = ShuffleState::for_context(&self.queue.tracks, self.queue.shuffle.mode);
        self.queue.radio = true;
        self.queue.kind = ContextKind::Collection;
        self.injections.reset();
        self.dj.playing = true;
        self.dj.next_page_url = cursor;
        self.dj.lines = stretch.lines;
        let _ = self.events.send(BackendEvent::StationChanged(true));
        self.commit_loaded_queue(0).await;
    }

    /// Synthesizes what the DJ opens a starting station with, if anything.
    async fn opening_line(&self, stretch: &DjStretch) -> Option<NarrationClip> {
        let uri = stretch.tracks.first()?.spotify_uri.as_deref()?;
        let line = stretch.lines.get(uri)?.line(false)?;
        match self.connected_player().ok()?.synthesize(line).await {
            Ok(clip) => Some(clip),
            Err(error) => {
                log::warn!("dj: narration synthesis failed: {error:#}");
                None
            }
        }
    }

    /// Drops the station's state: another context has the player, and a
    /// stale cursor must not extend somebody else's queue, nor a prepared
    /// line interrupt its music.
    fn stand_down_dj(&mut self) {
        abort_task(&mut self.dj.task);
        abort_task(&mut self.dj.voice);
        if self.dj.playing {
            let _ = self.events.send(BackendEvent::StationChanged(false));
        }
        self.dj = Dj::default();
    }

    /// Hands the sink whatever the DJ prepared for the song at `index`,
    /// before the song itself is loaded, so the voice is queued ahead of
    /// the music instead of over it.
    fn speak_before(&mut self, index: usize) {
        let after_skip = std::mem::take(&mut self.dj.skipped);
        if !self.dj.playing {
            return;
        }
        let Some(uri) = self
            .queue
            .tracks
            .get(index)
            .and_then(|track| track.spotify_uri.as_deref())
        else {
            return;
        };
        let Some(ready) = self.dj.ready.take().filter(|ready| ready.uri == uri) else {
            return;
        };
        // A skip is answered with the DJ's own "moving on" line where it
        // prepared one; its introduction still fits when it did not.
        let clip = if after_skip {
            ready.jump.or(ready.intro)
        } else {
            ready.intro
        };
        if let (Some(clip), Ok(player)) = (clip, self.connected_player()) {
            player.speak(clip);
        }
    }

    /// Synthesizes the line for the song after `index` while the current
    /// one plays. Without this the voice would arrive after the music has
    /// already started.
    fn prepare_next_line(&mut self, index: usize) {
        if !self.dj.playing || self.dj.voice.is_some() {
            return;
        }
        let Some(uri) = self
            .queue
            .tracks
            .get(index + 1)
            .and_then(|track| track.spotify_uri.clone())
        else {
            return;
        };
        if self.dj.ready.as_ref().is_some_and(|ready| ready.uri == uri) {
            return;
        }
        // Most songs carry nothing to say; remembering that is what stops
        // this from asking again at every advance.
        let Some(prepared) = self.dj.lines.get(&uri).cloned() else {
            self.dj.ready = Some(ReadyLine {
                uri,
                intro: None,
                jump: None,
            });
            return;
        };
        let Ok(player) = self.connected_player().cloned() else {
            return;
        };
        self.dj.voice = Some(tokio::spawn(async move {
            let mut ready = ReadyLine {
                uri,
                intro: None,
                jump: None,
            };
            for after_skip in [false, true] {
                let Some(line) = prepared.line(after_skip) else {
                    continue;
                };
                match player.synthesize(line).await {
                    // A line that cannot be synthesized costs the voice
                    // and nothing else; the music still plays.
                    Err(error) => log::warn!("dj: narration synthesis failed: {error:#}"),
                    Ok(clip) if after_skip => ready.jump = Some(clip),
                    Ok(clip) => ready.intro = Some(clip),
                }
            }
            ready
        }));
    }

    /// Takes a synthesized line, ready for whenever its song starts.
    fn finish_voice(&mut self, ready: Finished<ReadyLine>) {
        self.dj.voice = None;
        if let Some(Ok(ready)) = ready {
            self.dj.ready = Some(ready);
        }
    }

    /// Starts a radio prefetch when the playing track is the queue's last,
    /// so autoplay can extend the queue before it runs out. The preference
    /// is read inside the task and re-checked when the result lands, so a
    /// toggle takes effect without replumbing.
    fn maybe_prefetch_autoplay(&mut self) {
        let Some(index) = self.queue.index else {
            return;
        };
        if index + 1 < self.queue.tracks.len() {
            return;
        }
        let Some(seed) = self.queue.tracks.get(index).cloned() else {
            return;
        };
        if self.autoplay.fruitless_seed.as_deref() == Some(seed.source_id.as_str()) {
            return;
        }
        if self.autoplay.task.is_some()
            && self.autoplay.seed_id.as_deref() == Some(seed.source_id.as_str())
        {
            return;
        }
        // Any pending fetch at this point is seeded on music the listener
        // has moved away from; its result must never touch this queue.
        abort_task(&mut self.autoplay.task);
        self.autoplay.seed_id = Some(seed.source_id.clone());
        let player = self.connection.player.clone();
        let spotify = self.spotify.clone();
        let store = self.store.clone();
        self.autoplay.task = Some(tokio::spawn(async move {
            if !store.call(|store| store.preferences()).await?.autoplay {
                return Ok(None);
            }
            run_with_timeout(60, "Spotify autoplay radio", async {
                let player = player.context("Spotify playback is not connected")?;
                let seed_uri = seed
                    .spotify_uri
                    .as_deref()
                    .context("autoplay seed has no Spotify track URI")?;
                recommendation_tracks(&player, &spotify, seed_uri)
                    .await
                    .map(Some)
            })
            .await
        }));
    }

    /// Extends the queue with a finished autoplay prefetch, and picks the
    /// music back up when the song outran the fetch.
    async fn finish_autoplay(&mut self, extended: Finished<Result<Option<Vec<Track>>>>) {
        self.autoplay.task = None;
        let seed_id = self.autoplay.seed_id.take();
        let tracks = match extended {
            Some(Ok(Ok(Some(tracks)))) => tracks,
            // The preference was off when the task looked: not a dry seed.
            Some(Ok(Ok(None))) => return,
            Some(Ok(Err(error))) => {
                log::warn!("autoplay: radio prefetch failed: {error:#}");
                self.autoplay.fruitless_seed = seed_id;
                return;
            }
            Some(Err(error)) => {
                send_error(&self.events, error);
                return;
            }
            None => return,
        };
        // The queue may have moved on while the prefetch ran; a result for
        // any other seed than the still-playing last track is stale.
        let Some(index) = self.queue.index else {
            return;
        };
        if index + 1 < self.queue.tracks.len() {
            return;
        }
        let current_seed = self
            .queue
            .tracks
            .get(index)
            .map(|track| track.source_id.as_str());
        if seed_id.as_deref() != current_seed {
            return;
        }
        // The listener may have switched autoplay off while this ran.
        match self.store.call(|store| store.preferences()).await {
            Ok(preferences) if !preferences.autoplay => return,
            Ok(_) => {}
            Err(error) => {
                send_error(&self.events, error);
                return;
            }
        }
        let additions: Vec<Track> = {
            let known: HashSet<&str> = self
                .queue
                .tracks
                .iter()
                .map(|track| track.source_id.as_str())
                .collect();
            tracks
                .into_iter()
                .filter(|track| !known.contains(track.source_id.as_str()))
                .collect()
        };
        if additions.is_empty() {
            // Radio had nothing new for this seed; autoplay rests here the
            // way Spotify's does when its well runs dry.
            self.autoplay.fruitless_seed = seed_id;
            return;
        }
        let ended = self.queue.ended;
        // Autoplay additions are recommendations, not context: they anchor
        // to their slots at the queue's end and stay put through toggles.
        for track in additions {
            self.queue.tracks.push(track);
            self.queue.shuffle.push_anchor();
        }
        if let Err(error) = self.commit_queue_change(index).await {
            send_error(&self.events, error);
            return;
        }
        if ended {
            // The song outran the fetch: continue into the extension rather
            // than leaving playback parked on the ended state.
            if let Err(error) = self.load_queue_track(index + 1, true).await {
                send_error(&self.events, error);
            }
        }
    }

    /// How many injections Smart Shuffle still owes the upcoming tail to
    /// meet its density.
    fn injection_deficit(&self, index: usize) -> usize {
        let (context, injected) = self.queue.shuffle.upcoming_counts(index);
        injection_target(context).saturating_sub(injected)
    }

    /// Keeps the upcoming tail's injection density topped up: buffered
    /// recommendations are woven in first, and only a still-thin tail with
    /// no fetch in flight starts one, seeded on already-heard music so the
    /// offering keeps rotating. No-op unless Smart is active on a live
    /// queue.
    async fn maybe_prefetch_injections(&mut self) {
        if !self.smart_active() {
            return;
        }
        let Some(index) = self.queue.index else {
            return;
        };
        let deficit = self.injection_deficit(index);
        if deficit == 0 {
            return;
        }

        // The buffer first: those tracks cost no network round-trip.
        if !self.injections.buffer.is_empty() {
            let buffered = self.injections.buffer.drain(..).collect();
            let fresh = self.fresh_recommendations(buffered);
            let inserted = self.weave_injections(&fresh, deficit, index);
            // Whatever did not fit this pass goes back on top of the
            // buffer instead of being refetched later.
            self.buffer_recommendations(fresh.iter().skip(inserted).cloned());
            if inserted > 0 {
                if let Err(error) = self.commit_queue_change(index).await {
                    send_error(&self.events, error);
                    return;
                }
                // Mark as offered only once they actually reached the queue.
                self.remember_injections(&fresh, inserted).await;
            }
        }

        let remaining = self.injection_deficit(index);
        if remaining == 0 || self.injections.task.is_some() {
            return;
        }
        // Seed on a track already heard this session, never the same one
        // as the previous fetch, so consecutive batches come from
        // different stations instead of re-offering the same list.
        let heard_end = index.min(self.queue.tracks.len());
        let Some(seed) = next_injection_seed(
            self.queue.tracks[..heard_end].iter().rev(),
            self.queue.tracks.get(index),
            self.injections.previous_seed.as_deref(),
        )
        .cloned() else {
            return;
        };
        self.injections.seed_id = Some(seed.source_id.clone());
        self.injections.previous_seed = Some(seed.source_id.clone());
        let player = self.connection.player.clone();
        let spotify = self.spotify.clone();
        self.injections.task = Some(tokio::spawn(async move {
            run_with_timeout(60, "Smart Shuffle recommendations", async {
                let player = player.context("Spotify playback is not connected")?;
                let seed_uri = seed
                    .spotify_uri
                    .as_deref()
                    .context("Smart Shuffle seed has no Spotify track URI")?;
                recommendation_tracks(&player, &spotify, seed_uri).await
            })
            .await
        }));
    }

    /// Weaves fresh tracks into the upcoming region at the density-planned
    /// slots via [`ShuffleState::weave_injections`]; returns how many
    /// landed. Callers pre-filter with [`Self::fresh_recommendations`] and
    /// keep whatever this did not take.
    fn weave_injections(&mut self, fresh: &[Track], wanted: usize, playing: usize) -> usize {
        self.queue
            .shuffle
            .weave_injections(&mut self.queue.tracks, fresh, wanted, playing)
    }

    /// The source ids this queue already holds, for filtering incoming
    /// recommendations.
    fn queued_ids(&self) -> HashSet<String> {
        self.queue
            .tracks
            .iter()
            .map(|track| track.source_id.clone())
            .collect()
    }

    /// Filters a recommendation batch down to displayable tracks the queue
    /// does not already hold and Smart Shuffle has never offered before,
    /// deduplicated among themselves.
    fn fresh_recommendations(&self, candidates: Vec<Track>) -> Vec<Track> {
        let known = self.queued_ids();
        let mut seen: HashSet<String> = HashSet::new();
        candidates
            .into_iter()
            .filter(|track| track.is_displayable() && seen.insert(track.source_id.clone()))
            .filter(|track| !known.contains(&track.source_id))
            .filter(|track| !self.smart_shuffle_seen.contains(&track.source_id))
            .collect()
    }

    /// Remembers offered tracks in memory and in the store, so an offered
    /// track is never repeated within the retained history. Called only
    /// after the tracks actually reached the queue.
    async fn remember_injections(&mut self, fresh: &[Track], inserted: usize) {
        let ids: Vec<String> = fresh
            .iter()
            .take(inserted)
            .map(|track| track.source_id.clone())
            .filter(|id| self.smart_shuffle_seen.insert(id.clone()))
            .collect();
        if ids.is_empty() {
            return;
        }
        if let Err(error) = self.store.add_smart_shuffle_seen(ids).await {
            send_error(&self.events, error);
        }
    }

    /// Queues recommendations for later refills, skipping anything already
    /// queued or buffered.
    fn buffer_recommendations(&mut self, candidates: impl IntoIterator<Item = Track>) {
        let known = self.queued_ids();
        let mut buffered: HashSet<String> = self
            .injections
            .buffer
            .iter()
            .map(|track| track.source_id.clone())
            .collect();
        for track in candidates {
            if !track.is_displayable()
                || known.contains(&track.source_id)
                || !buffered.insert(track.source_id.clone())
            {
                continue;
            }
            self.injections.buffer.push_back(track);
        }
    }

    /// Interleaves a finished recommendation batch into the tail and keeps
    /// whatever did not fit buffered for later refills.
    async fn finish_injection_fetch(&mut self, fetched: Finished<Result<Vec<Track>>>) {
        self.injections.task = None;
        let tracks = match fetched {
            Some(Ok(Ok(tracks))) => tracks,
            Some(Ok(Err(error))) => {
                // The rotating seed already points the next attempt
                // somewhere else; just note the failure.
                log::warn!("smart shuffle: recommendation fetch failed: {error:#}");
                return;
            }
            Some(Err(error)) => {
                send_error(&self.events, error);
                return;
            }
            None => return,
        };
        // The listener may have toggled Smart off while this ran.
        if !self.smart_active() {
            return;
        }
        let Some(index) = self.queue.index else {
            return;
        };
        let deficit = self.injection_deficit(index);
        let fresh = self.fresh_recommendations(tracks);
        let inserted = self.weave_injections(&fresh, deficit, index);
        if inserted == 0 {
            // Nothing usable this batch; the next attempt rotates to a
            // different seed on its own.
            return;
        }
        // The surplus stays ready for the next thinning, no refetch.
        self.buffer_recommendations(fresh.iter().skip(inserted).cloned());
        if let Err(error) = self.commit_queue_change(index).await {
            send_error(&self.events, error);
            return;
        }
        // Mark them as offered only once they actually reached the queue:
        // tracks that never made it may fairly be offered again.
        self.remember_injections(&fresh, inserted).await;
    }

    fn connected_player(&self) -> Result<&Playback> {
        self.connection
            .player
            .as_ref()
            .context("Spotify playback is not connected")
    }

    fn configuration_is_from_environment(&self) -> bool {
        self.configuration
            .as_ref()
            .is_some_and(|configuration| configuration.source == ClientIdSource::Environment)
    }

    fn send_configuration_failed(&self, generation: u64, error: impl std::fmt::Display) {
        let _ = self.events.send(BackendEvent::SpotifyConfigurationFailed {
            generation,
            error: error.to_string(),
        });
    }
}

/// The queue as last handed to the player and saved to disk.
#[derive(Clone, Default)]
struct PlayQueue {
    tracks: Vec<Track>,
    /// Shuffle bookkeeping aligned with `tracks`: mode, base order, origins.
    shuffle: ShuffleState,
    /// A track-radio context ignores the toggle entirely.
    radio: bool,
    /// Where this context was started from, which gates Smart Shuffle.
    kind: ContextKind,
    index: Option<usize>,
    position_ms: u32,
    /// What the queue was started from, for the state Cadence reports to
    /// Spotify. A queue built track by track was started from nothing.
    context_uri: Option<String>,
    /// Names this run of the current track in what is reported, so several
    /// reports about one track read as one play. New on every load.
    playback_id: String,
    /// The last track finished with nothing after it. librespot sits in
    /// EndOfTrack, where play() and seek() are no-ops; only a fresh load
    /// leaves it, so Resume and Seek take different paths while this is set.
    ended: bool,
    /// Whether the live load was asked to start playing. An unavailable
    /// track auto-skips only then: a paused restore keeps its selection
    /// until the user acts, rather than bursting into the next track.
    play_requested: bool,
    /// Set when the current entry is known unplayable but auto-skip did
    /// not apply — a paused restore. Resume then advances past it instead
    /// of replaying a load that already failed.
    current_unavailable: bool,
}

impl PlayQueue {
    fn from_snapshot(snapshot: Option<PlaybackSnapshot>) -> Self {
        match snapshot {
            Some(snapshot) => Self {
                tracks: snapshot.tracks,
                shuffle: snapshot.shuffle,
                radio: snapshot.radio,
                kind: snapshot.context_kind,
                index: Some(snapshot.index),
                position_ms: snapshot.position_ms,
                context_uri: None,
                playback_id: String::new(),
                ended: false,
                play_requested: false,
                current_unavailable: false,
            },
            None => Self::default(),
        }
    }

    /// The playing track's Spotify URI, if a track is current.
    fn current_uri(&self) -> Option<&str> {
        self.index
            .and_then(|index| self.tracks.get(index))
            .and_then(|track| track.spotify_uri.as_deref())
    }

    /// What Spotify should be told this queue is doing, if anything. A
    /// track Spotify cannot name — a local file — is nothing to report.
    fn report(&self) -> Option<connect::Report> {
        let track = self.index.and_then(|index| self.tracks.get(index))?;
        Some(connect::Report {
            track_uri: track.spotify_uri.clone()?,
            context_uri: self.context_uri.clone(),
            playback_id: self.playback_id.clone(),
            position_ms: self.position_ms,
            duration_ms: track.duration_ms,
            playing: self.play_requested && !self.ended,
        })
    }

    /// Whether an unavailable-track report should advance the queue: only a
    /// still-current entry whose load was asked to start playing qualifies.
    /// Stale reports (the queue already moved on), preload failures for a
    /// not-yet-playing track, and paused restores all stay put.
    fn should_auto_skip_unavailable(&self, spotify_uri: &str) -> bool {
        self.play_requested && !self.ended && self.current_uri() == Some(spotify_uri)
    }

    /// Records that the current entry cannot be played when auto-skip does
    /// not apply — a paused restore — so Resume can advance instead of
    /// replaying a load that already failed.
    fn note_unavailable_while_paused(&mut self, spotify_uri: &str) {
        if !self.ended && !self.play_requested && self.current_uri() == Some(spotify_uri) {
            self.current_unavailable = true;
        }
    }
}

/// The sign-in and sign-out flows, at most one of each in flight.
#[derive(Default)]
struct SessionTasks {
    authorization: Option<tokio::task::JoinHandle<(u64, Result<AuthorizationSuccess>)>>,
    logout: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl SessionTasks {
    fn begin_authorization(
        &mut self,
        generation: u64,
        spotify: Spotify,
        needs_playback: bool,
        playback_credentials_invalidated: bool,
    ) {
        abort_task(&mut self.authorization);
        self.authorization = Some(tokio::spawn(async move {
            let result =
                authorize_account(spotify, needs_playback, playback_credentials_invalidated).await;
            (generation, result)
        }));
    }

    fn begin_logout(&mut self, store: BlockingStore, spotify: Spotify) {
        abort_task(&mut self.logout);
        self.logout = Some(tokio::spawn(logout_account(store, spotify)));
    }

    async fn finish_pending_logout(&mut self) {
        if let Some(task) = self.logout.take() {
            let _ = task.await;
        }
    }
}

/// The in-flight track-radio request. Starting another cancels the previous.
#[derive(Default)]
struct Radio {
    task: Option<RadioTask>,
    request_id: Option<u64>,
}

/// Prefetches a radio continuation while the queue's last track plays, so
/// autoplay can extend the queue before it runs out.
#[derive(Default)]
struct Autoplay {
    /// Resolves to None when the preference is off, so an opt-out fetch is
    /// never mistaken for a dry radio.
    task: Option<tokio::task::JoinHandle<Result<Option<Vec<Track>>>>>,
    /// The source id the in-flight (or last) fetch was seeded with.
    seed_id: Option<String>,
    /// A seed whose fetch came back dry or failed; skipped until the queue
    /// moves to a different last track, so pause/play cannot spam radio.
    fruitless_seed: Option<String>,
}

/// How much runway the DJ station keeps: the next stretch is fetched once
/// the queue runs to this many tracks or fewer. Tied to the size of a
/// stretch, which the station decides and which runs to about five tracks
/// — a lead shorter than that drains the queue to a handful before it
/// jumps back up, which the station page shows as a list that shrinks and
/// leaps. Retune the two together.
const DJ_TOP_UP_LEAD: usize = 6;

/// One stretch of the DJ station, resolved into what the queue plays and
/// where the stretch after it lives.
struct DjStretch {
    tracks: Vec<Track>,
    next_page_url: Option<String>,
    /// What the DJ prepared to say, by song uri. Most songs carry
    /// nothing: only the first of each stretch does.
    lines: HashMap<String, dj::SessionTrack>,
}

/// The synthesized speech for one upcoming song, ready to be queued the
/// moment it starts. Both variants are prepared because whether the
/// listener skips into the song is not known until they do.
struct ReadyLine {
    uri: String,
    intro: Option<NarrationClip>,
    jump: Option<NarrationClip>,
}

/// The DJ station's state, beside the queue the way radio and autoplay
/// are. The station keeps its own continuation instead of falling back to
/// autoplay: the session decides what comes next, not a recommendation
/// seed.
#[derive(Default)]
struct Dj {
    /// Whether the queue is the station. Cleared as soon as any other
    /// context takes the player.
    playing: bool,
    /// The cursor for the stretch after the one queued, when the session
    /// named one.
    next_page_url: Option<String>,
    /// A stretch being fetched: the opening one while `playing` is false,
    /// a continuation afterwards.
    task: Option<tokio::task::JoinHandle<Result<DjStretch>>>,
    /// What the DJ prepared to say, by song uri.
    lines: HashMap<String, dj::SessionTrack>,
    /// Speech for the upcoming song, synthesized while the current one
    /// plays so the voice is ready before the music needs it.
    ready: Option<ReadyLine>,
    voice: Option<tokio::task::JoinHandle<ReadyLine>>,
    /// Whether the listener skipped into the song about to load, which
    /// decides which of the two prepared lines is spoken.
    skipped: bool,
}

/// How long a session request may take before it is given up on. A
/// stretch resolves its songs' metadata too, so it is not one round trip.
const DJ_FETCH_TIMEOUT_SECONDS: u64 = 60;

/// Spawns the one shape both station fetches share: work out which cursor
/// to follow, then resolve the stretch it leads to.
fn fetch_dj_stretch(
    playback: Option<Playback>,
    cursor: impl Future<Output = Result<Option<String>>> + Send + 'static,
) -> tokio::task::JoinHandle<Result<DjStretch>> {
    tokio::spawn(async move {
        let playback = playback.context("Spotify playback is not connected")?;
        let cursor = cursor.await?;
        run_with_timeout(
            DJ_FETCH_TIMEOUT_SECONDS,
            "Spotify DJ session",
            dj_stretch_from(&playback, cursor),
        )
        .await
    })
}

/// Resolves one stretch, following `cursor` when there is one. A cursor
/// that answers with nothing is a session that has ended, and opening a
/// fresh one is the recovery — nothing else about the station has to
/// notice.
async fn dj_stretch_from(playback: &Playback, cursor: Option<String>) -> Result<DjStretch> {
    if let Some(cursor) = cursor {
        let stretch = dj_stretch(playback, &cursor).await?;
        if !stretch.tracks.is_empty() {
            return Ok(stretch);
        }
    }
    dj_stretch(playback, &dj::session_url()).await
}

/// Resolves one stretch: the session body names songs by uri only, so
/// every title, artist, and duration the queue shows comes from the
/// follow-up metadata lookups.
async fn dj_stretch(playback: &Playback, url: &str) -> Result<DjStretch> {
    let page = playback.dj_page(url).await?;
    let uris: Vec<String> = page.tracks.iter().map(|track| track.uri.clone()).collect();
    Ok(DjStretch {
        tracks: ListedTrack::tracks(&playback.tracks_for_uris(&uris).await),
        next_page_url: page.next_page_url,
        lines: page
            .tracks
            .into_iter()
            .filter(|track| track.intro.is_some() || track.jump.is_some())
            .map(|track| (track.uri.clone(), track))
            .collect(),
    })
}

/// Smart Shuffle's recommendation pipeline: one radio fetch in flight,
/// gated by the Smart toggle itself rather than by any preference, with
/// fetched-but-unwoven tracks buffered so a refill only hits the network
/// when the buffer runs dry too.
#[derive(Default)]
struct Injections {
    task: Option<tokio::task::JoinHandle<Result<Vec<Track>>>>,
    /// The source id the in-flight (or last) fetch was seeded with.
    seed_id: Option<String>,
    /// The seed the previous fetch used; the next fetch picks a different
    /// one so consecutive batches come from different stations. Survives
    /// `reset`: toggling Smart off and on must not re-offer the same
    /// station. A failed or dry batch needs no extra guard — the rotating
    /// seed and the low trigger rate already keep a failing endpoint from
    /// being hammered.
    previous_seed: Option<String>,
    /// Recommendations waiting for a gap in the upcoming tail.
    buffer: VecDeque<Track>,
}

impl Injections {
    fn reset(&mut self) {
        abort_task(&mut self.task);
        self.seed_id = None;
        self.buffer.clear();
    }
}

impl Radio {
    fn start(
        &mut self,
        request_id: u64,
        seed: Track,
        player: Option<Playback>,
        spotify: Spotify,
        events: &UnboundedSender<BackendEvent>,
    ) {
        self.cancel(events);
        self.request_id = Some(request_id);
        self.task = Some(tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                let player = player.context("Spotify playback is not connected")?;
                let seed_uri = seed
                    .spotify_uri
                    .as_deref()
                    .context("radio seed has no Spotify track URI")?;
                let uris = player.radio_track_uris(seed_uri).await?;
                let recommendations = spotify.resolve_track_uris(&uris).await?;
                build_radio_context(seed, recommendations)
            })
            .await
            .unwrap_or_else(|_| Err(anyhow!("Spotify track radio timed out")));
            (request_id, result)
        }));
    }

    fn cancel(&mut self, events: &UnboundedSender<BackendEvent>) {
        abort_task(&mut self.task);
        if let Some(request_id) = self.request_id.take() {
            let _ = events.send(BackendEvent::RadioCancelled { request_id });
        }
    }
}
async fn wait_for_shutdown(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    let _ = shutdown.wait_for(|shutdown| *shutdown).await;
}

/// The in-flight catalog work. Each kind keeps only its newest request:
/// starting another aborts the previous.
#[derive(Default)]
struct CatalogFetches {
    library: Option<tokio::task::JoinHandle<()>>,
    reload: Option<tokio::task::JoinHandle<()>>,
    /// The playlist-order refresh, which runs off the playback session
    /// rather than the Web API.
    order: Option<tokio::task::JoinHandle<()>>,
    /// The pinned set: one task that reads it, then stays on the dealer
    /// subscription for as long as the session lives.
    pins: Option<tokio::task::JoinHandle<()>>,
    /// The connection ids Spotify issues for this session, which every
    /// device state Cadence reports is tagged with.
    connection_id: Option<tokio::task::JoinHandle<()>>,
    search: Option<tokio::task::JoinHandle<()>>,
    playlist: Option<tokio::task::JoinHandle<()>>,
    artist: Option<tokio::task::JoinHandle<()>>,
    album: Option<tokio::task::JoinHandle<()>>,
    /// Seeded from the database at boot and kept current by every load;
    /// lets later reloads answer Unchanged from the head probes alone,
    /// across restarts.
    library_fingerprint: SharedFingerprint,
}

impl CatalogFetches {
    fn load_library(
        &mut self,
        spotify: Spotify,
        store: BlockingStore,
        generation: u64,
        current_generation: Arc<AtomicU64>,
        events: UnboundedSender<BackendEvent>,
    ) {
        abort_task(&mut self.library);
        self.library = Some(spawn_library_load(
            spotify,
            store,
            generation,
            current_generation,
            events,
            self.library_fingerprint.clone(),
        ));
    }

    fn search(&mut self, spotify: Spotify, query: String, respond: Reply<TrackAndPlaylistResults>) {
        Self::start(&mut self.search, respond, "Spotify search", async move {
            tokio::try_join!(
                spotify.search_tracks(&query),
                spotify.search_playlists(&query)
            )
        });
    }

    fn playlist(&mut self, spotify: Spotify, playlist: Playlist, respond: Reply<PlaylistContents>) {
        Self::start(
            &mut self.playlist,
            respond,
            "Spotify playlist request",
            async move {
                spotify
                    .playlist_tracks(&playlist.source_id)
                    .await
                    .map(|tracks| PlaylistContents::Loaded {
                        playlist: None,
                        tracks,
                    })
            },
        );
    }

    /// Loads the stretch the DJ station would start with, for its page.
    /// Only ever asked while the station is not playing — once it is, the
    /// page follows the live queue instead, because the cursor by then
    /// points past everything already queued.
    fn dj_lineup(
        &mut self,
        playback: Option<Playback>,
        store: BlockingStore,
        respond: Reply<PlaylistContents>,
    ) {
        Self::start(
            &mut self.playlist,
            respond,
            "Spotify DJ session request",
            async move {
                let playback = playback.context("Spotify playback is not connected")?;
                let cursor = store.call(|store| store.dj_cursor()).await?;
                let tracks = dj_stretch_from(&playback, cursor).await?.tracks;
                Ok(PlaylistContents::Loaded {
                    playlist: None,
                    tracks: tracks.into_iter().map(ListedTrack::undated).collect(),
                })
            },
        );
    }

    fn artist(&mut self, spotify: Spotify, source_id: String, respond: Reply<ArtistDetails>) {
        Self::start(
            &mut self.artist,
            respond,
            "Spotify artist request",
            async move { spotify.artist(&source_id).await },
        );
    }

    fn album(&mut self, spotify: Spotify, source_id: String, respond: Reply<AlbumDetails>) {
        Self::start(
            &mut self.album,
            respond,
            "Spotify album request",
            async move { spotify.album(&source_id).await },
        );
    }

    fn start<T: Send + 'static>(
        slot: &mut Option<tokio::task::JoinHandle<()>>,
        respond: Reply<T>,
        operation: &'static str,
        request: impl Future<Output = Result<T>> + Send + 'static,
    ) {
        abort_task(slot);
        *slot = Some(tokio::spawn(async move {
            let _ =
                respond.send(run_with_timeout(CATALOG_TIMEOUT_SECONDS, operation, request).await);
        }));
    }

    fn abort_all(&mut self) {
        abort_task(&mut self.library);
        abort_task(&mut self.reload);
        abort_task(&mut self.order);
        abort_task(&mut self.pins);
        abort_task(&mut self.connection_id);
        abort_task(&mut self.search);
        abort_task(&mut self.playlist);
        abort_task(&mut self.artist);
        abort_task(&mut self.album);
        // The next account's library must not compare equal to this one's.
        clear_fingerprint(&self.library_fingerprint);
    }
}

/// The live Spotify playback session and the tasks that keep it alive.
#[derive(Default)]
struct PlaybackConnection {
    player: Option<Playback>,
    observer: Option<tokio::task::JoinHandle<()>>,
    reconnect: Option<tokio::task::JoinHandle<Result<Playback>>>,
    connect: Option<tokio::task::JoinHandle<Result<Playback>>>,
    reconnect_pending: bool,
    /// The connect in flight is replacing a dropped session rather than starting
    /// a fresh one, so playback must not be restored on top of it.
    connect_restoring: bool,
}

impl PlaybackConnection {
    fn disconnect(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        abort_task(&mut self.observer);
    }

    fn adopt(
        &mut self,
        player: Playback,
        events: &UnboundedSender<BackendEvent>,
        unavailable: &UnboundedSender<String>,
    ) {
        self.observer = Some(observe_playback(&player, events, unavailable.clone()));
        self.player = Some(player);
    }

    fn abort_attempts(&mut self) {
        abort_task(&mut self.reconnect);
        abort_task(&mut self.connect);
    }

    /// Replaces the current session with a fresh connection attempt.
    fn begin_connect(&mut self, request: PlaybackConnectionRequest) {
        self.connect_restoring = self.reconnect_pending;
        self.abort_attempts();
        self.reconnect_pending = false;
        self.disconnect();
        self.connect = Some(tokio::spawn(Playback::connect(
            request.load_saved_token,
            request.authorization,
        )));
    }

    /// Starts a reconnect attempt when the session dropped, unless one is
    /// already in flight.
    fn reconnect_if_dead(&mut self, events: &UnboundedSender<BackendEvent>) {
        let session_dropped = self.reconnect_pending
            || self
                .player
                .as_ref()
                .is_some_and(|player| !player.is_connected());
        if !session_dropped {
            return;
        }
        self.reconnect_pending = true;
        if self.reconnect.is_none() {
            log::info!("playback: connection lost, reconnecting");
            self.disconnect();
            let _ = events.send(BackendEvent::PlaybackReconnecting);
            self.reconnect = Some(tokio::spawn(async {
                tokio::time::timeout(Duration::from_secs(15), Playback::reconnect())
                    .await
                    .context("Spotify playback reconnection timed out")?
            }));
        }
    }

    fn finish_reconnect(
        &mut self,
        reconnected: Finished<Result<Playback>>,
        events: &UnboundedSender<BackendEvent>,
        unavailable: &UnboundedSender<String>,
    ) {
        self.reconnect = None;
        match reconnected {
            Some(Ok(Ok(player))) => {
                log::info!("playback: reconnected");
                self.adopt(player, events, unavailable);
                self.reconnect_pending = false;
                let _ = events.send(BackendEvent::PlaybackReconnected);
            }
            Some(Ok(Err(error))) => {
                log::warn!("playback: reconnect attempt failed: {error}");
                let _ = events.send(BackendEvent::PlaybackFailed(format!(
                    "Spotify playback disconnected; reconnecting: {error}"
                )));
            }
            Some(Err(error)) => send_error(events, error),
            None => {}
        }
    }
}

/// Never resolves when the slot is empty, so a `select!` arm can wait on a
/// task that may not exist.
async fn finished<T>(
    task: &mut Option<tokio::task::JoinHandle<T>>,
) -> Option<Result<T, tokio::task::JoinError>> {
    match task.as_mut() {
        Some(task) => Some(task.await),
        None => std::future::pending().await,
    }
}

/// Runs `request`, turning a timeout into an error the caller can show.
async fn run_with_timeout<T>(
    seconds: u64,
    operation: &str,
    request: impl Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(seconds), request).await {
        Ok(result) => result,
        // Keep `Elapsed` in the chain so the error classifies as transient.
        Err(elapsed) => Err(anyhow::Error::new(elapsed).context(format!("{operation} timed out"))),
    }
}

fn abort_task<T>(task: &mut Option<tokio::task::JoinHandle<T>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

async fn receive_shutdown_acknowledgment(
    commands: &mut Receiver<BackendCommand>,
) -> Option<StdSender<()>> {
    while let Some(command) = commands.recv().await {
        if let BackendCommand::Shutdown { acknowledged } = command {
            return Some(acknowledged);
        }
    }
    None
}

/// How long a fetched order counts as fresh. The library behind it is
/// revalidated as often as every half minute when windows are switched.
/// Refetching the order that often would ask a lot of two endpoints Spotify
/// makes no promises about, for an order that rarely moves in half a minute.
const ORDER_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How many rootlist items one page asks for: the page size the official
/// desktop client uses.
const ROOTLIST_PAGE: usize = 120;

/// How many plays the recently-played endpoint is asked for at once. Above
/// the official client's default of 50, so one request normally covers a
/// whole history.
const RECENTLY_PLAYED_LIMIT: usize = 1000;

/// A ceiling on how many pages one refresh follows. Both endpoints report
/// their own totals, so this only ever stops a server that keeps saying
/// there is more without sending any.
const MAX_ORDER_PAGES: usize = 64;

/// Refetches the playlist order and hands back the index as stored.
///
/// The two sources apply independently: a rootlist that failed half way
/// leaves what did arrive, and the play times are fetched either way. Both
/// are reverse-engineered endpoints, so neither failing is allowed to stop
/// the other or to empty what is already stored.
async fn refresh_library_order(playback: &Playback, store: &BlockingStore) -> Result<LibraryIndex> {
    if let Err(error) = refresh_rootlist(playback, store).await {
        log::warn!("library order: rootlist refresh failed: {error:#}");
    }
    if let Err(error) = refresh_recently_played(playback, store).await {
        log::warn!("library order: recently-played refresh failed: {error:#}");
    }
    store.library_index().await
}

/// How many pins one page asks for. Accounts hold a couple of dozen at
/// most, so one page normally covers the whole set.
const PIN_PAGE: usize = 100;

/// A ceiling on how many pin pages one full read follows, matching the one
/// the order walk uses: it only ever stops a server that keeps handing back
/// a next-page token.
const MAX_PIN_PAGES: usize = 8;

/// Asks for the changes since the stored sync token, and hands back the
/// pinned set as it now stands.
///
/// Without a token there is no set to compare against, and a server may
/// also answer that it can no longer describe the difference. Either way
/// the set is read in full instead.
async fn refresh_pins(playback: &Playback, store: &BlockingStore) -> Result<Pins> {
    let Some(token) = store.pin_sync_token().await? else {
        return Ok(read_pins(playback, store).await?.0);
    };
    let delta = playback.pin_delta(&token).await?;
    if !delta.delta_update_possible {
        return Ok(read_pins(playback, store).await?.0);
    }
    let mut pins = store.pins().await?;
    pins.apply_delta(&delta);
    store
        .set_pins(pins.clone(), pins::token(&delta.sync_token))
        .await?;
    Ok(pins)
}

/// Reads the whole pinned set, in the order Spotify holds it, and says
/// whether the whole of it arrived. Only a complete read may be written
/// back, because a write replaces the set with what it is given.
///
/// A read that runs out of pages stores what arrived but no sync token: an
/// increment against a set with a hole in it would apply to the wrong
/// thing, and would go on doing so. Without a token the next read is
/// another full one.
async fn read_pins(playback: &Playback, store: &BlockingStore) -> Result<(Pins, bool)> {
    let mut pins = Pins::default();
    let mut page_token = None;
    let mut sync_token = None;
    let mut complete = false;
    for _ in 0..MAX_PIN_PAGES {
        let page = playback.pin_page(PIN_PAGE, page_token.as_deref()).await?;
        pins.read_page(&page);
        sync_token = pins::token(&page.sync_token).or(sync_token);
        page_token = pins::token(&page.next_page_token);
        if page_token.is_none() {
            complete = true;
            break;
        }
    }
    if !complete {
        log::warn!(
            "pins: the set is longer than {MAX_PIN_PAGES} pages; reading it again next time"
        );
        sync_token = None;
    }
    store.set_pins(pins.clone(), sync_token).await?;
    Ok((pins, complete))
}

/// Walks the rootlist into the index: the playlist set, the folder tree and
/// every Date Added.
///
/// The first page carries the list's revision. When it matches the one the
/// stored index was built from, the list has not changed and the remaining
/// pages are never asked for. A walk that stops early stores what it read
/// but no revision, so the next refresh does not skip the pages it missed.
async fn refresh_rootlist(playback: &Playback, store: &BlockingStore) -> Result<()> {
    let stored = store.rootlist_revision().await?;
    let mut scan = RootlistScan::default();
    scan.read(&playback.rootlist_page(0, ROOTLIST_PAGE).await?);
    if stored.is_some() && scan.revision() == stored.as_deref() {
        return Ok(());
    }
    let mut failure = None;
    for _ in 1..MAX_ORDER_PAGES {
        if !scan.has_more() {
            break;
        }
        let from = scan.read_so_far();
        match playback.rootlist_page(from as usize, ROOTLIST_PAGE).await {
            Ok(page) => scan.read(&page),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
        // A page that moved nothing forward would loop forever; the server
        // has said all it is going to say.
        if scan.read_so_far() == from {
            break;
        }
    }
    let complete = failure.is_none() && !scan.has_more();
    let revision = complete
        .then(|| scan.revision().map(str::to_owned))
        .flatten();
    store
        .replace_rootlist(scan.into_entries(), revision, complete)
        .await?;
    failure.map_or(Ok(()), Err)
}

/// Reads when each context was last played into the index.
///
/// The endpoint answers newest first, so a first page holding nothing newer
/// than the stored watermark means the rest is what the index already has.
async fn refresh_recently_played(playback: &Playback, store: &BlockingStore) -> Result<()> {
    let watermark = store.recently_played_watermark().await?;
    let mut offset = 0;
    for page in 0..MAX_ORDER_PAGES {
        let message = playback
            .recently_played(RECENTLY_PLAYED_LIMIT, offset)
            .await?;
        let plays = library_index::recently_played(&message);
        let newest = plays.iter().map(|(_, played)| *played).max();
        store.apply_recently_played(plays).await?;
        if page == 0 && watermark.is_some() && newest <= watermark {
            return Ok(());
        }
        // The server reports what it withheld; the limit asked for is only
        // a request.
        match library_index::recently_played_withheld(&message) {
            Some(next) => offset = next as usize,
            None => return Ok(()),
        }
    }
    Ok(())
}

async fn load_library(spotify: &Spotify) -> Result<LibraryContents> {
    tokio::try_join!(spotify.liked_tracks(), spotify.playlists())
}

/// What a probed load found. `Changed` hands the caller the fingerprint to
/// commit, which must only happen once the contents are safely persisted: a
/// committed fingerprint over unpersisted contents would answer Unchanged
/// over stale data forever after.
enum ProbedLibrary {
    Unchanged,
    Changed {
        contents: LibraryContents,
        fingerprint: LibraryFingerprint,
    },
}

/// Probes the first pages first: when they and the totals match the last
/// reload, the answer is two requests instead of a full paginated walk.
async fn probe_and_load_library(
    spotify: &Spotify,
    fingerprint: &SharedFingerprint,
) -> Result<ProbedLibrary> {
    let (liked, playlists) =
        tokio::try_join!(spotify.liked_tracks_head(), spotify.playlists_head())?;
    let current = LibraryFingerprint::new(&liked, &playlists);
    let unchanged = fingerprint
        .lock()
        .expect("library fingerprint lock")
        .as_ref()
        == Some(&current);
    if unchanged {
        return Ok(ProbedLibrary::Unchanged);
    }
    let contents = load_library(spotify).await?;
    Ok(ProbedLibrary::Changed {
        contents,
        fingerprint: current,
    })
}

/// Whether the local caches may stand in for a library whose fingerprint
/// matched Spotify's heads: non-empty, or empty because the account is
/// (both saved totals zero).
fn boot_cache_is_plausible(
    persisted: &Option<LibraryFingerprint>,
    cache: &LibraryContents,
) -> bool {
    let non_empty = !cache.0.is_empty() || !cache.1.is_empty();
    let account_is_empty = matches!(
        persisted,
        Some(saved) if saved.liked_total == 0 && saved.playlist_total == 0
    );
    non_empty || account_is_empty
}

fn spawn_library_load(
    spotify: Spotify,
    store: BlockingStore,
    generation: u64,
    current_generation: Arc<AtomicU64>,
    events: UnboundedSender<BackendEvent>,
    fingerprint: SharedFingerprint,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let profile_load = async {
            let Ok(Ok(profile)) =
                tokio::time::timeout(Duration::from_secs(30), spotify.profile()).await
            else {
                return;
            };
            if current_generation.load(Ordering::Acquire) == generation {
                let _ = events.send(BackendEvent::ProfileLoaded {
                    generation,
                    profile,
                });
            }
        };
        let library_load = async {
            // Seed the probe with the fingerprint the previous session
            // saved: when Spotify still matches it, boot answers from the
            // local cache instead of walking the whole library. A fresh
            // database has no fingerprint, so the probe seeds one and boot
            // fetches as before.
            let persisted = match store.saved_library_fingerprint().await {
                Ok(persisted) => {
                    if let Some(saved) = &persisted {
                        commit_fingerprint(&fingerprint, saved.clone());
                    } else {
                        clear_fingerprint(&fingerprint);
                    }
                    persisted
                }
                Err(error) => {
                    send_error(&events, error);
                    clear_fingerprint(&fingerprint);
                    None
                }
            };
            let library = tokio::time::timeout(
                Duration::from_secs(60),
                probe_and_load_library(&spotify, &fingerprint),
            )
            .await;
            if current_generation.load(Ordering::Acquire) != generation {
                return;
            }
            match library {
                Ok(Ok(ProbedLibrary::Unchanged)) => {
                    // The persisted fingerprint matched Spotify's heads, so
                    // the caches written beside it hold the account's
                    // current contents. An implausible cache cannot happen
                    // through normal operation (fingerprint and contents
                    // share one transaction); fail loudly rather than boot
                    // with a silently empty library.
                    match store.library_cache().await {
                        Ok(cache) if boot_cache_is_plausible(&persisted, &cache) => {
                            let _ = events.send(BackendEvent::LibraryLoaded {
                                generation,
                                liked_tracks: cache.0,
                                playlists: cache.1,
                            });
                            let _ = events.send(BackendEvent::CatalogReady { generation });
                        }
                        Ok(_) => {
                            let _ = events.send(BackendEvent::CatalogFailed {
                                generation,
                                error: "library cache is missing although its fingerprint matched"
                                    .to_owned(),
                            });
                        }
                        Err(error) => {
                            let _ = events.send(BackendEvent::CatalogFailed {
                                generation,
                                error: error.to_string(),
                            });
                        }
                    }
                }
                Ok(Ok(ProbedLibrary::Changed {
                    contents,
                    fingerprint: probed,
                })) => {
                    match persist_library_cache(
                        &store,
                        contents,
                        probed.clone(),
                        current_generation.clone(),
                        generation,
                    )
                    .await
                    {
                        Ok(Some((liked_tracks, playlists))) => {
                            commit_fingerprint(&fingerprint, probed);
                            let _ = events.send(BackendEvent::LibraryLoaded {
                                generation,
                                liked_tracks,
                                playlists,
                            });
                            let _ = events.send(BackendEvent::CatalogReady { generation });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = events.send(BackendEvent::CatalogFailed {
                                generation,
                                error: error.to_string(),
                            });
                        }
                    }
                }
                Ok(Err(error)) => {
                    let _ = events.send(BackendEvent::CatalogFailed {
                        generation,
                        error: error.to_string(),
                    });
                }
                Err(_) => {
                    let _ = events.send(BackendEvent::CatalogFailed {
                        generation,
                        error: "Spotify library request timed out".to_owned(),
                    });
                }
            }
        };
        tokio::join!(profile_load, library_load);
    })
}

async fn persist_library_cache(
    store: &BlockingStore,
    contents: LibraryContents,
    fingerprint: LibraryFingerprint,
    current_generation: Arc<AtomicU64>,
    generation: u64,
) -> Result<Option<LibraryContents>> {
    store
        .replace_library_cache_if_current(
            contents.0,
            contents.1,
            fingerprint,
            current_generation,
            generation,
        )
        .await
}

async fn authorize_account(
    mut spotify: Spotify,
    needs_playback: bool,
    playback_credentials_invalidated: bool,
) -> Result<AuthorizationSuccess> {
    let playback_authorization = if needs_playback && playback_credentials_invalidated {
        Some(Playback::prepare_authorization().await?)
    } else {
        None
    };
    let playback_authorization_url = playback_authorization
        .as_ref()
        .map(|authorization| authorization.url().to_owned());
    spotify
        .authorize(playback_authorization_url.as_deref())
        .await?;
    let playback = if needs_playback {
        Some(PlaybackConnectionRequest {
            load_saved_token: !playback_credentials_invalidated,
            authorization: playback_authorization,
        })
    } else {
        None
    };
    Ok(AuthorizationSuccess { playback })
}

async fn logout_account(store: BlockingStore, spotify: Spotify) -> Result<()> {
    let mut errors = Vec::new();
    for result in [
        store.set_oauth_credentials_invalidated(true).await,
        store.set_playback_credentials_invalidated(true).await,
        spotify.logout().await,
        delete_playback_refresh_token().await,
        store.clear_library_cache().await,
        store.clear_playback_state().await,
        store.clear_smart_shuffle_seen().await,
    ] {
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("logout cleanup failed: {}", errors.join("; ")))
    }
}

fn observe_playback(
    player: &Playback,
    events: &UnboundedSender<BackendEvent>,
    unavailable: UnboundedSender<String>,
) -> tokio::task::JoinHandle<()> {
    let mut player_events = player.events();
    let event_sender = events.clone();
    tokio::spawn(async move {
        while let Some(event) = player_events.recv().await {
            if let Some(event) = map_player_event(event, &event_sender, &unavailable) {
                let _ = event_sender.send(event);
            }
        }
    })
}

/// Maps one raw player event to the backend event that carries the same
/// information, if any.
fn map_player_event(
    event: PlayerEvent,
    event_sender: &UnboundedSender<BackendEvent>,
    unavailable: &UnboundedSender<String>,
) -> Option<BackendEvent> {
    match event {
        PlayerEvent::Loading { track_id, .. } => Some(BackendEvent::Loading {
            spotify_uri: track_id.to_string(),
        }),
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => {
            let _ = event_sender.send(BackendEvent::PositionChanged {
                spotify_uri: track_id.to_string(),
                position_ms,
            });
            Some(BackendEvent::Playing {
                spotify_uri: track_id.to_string(),
            })
        }
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => {
            let _ = event_sender.send(BackendEvent::PositionChanged {
                spotify_uri: track_id.to_string(),
                position_ms,
            });
            Some(BackendEvent::Paused {
                spotify_uri: track_id.to_string(),
            })
        }
        PlayerEvent::EndOfTrack { track_id, .. } => Some(BackendEvent::EndOfTrack {
            spotify_uri: track_id.to_string(),
        }),
        PlayerEvent::Unavailable { track_id, .. } => {
            let spotify_uri = track_id.to_string();
            // The worker listens on its own channel so a dead current track
            // auto-skips; the UI only learns of the failure through
            // `TrackFailed`.
            let _ = unavailable.send(spotify_uri.clone());
            Some(BackendEvent::TrackFailed {
                spotify_uri,
                error: "Spotify cannot play this track".to_owned(),
            })
        }
        PlayerEvent::PositionChanged {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::PositionCorrection {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::Seeked {
            track_id,
            position_ms,
            ..
        } => Some(BackendEvent::PositionChanged {
            spotify_uri: track_id.to_string(),
            position_ms,
        }),
        _ => None,
    }
}

/// Loads the queue entry at `index` and starts it playing. With
/// `record_history`, the entry is logged as heard immediately — right for
/// user-driven loads; auto-advance past unplayable entries passes `false`.
async fn load_context_track(
    playback: &Option<Playback>,
    shuffle: &ShuffleState,
    tracks: &[Track],
    index: usize,
    record_history: bool,
    store: &BlockingStore,
    events: &UnboundedSender<BackendEvent>,
) -> Result<()> {
    let track = tracks
        .get(index)
        .context("playback track index is out of bounds")?;
    let spotify_uri = track
        .spotify_uri
        .as_deref()
        .context("track has no Spotify playback URI")?;
    let spotify_uri = SpotifyUri::from_uri(spotify_uri).context("invalid Spotify track URI")?;
    let player = playback
        .as_ref()
        .context("Spotify playback is not connected")?;
    send_playback_context(shuffle, tracks, index, events);
    player.load(spotify_uri, true, 0);
    if record_history && let Err(error) = store.add_history(track.clone()).await {
        send_error(events, error);
    } else if record_history {
        send_local_state(store, events).await;
    }
    Ok(())
}

fn restore_context_track(
    playback: &Option<Playback>,
    shuffle: &ShuffleState,
    tracks: &[Track],
    index: Option<usize>,
    position_ms: u32,
    playing: bool,
    events: &UnboundedSender<BackendEvent>,
) -> Result<()> {
    let index = index.context("playback context is not available")?;
    let track = tracks
        .get(index)
        .context("playback track index is out of bounds")?;
    let spotify_uri = track
        .spotify_uri
        .as_deref()
        .context("track has no Spotify playback URI")?;
    let spotify_uri = SpotifyUri::from_uri(spotify_uri).context("invalid Spotify track URI")?;
    let player = playback
        .as_ref()
        .context("Spotify playback is not connected")?;
    send_playback_context(shuffle, tracks, index, events);
    player.load(spotify_uri, playing, position_ms);
    Ok(())
}

fn restore_saved_playback(
    playback: &Option<Playback>,
    shuffle: &ShuffleState,
    tracks: &[Track],
    index: Option<usize>,
    position_ms: u32,
    events: &UnboundedSender<BackendEvent>,
) -> Result<()> {
    if index.is_none() {
        return Ok(());
    }
    restore_context_track(playback, shuffle, tracks, index, position_ms, false, events)
}

/// Per-track injected flags from the playing track onward — entry 0 is the
/// current track, the rest align with the `next` half of a playback-context
/// event.
fn injected_flags(shuffle: &ShuffleState, index: usize, tracks_len: usize) -> Vec<bool> {
    (index..tracks_len)
        .map(|slot| shuffle.origins.get(slot) == Some(&Origin::Injected))
        .collect()
}

fn send_playback_context(
    shuffle: &ShuffleState,
    tracks: &[Track],
    index: usize,
    events: &UnboundedSender<BackendEvent>,
) {
    if let Some(current) = tracks.get(index) {
        let _ = events.send(BackendEvent::PlaybackContext {
            current: current.clone(),
            next: tracks.get(index + 1..).unwrap_or_default().to_vec(),
            injected: injected_flags(shuffle, index, tracks.len()),
        });
    }
}

/// Tells the UI what the shuffle toggle should show for this queue.
fn send_shuffle_changed(queue: &PlayQueue, events: &UnboundedSender<BackendEvent>) {
    let supported = !queue.radio && queue.index.is_some();
    let _ = events.send(BackendEvent::ShuffleChanged {
        mode: queue.shuffle.mode,
        supported,
        smart_supported: smart_admissible(queue.kind, queue.radio, queue.shuffle.context.len()),
    });
}

/// The current second, which is what a pin write numbers down from.
fn now_seconds() -> i32 {
    chrono::Utc::now()
        .timestamp()
        .try_into()
        .unwrap_or(i32::MAX)
}

fn send_error(events: &UnboundedSender<BackendEvent>, error: impl std::fmt::Display) {
    let _ = events.send(BackendEvent::Error(error.to_string()));
}

fn send_fatal_error(events: &UnboundedSender<BackendEvent>, error: impl std::fmt::Display) {
    let _ = events.send(BackendEvent::FatalError(error.to_string()));
}

/// Picks which track seeds the next Smart Shuffle fetch: walk back through
/// the tracks already heard this session, newest first, skipping the seed
/// the previous fetch used, so consecutive batches come from different
/// stations. Falls back to the playing track when the session has no other
/// candidate — including when every heard track is the avoided one.
fn next_injection_seed<'a>(
    heard_newest_first: impl IntoIterator<Item = &'a Track>,
    fallback: Option<&'a Track>,
    avoid: Option<&str>,
) -> Option<&'a Track> {
    heard_newest_first
        .into_iter()
        .find(|track| Some(track.source_id.as_str()) != avoid)
        .or(fallback)
}

/// One radio-pipeline round-trip shared by autoplay and Smart Shuffle:
/// apollo station URIs seeded on one track, resolved into full tracks.
async fn recommendation_tracks(
    player: &Playback,
    spotify: &Spotify,
    seed_uri: &str,
) -> Result<Vec<Track>> {
    let uris = player.radio_track_uris(seed_uri).await?;
    spotify.resolve_track_uris(&uris).await
}

fn build_radio_context(seed: Track, recommendations: Vec<Track>) -> Result<Vec<Track>> {
    let mut seen = HashSet::from([seed.source_id.clone()]);
    let recommendations = recommendations
        .into_iter()
        .filter(|track| {
            track.spotify_uri.as_deref() != seed.spotify_uri.as_deref()
                && track.is_displayable()
                && seen.insert(track.source_id.clone())
        })
        .collect::<Vec<_>>();
    if recommendations.is_empty() {
        return Err(anyhow!("Spotify track radio returned no playable tracks"));
    }
    let mut tracks = Vec::with_capacity(recommendations.len() + 1);
    tracks.push(seed);
    tracks.extend(recommendations);
    Ok(tracks)
}

async fn send_local_state(store: &BlockingStore, events: &UnboundedSender<BackendEvent>) {
    match store.local_state().await {
        Ok((pins, recently_played, library_index)) => {
            let _ = events.send(BackendEvent::LocalStateLoaded {
                pins,
                recently_played,
                library_index,
            });
        }
        Err(error) => send_error(events, error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackendCommand, BlockingStore, PlayQueue, ShuffleState, build_radio_context,
        injected_flags, next_injection_seed, send_command,
    };
    use crate::model::{Provider, Track};
    use crate::shuffle::Origin;
    use crate::storage::Store;

    fn track() -> Track {
        Track {
            provider: Provider::Spotify,
            source_id: "track-id".to_owned(),
            spotify_uri: Some("spotify:track:track-id".to_owned()),
            isrc: None,
            title: "Track".to_owned(),
            artist: "Artist".to_owned(),
            artists: Vec::new(),
            album: "Album".to_owned(),
            album_ref: None,
            duration_ms: 180_000,
            artwork_url: None,
        }
    }

    #[test]
    fn command_ingress_is_bounded_and_volume_is_coalesced() {
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let (controls, mut control_receiver) = tokio::sync::mpsc::channel(1);
        let (volume, mut volume_receiver) = tokio::sync::watch::channel(0.5);

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::SetVolume(0.2)
        ));
        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::SetVolume(0.8)
        ));
        assert_eq!(*volume_receiver.borrow_and_update(), 0.8);
        assert!(command_receiver.try_recv().is_err());

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Pause
        ));
        assert!(!send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Resume
        ));
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(BackendCommand::Pause)
        ));

        assert!(send_command(
            &commands,
            &controls,
            &volume,
            BackendCommand::Logout { generation: 1 }
        ));
        assert!(matches!(
            control_receiver.try_recv(),
            Ok(BackendCommand::Logout { generation: 1 })
        ));
    }

    #[tokio::test]
    async fn blocking_store_runs_operations_off_the_async_thread() {
        let caller = std::thread::current().id();
        let store = BlockingStore::from_store(Store::in_memory().unwrap());

        let worker = store
            .call(|_| Ok(std::thread::current().id()))
            .await
            .unwrap();

        assert_ne!(worker, caller);
    }

    #[tokio::test]
    async fn cancelling_a_store_call_does_not_lose_or_reorder_the_store() {
        let store = BlockingStore::from_store(Store::in_memory().unwrap());
        let first_store = store.clone();
        let first = tokio::spawn(async move {
            first_store
                .call(|store| {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    store.set_spotify_client_id("first")
                })
                .await
        });
        tokio::task::yield_now().await;
        first.abort();

        store
            .call(|store| store.set_spotify_client_id("second"))
            .await
            .unwrap();
        let client_id = store.call(|store| store.spotify_client_id()).await.unwrap();

        assert_eq!(client_id.as_deref(), Some("second"));
    }

    #[test]
    fn radio_context_starts_with_seed_and_deduplicates_recommendations() {
        let seed = track();
        let mut recommendation = track();
        recommendation.source_id = "recommendation".to_owned();
        recommendation.spotify_uri = Some("spotify:track:recommendation".to_owned());
        recommendation.title = "Recommendation".to_owned();

        let tracks = build_radio_context(
            seed.clone(),
            vec![seed.clone(), recommendation.clone(), recommendation.clone()],
        )
        .unwrap();

        assert_eq!(tracks, vec![seed, recommendation]);
    }

    #[test]
    fn radio_context_rejects_an_empty_recommendation_set() {
        let seed = track();

        assert!(build_radio_context(seed.clone(), vec![seed]).is_err());
    }

    #[test]
    fn unavailable_track_autoskips_only_a_playing_load_of_the_current_entry() {
        fn queue_on_first_track() -> PlayQueue {
            let mut next = track();
            next.source_id = "next".to_owned();
            next.spotify_uri = Some("spotify:track:next".to_owned());
            PlayQueue {
                tracks: vec![track(), next],
                index: Some(0),
                ..PlayQueue::default()
            }
        }

        assert_eq!(
            queue_on_first_track().current_uri(),
            Some("spotify:track:track-id")
        );

        let mut queue = queue_on_first_track();
        queue.play_requested = true;
        assert!(queue.should_auto_skip_unavailable("spotify:track:track-id"));

        // A paused restore keeps its selection instead of bursting into
        // the next track on launch or reconnect.
        queue.play_requested = false;
        assert!(!queue.should_auto_skip_unavailable("spotify:track:track-id"));
        queue.play_requested = true;

        // Stale report: the queue already moved past the failed track.
        assert!(!queue.should_auto_skip_unavailable("spotify:track:next"));

        // Nothing may advance once the queue has ended.
        queue.ended = true;
        assert!(!queue.should_auto_skip_unavailable("spotify:track:track-id"));
    }

    #[test]
    fn paused_restore_notes_a_dead_current_track_for_resume() {
        let mut queue = {
            let mut next = track();
            next.source_id = "next".to_owned();
            next.spotify_uri = Some("spotify:track:next".to_owned());
            PlayQueue {
                tracks: vec![track(), next],
                index: Some(0),
                ..PlayQueue::default()
            }
        };

        // A paused restore onto a dead track is remembered so Resume can
        // advance instead of replaying the failed load.
        queue.note_unavailable_while_paused("spotify:track:track-id");
        assert!(queue.current_unavailable);

        // A report for anything but the current entry is stale and ignored.
        let mut stale = queue.clone();
        stale.current_unavailable = false;
        stale.note_unavailable_while_paused("spotify:track:next");
        assert!(!stale.current_unavailable);

        // A playing load auto-skips instead, so nothing is noted.
        let mut playing = queue.clone();
        playing.current_unavailable = false;
        playing.play_requested = true;
        playing.note_unavailable_while_paused("spotify:track:track-id");
        assert!(!playing.current_unavailable);

        // An ended queue never takes notes.
        let mut ended = queue;
        ended.current_unavailable = false;
        ended.ended = true;
        ended.note_unavailable_while_paused("spotify:track:track-id");
        assert!(!ended.current_unavailable);
    }

    #[test]
    fn injected_flags_mark_only_smart_shuffle_entries_from_the_playing_track_on() {
        let shuffle = ShuffleState {
            origins: vec![
                Origin::Context { ordinal: 0 },
                Origin::Injected,
                Origin::Context { ordinal: 1 },
                Origin::Anchor,
            ],
            ..ShuffleState::default()
        };

        // Entry 0 is the playing track; the rest align with `next`.
        assert_eq!(
            injected_flags(&shuffle, 0, 4),
            vec![false, true, false, false]
        );
        assert_eq!(injected_flags(&shuffle, 1, 4), vec![true, false, false]);
    }

    #[test]
    fn injection_seed_rotates_through_heard_tracks_and_skips_the_previous_one() {
        let heard = [
            track(),
            {
                let mut previous = track();
                previous.source_id = "previous".to_owned();
                previous
            },
            {
                let mut current = track();
                current.source_id = "current".to_owned();
                current
            },
        ];
        // Newest-first walk skips the seed the last fetch used and lands
        // on the next-newest heard track.
        assert_eq!(
            next_injection_seed(heard.iter().rev(), None, Some("current"))
                .unwrap()
                .source_id,
            "previous"
        );
        // Nothing heard yet falls back to the playing track.
        assert_eq!(
            next_injection_seed([], Some(&heard[2]), None)
                .unwrap()
                .source_id,
            "current"
        );
        // Every heard track is the avoided one: the fallback still fires
        // rather than skipping the fetch entirely.
        assert_eq!(
            next_injection_seed([&heard[2]].into_iter(), Some(&heard[2]), Some("current"))
                .unwrap()
                .source_id,
            "current"
        );
        assert!(next_injection_seed([], None, None).is_none());
    }
}
