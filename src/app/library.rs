use super::*;

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
    /// The rows the playlist list draws, rebuilt whenever anything above
    /// changes so a render never has to sort.
    rows: Arc<[library_index::LibraryRow]>,
    pinned_playlists: Arc<[model::Playlist]>,
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
    pub(super) fn new(backend: BackendHandle, cx: &App) -> Self {
        Self {
            backend,
            liked_tracks: Arc::default(),
            liked_keys: HashSet::new(),
            playlists: Arc::default(),
            loaded: false,
            index: library_index::LibraryIndex::default(),
            sort: services::AppServices::playlist_sort(cx),
            expanded_folders: services::AppServices::expanded_folders(cx),
            rows: Arc::default(),
            pinned_playlists: Arc::default(),
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
        let fresh = self.refreshed_at.is_some_and(|refreshed_at| {
            refreshed_at
                .elapsed()
                .is_ok_and(|elapsed| elapsed < REVALIDATION_DEBOUNCE)
        });
        if self.reload.is_some() || fresh {
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

    /// Rebuilds the drawn order. Every path that changes the index, the
    /// playlists, the sort or the open folders ends here, so the rows and
    /// what they came from cannot disagree.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = library_index::rows(
            &self.index,
            &self.playlists,
            self.sort,
            &self.expanded_folders,
        )
        .into();
        cx.notify();
    }

    pub(super) fn loaded(&self) -> bool {
        self.loaded
    }

    pub(super) fn pinned_playlists(&self) -> &Arc<[model::Playlist]> {
        &self.pinned_playlists
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

    pub(super) fn is_playlist_pinned(&self, playlist: &model::Playlist) -> bool {
        self.pinned_playlists.iter().any(|candidate| {
            candidate.provider == playlist.provider && candidate.source_id == playlist.source_id
        })
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

    pub(super) fn set_playlist_pinned(
        &mut self,
        playlist: model::Playlist,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        cx.notify();
        self.backend
            .send(BackendCommand::SetPlaylistPinned { playlist, pinned })
    }

    /// Forgets the account's catalog. Locally-owned state stays: it is not tied
    /// to the Spotify account and the backend re-sends it regardless.
    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.reload = None;
        self.refreshed_at = None;
        self.set_liked_tracks(Vec::new());
        self.index = library_index::LibraryIndex::default();
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
                pinned_playlists,
                recently_played,
                library_index,
            } => {
                self.pinned_playlists = pinned_playlists.into();
                self.recently_played = recently_played
                    .into_iter()
                    .map(model::ListedTrack::undated)
                    .collect();
                self.local_loaded = true;
                self.index = library_index;
                self.refresh_rows(cx);
                cx.emit(LibraryLoaded);
            }
            BackendEvent::LibraryOrderLoaded {
                generation: order_generation,
                index,
            } => {
                if order_generation == generation {
                    self.index = index;
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
