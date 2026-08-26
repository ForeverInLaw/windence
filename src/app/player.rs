use super::*;

/// How far playback may drift from the saved position before it is written back.
const POSITION_SAVE_INTERVAL_MS: u32 = 5_000;

/// Reported when a playback command could not be delivered to the backend.
pub(super) struct PlaybackUnavailable;

impl EventEmitter<PlaybackUnavailable> for Player {}

/// Playback state that belongs to the process rather than to a window.
pub(super) struct Player {
    backend: BackendHandle,
    now_playing: Option<model::Track>,
    /// Whether the track playing now was injected by Smart Shuffle; it
    /// keeps its mark in the queue panel until it finishes.
    now_playing_injected: bool,
    context: Arc<[model::Track]>,
    queue: Arc<[model::Track]>,
    /// Which upcoming tracks are Smart Shuffle injections, aligned with
    /// `queue`.
    queue_injected: Arc<[bool]>,
    /// The kind of context playback started from, kept so re-playing a
    /// queued track hands the backend the same kind again.
    context_kind: ContextKind,
    /// Whether the DJ station is what is playing. The station page shows
    /// the live queue while it is.
    station: bool,
    playing: bool,
    loading: bool,
    /// The shuffle toggle's value and whether it can act: a live,
    /// non-radio context. A radio is already a recommendation stream.
    shuffle_mode: ShuffleMode,
    shuffle_supported: bool,
    /// Whether the toggle's third state (Smart Shuffle) may act on this
    /// context: playlist-like and long enough.
    shuffle_smart_supported: bool,
    /// Position and play state to reapply once a reconnected player is ready.
    restore: Option<(u32, bool)>,
    position_ms: u32,
    saved_position_ms: u32,
    volume: f32,
    volume_before_mute: f32,
    volume_dragging: bool,
    error: Option<String>,
}

impl Player {
    /// `volume` is what the listener last chose, so playback starts where
    /// they left it rather than at the default.
    pub(super) fn new(backend: BackendHandle, volume: f32) -> Self {
        Self {
            backend,
            now_playing: None,
            now_playing_injected: false,
            context: Arc::default(),
            queue: Arc::default(),
            queue_injected: Arc::default(),
            context_kind: ContextKind::default(),
            station: false,
            playing: false,
            loading: false,
            shuffle_mode: ShuffleMode::Off,
            shuffle_supported: false,
            shuffle_smart_supported: false,
            restore: None,
            position_ms: 0,
            saved_position_ms: 0,
            volume,
            volume_before_mute: volume,
            volume_dragging: false,
            error: None,
        }
    }

    pub(super) fn now_playing(&self) -> Option<&model::Track> {
        self.now_playing.as_ref()
    }

    /// Whether the track playing now was injected by Smart Shuffle.
    pub(super) fn now_playing_injected(&self) -> bool {
        self.now_playing_injected
    }

    pub(super) fn context(&self) -> &Arc<[model::Track]> {
        &self.context
    }

    /// The kind of the context playback started from.
    pub(super) fn context_kind(&self) -> ContextKind {
        self.context_kind
    }

    pub(super) fn queue(&self) -> &Arc<[model::Track]> {
        &self.queue
    }

    /// Whether the DJ station is what is playing.
    pub(super) fn station(&self) -> bool {
        self.station
    }

    /// Whether the upcoming track at `index` is a Smart Shuffle injection.
    pub(super) fn queue_track_injected(&self, index: usize) -> bool {
        self.queue_injected.get(index).copied().unwrap_or(false)
    }

    pub(super) fn playing(&self) -> bool {
        self.playing
    }

    pub(super) fn loading(&self) -> bool {
        self.loading
    }

    pub(super) fn shuffle_mode(&self) -> ShuffleMode {
        self.shuffle_mode
    }

    /// False until a live queue is known and always false for radio
    /// contexts, whose toggle would be a no-op.
    pub(super) fn shuffle_supported(&self) -> bool {
        self.shuffle_supported
    }

    /// Whether the toggle's Smart Shuffle state may act on this context.
    pub(super) fn shuffle_smart_supported(&self) -> bool {
        self.shuffle_smart_supported
    }

    pub(super) fn position_ms(&self) -> u32 {
        self.position_ms
    }

    pub(super) fn volume(&self) -> f32 {
        self.volume
    }

    pub(super) fn volume_dragging(&self) -> bool {
        self.volume_dragging
    }

    pub(super) fn error(&self) -> Option<&String> {
        self.error.as_ref()
    }

    pub(super) fn is_current_track(&self, track: &model::Track) -> bool {
        self.now_playing.as_ref().is_some_and(|playing| {
            playing.provider == track.provider && playing.source_id == track.source_id
        })
    }

    fn live_track_matches(&self, spotify_uri: &str) -> bool {
        self.now_playing
            .as_ref()
            .and_then(|track| track.spotify_uri.as_deref())
            == Some(spotify_uri)
    }

    /// Playback commands are dropped while a restore is in flight so they cannot
    /// race the position the backend is about to reapply.
    fn send(&self, command: BackendCommand, cx: &mut Context<Self>) -> bool {
        if self.restore.is_some() {
            return false;
        }
        self.deliver(command, cx)
    }

    /// Sends `command` and reports the failure when the backend cannot take it,
    /// so a dead or saturated worker does not leave controls silently inert.
    fn deliver(&self, command: BackendCommand, cx: &mut Context<Self>) -> bool {
        if self.backend.send(command) {
            return true;
        }
        cx.emit(PlaybackUnavailable);
        false
    }

    pub(super) fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.now_playing.is_none() {
            return;
        }
        let playing = !self.playing;
        if self.send(
            if self.playing {
                BackendCommand::Pause
            } else {
                BackendCommand::Resume
            },
            cx,
        ) {
            self.playing = playing;
            self.loading = playing;
        }
        cx.notify();
    }

    /// Moves to `playing`, doing nothing if playback is already in that state.
    // Driven by the system media controls, which attach on macOS only until
    // the Windows SMTC window-handle wiring lands.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(super) fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playing != playing {
            self.toggle(cx);
        }
    }

    pub(super) fn next(&mut self, cx: &mut Context<Self>) {
        if self.now_playing.is_some() && self.send(BackendCommand::Next, cx) {
            self.loading = true;
        }
        cx.notify();
    }

    pub(super) fn previous(&mut self, cx: &mut Context<Self>) {
        if self.now_playing.is_some() && self.send(BackendCommand::Previous, cx) {
            self.loading = true;
        }
        cx.notify();
    }

    pub(super) fn seek(&mut self, position_ms: u32, cx: &mut Context<Self>) {
        if self.send(BackendCommand::Seek(position_ms), cx) {
            self.position_ms = position_ms;
        }
        cx.notify();
    }

    /// Starts a context at `index`, inheriting the global shuffle toggle.
    /// The kind gates Smart Shuffle: albums and short lists get plain
    /// shuffle even where the global toggle says Smart (the backend clamps
    /// too; this keeps the optimistic path honest). `context_uri` names what
    /// was started, so a playlist rises to the top of the library list;
    /// contexts with no uri of their own — a search result, the queue —
    /// pass `None`.
    pub(super) fn play_context(
        &mut self,
        tracks: Vec<model::Track>,
        index: usize,
        kind: ContextKind,
        context_uri: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.send_play_context(tracks, index, false, kind, context_uri, cx)
    }

    /// Starts a context shuffled and moves the global toggle to Shuffle,
    /// as the playlist and album shuffle-play controls do.
    pub(super) fn play_context_shuffled(
        &mut self,
        tracks: Vec<model::Track>,
        index: usize,
        kind: ContextKind,
        context_uri: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.send_play_context(tracks, index, true, kind, context_uri, cx)
    }

    fn send_play_context(
        &mut self,
        tracks: Vec<model::Track>,
        index: usize,
        shuffled: bool,
        kind: ContextKind,
        context_uri: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.context_kind = kind;
        let started = self.send(
            BackendCommand::PlayContext {
                tracks,
                index,
                shuffled,
                kind,
                context_uri,
            },
            cx,
        );
        if started {
            self.position_ms = 0;
            self.playing = false;
            self.loading = true;
        }
        cx.notify();
        started
    }

    /// Starts the DJ station. Cadence resolves the session itself, so no
    /// tracks travel with the request — the backend decides what plays.
    pub(super) fn play_dj(&mut self, cx: &mut Context<Self>) -> bool {
        self.context_kind = ContextKind::Collection;
        let started = self.send(BackendCommand::PlayDj, cx);
        if started {
            self.position_ms = 0;
            self.playing = false;
            self.loading = true;
        }
        cx.notify();
        started
    }

    /// Moves the toggle to its next value: Off → Shuffle → Smart → Off,
    /// with Smart skipped where it cannot act. The backend confirms with a
    /// `ShuffleChanged` event; until one arrives the optimistic update
    /// keeps the button responsive.
    pub(super) fn cycle_shuffle(&mut self, cx: &mut Context<Self>) {
        if !self.shuffle_supported {
            return;
        }
        let next = self.shuffle_mode.toggled(self.shuffle_smart_supported);
        if self.deliver(BackendCommand::SetShuffleMode(next), cx) {
            self.shuffle_mode = next;
        }
        cx.notify();
    }

    pub(super) fn play_next(&mut self, track: model::Track, cx: &mut Context<Self>) -> bool {
        self.deliver(BackendCommand::PlayNext(track), cx)
    }

    pub(super) fn append_to_queue(&mut self, track: model::Track, cx: &mut Context<Self>) -> bool {
        self.deliver(BackendCommand::AppendToQueue(track), cx)
    }

    pub(super) fn start_radio(
        &mut self,
        request_id: u64,
        seed: model::Track,
        cx: &mut Context<Self>,
    ) -> bool {
        self.deliver(BackendCommand::StartRadio { request_id, seed }, cx)
    }

    pub(super) fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        self.loading = loading;
        cx.notify();
    }

    pub(super) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if self.volume > 0. {
            self.volume_before_mute = self.volume;
            self.volume = 0.;
        } else {
            self.volume = self.volume_before_mute.max(0.2);
        }
        self.deliver(BackendCommand::SetVolume(self.volume), cx);
        self.save_volume(cx);
        cx.notify();
    }

    pub(super) fn begin_volume_drag(
        &mut self,
        pointer_x: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.volume_dragging = true;
        self.drag_volume(pointer_x, window, cx);
    }

    pub(super) fn drag_volume(
        &mut self,
        pointer_x: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let window_width = f32::from(window.window_bounds().get_bounds().size.width);
        self.volume = volume_for_pointer(f32::from(pointer_x), window_width);
        if self.volume > 0. {
            self.volume_before_mute = self.volume;
        }
        self.deliver(BackendCommand::SetVolume(self.volume), cx);
        cx.notify();
    }

    pub(super) fn end_volume_drag(&mut self, cx: &mut Context<Self>) {
        if self.volume_dragging {
            self.volume_dragging = false;
            // The end of the drag is the choice; the pixels along the way
            // are not worth a disk write each.
            self.save_volume(cx);
            cx.notify();
        }
    }

    /// Remembers the volume for the next launch.
    fn save_volume(&self, cx: &mut Context<Self>) {
        services::AppServices::set_volume(self.volume, cx);
    }

    /// Writes the live position back so a restart resumes where the listener left off.
    /// The position to persist for the current track, if one is playing.
    pub(super) fn position_snapshot(&self) -> Option<u32> {
        self.now_playing
            .as_ref()
            .and_then(|track| track.spotify_uri.as_ref())
            .map(|_| self.position_ms)
    }

    pub(super) fn save_position(&self) {
        if let Some(spotify_uri) = self
            .now_playing
            .as_ref()
            .and_then(|track| track.spotify_uri.clone())
        {
            self.backend.send(BackendCommand::SavePlaybackPosition {
                spotify_uri,
                position_ms: self.position_ms,
            });
        }
    }

    fn save_position_if_moved(&mut self, spotify_uri: String, position_ms: u32) {
        if position_ms.abs_diff(self.saved_position_ms) < POSITION_SAVE_INTERVAL_MS {
            return;
        }
        self.backend.send(BackendCommand::SavePlaybackPosition {
            spotify_uri,
            position_ms,
        });
        self.saved_position_ms = position_ms;
    }

    fn adopt_context(
        &mut self,
        current: model::Track,
        next: Vec<model::Track>,
        injected: Vec<bool>,
    ) {
        self.context = std::iter::once(current.clone())
            .chain(next.iter().cloned())
            .collect::<Vec<_>>()
            .into();
        // Entry 0 is the playing track's own flag; the rest align with the
        // upcoming queue.
        let (current_injected, upcoming) = match injected.split_first() {
            Some((first, rest)) => (*first, rest),
            None => (false, [].as_slice()),
        };
        self.now_playing = Some(current);
        self.now_playing_injected = current_injected;
        self.queue_injected = upcoming.to_vec().into();
        self.queue = next.into();
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.now_playing = None;
        self.now_playing_injected = false;
        self.context = Arc::default();
        self.queue = Arc::default();
        self.queue_injected = Arc::default();
        self.station = false;
        self.playing = false;
        self.loading = false;
        self.shuffle_mode = ShuffleMode::Off;
        self.shuffle_supported = false;
        self.shuffle_smart_supported = false;
        self.restore = None;
        self.position_ms = 0;
        self.saved_position_ms = 0;
        self.error = None;
        cx.notify();
    }

    /// Applies the playback half of a backend event, returning the event when the
    /// surrounding app still has its own work to do for it.
    pub(super) fn handle_backend_event(
        &mut self,
        event: BackendEvent,
        cx: &mut Context<Self>,
    ) -> Option<BackendEvent> {
        match event {
            BackendEvent::PlaybackReady => {
                self.error = None;
                self.backend.send(BackendCommand::SetVolume(self.volume));
            }
            BackendEvent::PlaybackReconnecting => {
                if self.restore.is_none() && self.now_playing.is_some() {
                    self.restore = Some((self.position_ms, self.playing));
                }
                self.loading = true;
            }
            BackendEvent::PlaybackReconnected => {
                self.error = None;
                self.backend.send(BackendCommand::SetVolume(self.volume));
                if let Some((position_ms, playing)) = self.restore {
                    self.backend.send(BackendCommand::RestorePlayback {
                        position_ms,
                        playing,
                    });
                } else {
                    self.loading = false;
                }
            }
            BackendEvent::PlaybackRestored {
                position_ms,
                playing,
            } => {
                self.position_ms = position_ms;
                self.saved_position_ms = position_ms;
                self.playing = playing;
                self.loading = false;
                self.restore = None;
            }
            BackendEvent::PlaybackSettled => {
                self.loading = false;
                self.restore = None;
            }
            BackendEvent::QueueEnded => {
                self.playing = false;
                self.loading = false;
                // A full bar with an armed Play button would lie: Play
                // starts this track over (or from wherever the seeker goes).
                self.position_ms = 0;
                self.saved_position_ms = 0;
            }
            BackendEvent::Playing { spotify_uri } => {
                if self.restore.is_none() && self.live_track_matches(&spotify_uri) {
                    self.playing = true;
                    self.loading = false;
                }
            }
            BackendEvent::Loading { spotify_uri } => {
                if self.restore.is_none() && self.live_track_matches(&spotify_uri) {
                    self.loading = true;
                }
            }
            BackendEvent::Paused { spotify_uri } => {
                if self.restore.is_none() && self.live_track_matches(&spotify_uri) {
                    self.playing = false;
                    self.loading = false;
                    if self.position_ms != self.saved_position_ms {
                        let position_ms = self.position_ms;
                        self.backend.send(BackendCommand::SavePlaybackPosition {
                            spotify_uri,
                            position_ms,
                        });
                        self.saved_position_ms = position_ms;
                    }
                }
            }
            BackendEvent::StationChanged(station) => self.station = station,
            BackendEvent::EndOfTrack { spotify_uri } => {
                if self.restore.is_none() && self.live_track_matches(&spotify_uri) {
                    self.playing = false;
                    self.loading = false;
                    self.backend.send(BackendCommand::Next);
                }
            }
            BackendEvent::PositionChanged {
                spotify_uri,
                position_ms,
            } => {
                if self.restore.is_none() && self.live_track_matches(&spotify_uri) {
                    self.position_ms = position_ms;
                    self.save_position_if_moved(spotify_uri, position_ms);
                }
            }
            BackendEvent::PlaybackSnapshotLoaded {
                current,
                next,
                injected,
                position_ms,
            } => {
                self.adopt_context(current, next, injected);
                self.position_ms = position_ms;
                self.saved_position_ms = position_ms;
                self.playing = false;
                self.loading = false;
            }
            BackendEvent::PlaybackContext {
                current,
                next,
                injected,
            } => {
                let changed = self.now_playing.as_ref().is_none_or(|track| {
                    track.provider != current.provider || track.source_id != current.source_id
                });
                self.adopt_context(current, next, injected);
                if changed {
                    self.loading = true;
                    self.position_ms = 0;
                    self.saved_position_ms = 0;
                    self.restore = None;
                }
            }
            BackendEvent::ShuffleChanged {
                mode,
                supported,
                smart_supported,
            } => {
                self.shuffle_mode = mode;
                self.shuffle_supported = supported;
                self.shuffle_smart_supported = smart_supported;
            }
            BackendEvent::PlaybackFailed(error) => {
                self.error = Some(error);
            }
            BackendEvent::TrackFailed { spotify_uri, error } => {
                if self.live_track_matches(&spotify_uri) {
                    self.now_playing = None;
                    self.now_playing_injected = false;
                    self.context = Arc::default();
                    self.queue = Arc::default();
                    self.queue_injected = Arc::default();
                    self.playing = false;
                    self.loading = false;
                    // No live queue left for the toggle to act on.
                    self.shuffle_mode = ShuffleMode::Off;
                    self.shuffle_supported = false;
                    self.shuffle_smart_supported = false;
                }
                cx.notify();
                return Some(BackendEvent::TrackFailed { spotify_uri, error });
            }
            event => return Some(event),
        }
        cx.notify();
        None
    }
}
