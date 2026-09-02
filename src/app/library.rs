use super::*;

use std::collections::HashMap;

/// The pin count the interface warns at. Spotify reports its real limit
/// only over the Esperanto IPC its own desktop client speaks to itself,
/// never over the network, so this is a warning and never a rule: the write
/// is attempted regardless, and Spotify's answer is what decides.
pub(super) const PIN_WARNING_LIMIT: usize = 20;

/// Debounce between revalidations, so rapid window switches coalesce. Short
/// on purpose: a revalidation is a two-request head probe unless something
/// changed, and Spotify's rate limit is a rolling 30-second window. Wall
/// clock rather than `Instant`, which stops while the machine sleeps.
const REVALIDATION_DEBOUNCE: Duration = Duration::from_secs(30);

/// Everything Cadence knows about the listener's music: what Spotify holds for
/// the account, and what Cadence keeps locally about it.
///
/// Owned by the services global so a window can be rebuilt without refetching.
pub(super) struct Library {
    backend: BackendHandle,
    liked_tracks: Arc<[model::ListedTrack]>,
    /// The source ids in `liked_tracks`, so a row can ask whether its track
    /// is liked without walking the collection.
    liked_keys: HashSet<String>,
    playlists: Arc<[model::Playlist]>,
    loaded: bool,
    /// Where Spotify puts each playlist and folder, and when each was last
    /// played. Stored on disk, so it is there before the network answers.
    index: library_index::LibraryIndex,
    /// The order the listener chose, remembered between launches.
    sort: library_index::PlaylistSort,
    /// The folders they left open, likewise.
    expanded_folders: HashSet<String>,
    /// The folders left open in the pinned section, which opens and closes
    /// on its own: the same folder is drawn in both lists, and opening it
    /// in one is no reason for the other to open. Not remembered between
    /// launches, unlike the list's own.
    expanded_pins: HashSet<String>,
    /// What the account has pinned in Spotify, in Spotify's own order.
    /// Stored on disk, so the section is drawn before the network answers
    /// and still drawn when there is no session to read it with.
    pins: Pins,
    /// Playlists the listener just saved or removed, as the button showed
    /// them on the click, until the library is read back from Spotify.
    pending_saves: HashMap<String, bool>,
    /// The rows the playlist list draws, rebuilt whenever anything above
    /// changes so a render never has to sort.
    rows: Arc<[library_index::LibraryRow]>,
    /// The pinned rows on their own, for the sidebar's own section. They
    /// also lead `rows`, so the two can never disagree.
    pinned_rows: Arc<[library_index::LibraryRow]>,
    recently_played: Arc<[model::ListedTrack]>,
    local_loaded: bool,
    reload: Option<gpui::Task<()>>,
    /// When the contents last arrived, so returning to the window repeatedly
    /// does not refetch the whole library every time.
    refreshed_at: Option<SystemTime>,
}

/// Raised when fresh contents arrived, so stale failures can be cleared.
pub(super) struct LibraryLoaded;

impl EventEmitter<LibraryLoaded> for Library {}

impl Library {
    pub(super) fn new(
        backend: BackendHandle,
        sort: library_index::PlaylistSort,
        expanded_folders: HashSet<String>,
    ) -> Self {
        Self {
            backend,
            liked_tracks: Arc::default(),
            liked_keys: HashSet::new(),
            playlists: Arc::default(),
            loaded: false,
            index: library_index::LibraryIndex::default(),
            sort,
            expanded_folders,
            expanded_pins: HashSet::new(),
            pins: Pins::default(),
            pending_saves: HashMap::new(),
            rows: Arc::default(),
            pinned_rows: Arc::default(),
            recently_played: Arc::default(),
            local_loaded: false,
            reload: None,
            refreshed_at: None,
        }
    }

    pub(super) fn reloading(&self) -> bool {
        self.reload.is_some()
    }

    /// Refetches the library, leaving the current contents visible until the
    /// answer arrives. Does nothing when one is already running; the backend
    /// additionally answers Unchanged while the boot load owns the first
    /// fetch, so this cannot race it into a doubled walk.
    pub(super) fn revalidate(&mut self, cx: &mut Context<Self>) {
        if self.reload.is_some() || is_fresh(self.refreshed_at, REVALIDATION_DEBOUNCE) {
            return;
        }
        let (respond, reply) = tokio::sync::oneshot::channel();
        if !self.backend.send(BackendCommand::ReloadLibrary { respond }) {
            return;
        }
        self.reload = Some(cx.spawn(async move |this, cx| {
            let contents = reply.await;
            let _ = this.update(cx, |library, cx| {
                library.reload = None;
                match contents {
                    Ok(Ok(LibraryReload::Fresh((liked_tracks, playlists)))) => {
                        library.set_liked_tracks(liked_tracks);
                        library.set_playlists(playlists, cx);
                        library.loaded = true;
                        library.refreshed_at = Some(SystemTime::now());
                        cx.emit(LibraryLoaded);
                    }
                    // The head probes matched: the contents on screen are the
                    // contents on Spotify.
                    Ok(Ok(LibraryReload::Unchanged)) => {
                        library.refreshed_at = Some(SystemTime::now());
                    }
                    Ok(Err(error)) => match spotify::classify_error(&error) {
                        // The gate holds automatic refreshes back; the next
                        // timer or activation retries after the cooldown.
                        spotify::ErrorKind::RateLimited { .. } => {
                            log::info!("library: rate limited; refresh deferred")
                        }
                        _ => log::warn!("library: refresh failed: {error:#}"),
                    },
                    Err(_) => log::warn!("library: backend stopped before answering"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn liked_tracks(&self) -> &Arc<[model::ListedTrack]> {
        &self.liked_tracks
    }

    /// The playlist list as it is drawn: playlists and folders, in the order
    /// the chosen mode puts them, with closed folders hiding what they hold.
    pub(super) fn playlist_rows(&self) -> &Arc<[library_index::LibraryRow]> {
        &self.rows
    }

    pub(super) fn playlist_sort(&self) -> library_index::PlaylistSort {
        self.sort
    }

    /// The modes the menu may offer. With no index behind it — no Premium,
    /// offline, connection not up, or the first load still running — only
    /// the two that read the Web API's own fields can work, so the other two
    /// are hidden. The mode in force is always listed, so the menu never
    /// omits what the control itself is showing.
    pub(super) fn available_sorts(&self) -> Vec<library_index::PlaylistSort> {
        library_index::PlaylistSort::ALL
            .into_iter()
            .filter(|mode| {
                *mode == self.sort || !self.index.is_empty() || mode.works_without_index()
            })
            .collect()
    }

    pub(super) fn set_playlist_sort(
        &mut self,
        sort: library_index::PlaylistSort,
        cx: &mut Context<Self>,
    ) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        services::AppServices::set_playlist_sort(sort, cx);
        self.refresh_rows(cx);
    }

    /// Opens or closes a folder in place, and remembers which it is.
    pub(super) fn toggle_folder(&mut self, uri: &str, cx: &mut Context<Self>) {
        if !self.expanded_folders.remove(uri) {
            self.expanded_folders.insert(uri.to_owned());
        }
        let mut folders: Vec<String> = self.expanded_folders.iter().cloned().collect();
        folders.sort();
        services::AppServices::set_expanded_folders(&folders, cx);
        self.refresh_rows(cx);
    }

    /// Opens or closes a folder in the pinned section, leaving the same
    /// folder in the list alone.
    pub(super) fn toggle_pinned_folder(&mut self, uri: &str, cx: &mut Context<Self>) {
        if !self.expanded_pins.remove(uri) {
            self.expanded_pins.insert(uri.to_owned());
        }
        self.refresh_rows(cx);
    }

    /// Rebuilds the drawn order. Every path that changes the index, the
    /// playlists, the sort or the open folders ends here, so the rows and
    /// what they came from cannot disagree.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let rows = library_index::rows(
            &self.index,
            &self.playlists,
            self.pins.uris(),
            self.sort,
            &self.expanded_folders,
            &self.expanded_pins,
        );
        self.rows = rows.all.into();
        self.pinned_rows = rows.pinned.into();
        // A pin names something the library may not draw — a podcast, an
        // artist, a playlist the rootlist has not caught up with. When the
        // section and the account disagree about what is pinned, this is
        // the line that says which side is missing what.
        log::debug!(
            "library: pins {:?} drew {:?}",
            self.pins.uris(),
            self.pinned_rows
                .iter()
                .map(library_index::LibraryRow::uri)
                .collect::<Vec<_>>()
        );
        cx.notify();
    }

    pub(super) fn loaded(&self) -> bool {
        self.loaded
    }

    /// The pinned rows, for the sidebar section that draws only those.
    pub(super) fn pinned_rows(&self) -> &Arc<[library_index::LibraryRow]> {
        &self.pinned_rows
    }

    pub(super) fn recently_played(&self) -> &Arc<[model::ListedTrack]> {
        &self.recently_played
    }

    pub(super) fn local_loaded(&self) -> bool {
        self.local_loaded
    }

    /// Whether the track is in the account's Spotify Liked Songs.
    pub(super) fn is_liked(&self, track: &model::Track) -> bool {
        track.provider == model::Provider::Spotify && self.liked_keys.contains(&track.source_id)
    }

    /// Takes a fresh Liked Songs collection, rebuilding the lookup with it.
    /// Every path that replaces the collection goes through here, so the two
    /// cannot disagree — including an optimistic change Spotify later
    /// contradicts, which the next refresh simply overwrites.
    fn set_liked_tracks(&mut self, tracks: Vec<model::ListedTrack>) {
        self.liked_keys = tracks
            .iter()
            .map(|listed| listed.track.source_id.clone())
            .collect();
        self.liked_tracks = tracks.into();
    }

    /// Takes a fresh playlist set and redraws the list with it. Every path
    /// that replaces the playlists goes through here, so the rows can never
    /// describe a set that is no longer on screen.
    fn set_playlists(&mut self, playlists: Vec<model::Playlist>, cx: &mut Context<Self>) {
        self.playlists = playlists.into();
        self.refresh_rows(cx);
    }

    /// Whether Spotify holds a pin for this playlist. Cadence keeps no pins
    /// of its own, so this is the account's answer, not this app's.
    pub(super) fn is_playlist_pinned(&self, playlist: &model::Playlist) -> bool {
        playlist.provider == model::Provider::Spotify
            && self.is_pinned(&library_index::playlist_uri(&playlist.source_id))
    }

    /// The same answer for anything the library draws, folders included.
    pub(super) fn is_pinned(&self, uri: &str) -> bool {
        self.pins.contains(uri)
    }

    /// Whether the playlist is in the account's library: placed by the
    /// rootlist, or listed by the Web API before the rootlist has arrived.
    /// A save still on its way answers as the click left it.
    pub(super) fn is_playlist_saved(&self, playlist: &model::Playlist) -> bool {
        let uri = library_index::playlist_uri(&playlist.source_id);
        if let Some(&saved) = self.pending_saves.get(&uri) {
            return saved;
        }
        self.index.entries.iter().any(|entry| entry.uri == uri)
            || self
                .playlists
                .iter()
                .any(|listed| listed.source_id == playlist.source_id)
    }

    /// Adds a playlist to the account's library or takes it out.
    ///
    /// The button answers the click at once; Spotify is told after, and the
    /// library read back from it is what settles the list.
    pub(super) fn set_playlist_saved(
        &mut self,
        playlist: &model::Playlist,
        saved: bool,
        cx: &mut Context<Self>,
    ) {
        if playlist.provider != model::Provider::Spotify {
            return;
        }
        let uri = library_index::playlist_uri(&playlist.source_id);
        self.pending_saves.insert(uri.clone(), saved);
        self.backend
            .send(BackendCommand::SetPlaylistSaved { uri, saved });
        cx.notify();
    }

    /// Whether one more pin would go past what Spotify is known to accept.
    /// See [`PIN_WARNING_LIMIT`]: the interface warns on this, and nothing
    /// stops the write.
    pub(super) fn pins_at_limit(&self) -> bool {
        self.pins.len() >= PIN_WARNING_LIMIT
    }

    /// Pins or unpins a playlist on the account.
    ///
    /// The section here moves at once, so the button answers the click.
    /// Spotify is told after, and what it holds is what settles the section.
    pub(super) fn set_playlist_pinned(
        &mut self,
        playlist: &model::Playlist,
        pinned: bool,
        cx: &mut Context<Self>,
    ) {
        // A pin is a Spotify account's, so nothing else can carry one.
        if playlist.provider != model::Provider::Spotify {
            return;
        }
        let uri = library_index::playlist_uri(&playlist.source_id);
        if pinned {
            self.pins.pin(&uri);
        } else {
            self.pins.unpin(&uri);
        }
        self.refresh_rows(cx);
        self.backend.send(BackendCommand::SetPinned { uri, pinned });
    }

    /// Moves a pin to where another one sits, which is what dropping a row
    /// on another row means. The section moves at once; Spotify is told
    /// after, and what it holds is what settles the order.
    pub(super) fn move_pin(&mut self, uri: &str, target: &str, cx: &mut Context<Self>) {
        if uri == target {
            return;
        }
        self.pins.move_onto(uri, target);
        self.refresh_rows(cx);
        self.backend.send(BackendCommand::MovePin {
            uri: uri.to_owned(),
            target: target.to_owned(),
        });
    }

    /// Marks the catalog as settled without contents, for when the fetch failed.
    pub(super) fn mark_loaded(&mut self, cx: &mut Context<Self>) {
        self.loaded = true;
        cx.notify();
    }

    /// Likes or unlikes a track on Spotify. The collection here moves at
    /// once so the heart answers the click; Spotify is told after, and the
    /// next refresh is what settles any disagreement.
    pub(super) fn set_liked(
        &mut self,
        track: model::Track,
        liked: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut tracks = self.liked_tracks.to_vec();
        tracks.retain(|listed| listed.track.source_id != track.source_id);
        if liked {
            // Spotify lists the collection newest first, and this is the newest.
            tracks.insert(
                0,
                model::ListedTrack {
                    track: track.clone(),
                    added_at: Some(chrono::Utc::now()),
                },
            );
        }
        self.set_liked_tracks(tracks);
        cx.notify();
        self.backend.send(BackendCommand::SetLiked { track, liked })
    }

    /// Forgets the account's catalog. Locally-owned state stays: it is not tied
    /// to the Spotify account and the backend re-sends it regardless.
    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.reload = None;
        self.refreshed_at = None;
        self.set_liked_tracks(Vec::new());
        self.index = library_index::LibraryIndex::default();
        self.pins = Pins::default();
        self.set_playlists(Vec::new(), cx);
        self.loaded = false;
    }

    /// Applies the library half of a backend event, returning the event when the
    /// surrounding app still has its own work to do for it.
    pub(super) fn handle_backend_event(
        &mut self,
        event: BackendEvent,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Option<BackendEvent> {
        match event {
            BackendEvent::LibraryLoaded {
                generation: loaded_generation,
                liked_tracks,
                playlists,
            } => {
                if loaded_generation == generation {
                    self.set_liked_tracks(liked_tracks);
                    self.set_playlists(playlists, cx);
                    self.loaded = true;
                    self.refreshed_at = Some(SystemTime::now());
                    cx.emit(LibraryLoaded);
                }
            }
            BackendEvent::CachedLikedTracks {
                generation: cached_generation,
                tracks,
            } => {
                if cached_generation == generation {
                    self.set_liked_tracks(tracks);
                }
            }
            BackendEvent::LocalStateLoaded {
                pins,
                recently_played,
                library_index,
            } => {
                self.pins = pins;
                self.recently_played = recently_played
                    .into_iter()
                    .map(model::ListedTrack::undated)
                    .collect();
                self.local_loaded = true;
                self.index = library_index;
                self.refresh_rows(cx);
                cx.emit(LibraryLoaded);
            }
            BackendEvent::PinsLoaded {
                generation: pins_generation,
                pins,
            } => {
                if pins_generation == generation {
                    self.pins = pins;
                    self.refresh_rows(cx);
                }
            }
            BackendEvent::LibraryOrderLoaded {
                generation: order_generation,
                index,
            } => {
                if order_generation == generation {
                    self.index = index;
                    // The library read back is the answer to every save
                    // still pending, whichever way it went.
                    self.pending_saves.clear();
                    self.refresh_rows(cx);
                }
            }
            BackendEvent::ContextPlayed { uri, played_at_ms } => {
                self.index.mark_played(&uri, played_at_ms);
                self.refresh_rows(cx);
            }
            event => return Some(event),
        }
        cx.notify();
        None
    }
}
