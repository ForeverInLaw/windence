use super::*;
use catalog::{page_header, request};
use gpui_kit::TestSupportExt as _;
use icons::CadenceIcon;
use page::PageEvent;
use spotify_gpui_client::{backend::PlaylistRequest, playlist::PlaylistSnapshot};
use track_list::{PlaylistListEvent, TrackList};

/// What the station page lists while the station plays: the song on air
/// and everything the DJ has lined up behind it.
fn station_tracks(player: &player::Player) -> Arc<[model::ListedTrack]> {
    player
        .now_playing()
        .into_iter()
        .chain(player.queue().iter())
        .cloned()
        .map(model::ListedTrack::undated)
        .collect()
}

/// The tracks of one Spotify playlist.
pub(super) struct PlaylistPage {
    search_input: Entity<InputState>,
    _search_subscription: Subscription,
    backend: BackendHandle,
    selected: Option<model::Playlist>,
    tracks: Arc<[model::ListedTrack]>,
    loaded: bool,
    error: Option<String>,
    request: Option<gpui_kit::Task<()>>,
    requests: Option<tokio::sync::mpsc::Sender<PlaylistRequest>>,
    progress: Option<PlaylistProgress>,
    completion: Option<gpui_kit::Task<()>>,
    pending_play: Option<PendingPlay>,
    loading_more: bool,
    needs_complete: bool,
    library: Entity<library::Library>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscriptions: [Subscription; 2],
    /// The live station queue, rebuilt whenever the player moves. Held as
    /// one `Arc` so the list is only handed a new one when it really
    /// changed; a slice rebuilt each render would notify forever.
    station_tracks: Arc<[model::ListedTrack]>,
    /// Keeps `station_tracks` in step with what the player is playing.
    _player_subscription: Subscription,
}

struct PlaylistProgress {
    revision: String,
    next_offset: u32,
    total: u32,
}

impl PlaylistProgress {
    fn complete(&self) -> bool {
        self.next_offset >= self.total
    }
}

struct PendingPlay {
    default_index: Option<usize>,
    shuffled: bool,
    expected_revision: Option<String>,
    ready_revision: Option<String>,
}

impl EventEmitter<PageEvent> for PlaylistPage {}

impl PlaylistPage {
    pub(super) fn new(backend: BackendHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search in this playlist"));
        let search_subscription =
            cx.subscribe(&search_input, |page, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value();
                    page.track_list
                        .update(cx, |list, cx| list.set_filter(&query, cx));
                    if !query.trim().is_empty() {
                        page.ensure_complete(None, cx);
                    }
                    cx.notify();
                }
            });
        let player = services::AppServices::player(cx);
        let player_subscription = cx.observe(&player, |page, player, cx| {
            page.station_tracks = station_tracks(player.read(cx));
            cx.notify();
        });
        Self {
            search_input,
            _search_subscription: search_subscription,
            backend,
            selected: None,
            tracks: Arc::default(),
            loaded: false,
            error: None,
            request: None,
            requests: None,
            progress: None,
            completion: None,
            pending_play: None,
            loading_more: false,
            needs_complete: false,
            library: services::AppServices::library(cx),
            player,
            image_cache: services::AppServices::image_cache(cx),
            _list_subscriptions: [
                page::forward(&track_list, cx),
                cx.subscribe(
                    &track_list,
                    |page, _, event: &PlaylistListEvent, cx| match event {
                        PlaylistListEvent::LoadMore => page.load_more(cx),
                        PlaylistListEvent::Complete => page.ensure_complete(None, cx),
                        PlaylistListEvent::Play(default_index) => {
                            page.ensure_complete(Some((Some(*default_index), false)), cx);
                        }
                    },
                ),
            ],
            station_tracks: Arc::default(),
            _player_subscription: player_subscription,
            track_list,
        }
    }

    /// The source id of the playlist on the page, if one is open.
    pub(super) fn open_source_id(&self) -> Option<&str> {
        self.selected
            .as_ref()
            .map(|playlist| playlist.source_id.as_str())
    }

    /// The track list the page can show, whatever is on screen.
    pub(super) fn track_list_entity(&self) -> Entity<track_list::TrackList> {
        self.track_list.clone()
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// Starts the page's contents from the top, if there is anything to play.
    /// Plays what the rows show: the list's current sort order.
    pub(super) fn play(&mut self, cx: &mut Context<Self>) {
        // The station resolves its own songs: what the page lists is only
        // what is coming up, and the backend decides where to start.
        if self.open_source_id().is_some_and(dj::matches) {
            self.player.update(cx, |player, cx| player.play_dj(cx));
            return;
        }
        self.ensure_complete(Some((None, false)), cx);
    }

    fn play_loaded(&self, default_index: Option<usize>, shuffled: bool, cx: &mut Context<Self>) {
        let tracks = self.track_list.read(cx).sorted_context_tracks();
        if tracks.is_empty() {
            return;
        }
        let index = match default_index {
            Some(index) => {
                let Some(position) = self.track_list.read(cx).sorted_position(index) else {
                    return;
                };
                position
            }
            None => 0,
        };
        let uri = self.context_uri();
        self.player.update(cx, |player, cx| {
            if shuffled {
                player.play_context_shuffled(tracks, index, ContextKind::Collection, uri, cx)
            } else {
                player.play_context(tracks, index, ContextKind::Collection, uri, cx)
            }
        });
    }

    /// Starts the context shuffled and moves the global toggle to Shuffle.
    pub(super) fn play_shuffled(&mut self, cx: &mut Context<Self>) {
        self.ensure_complete(Some((None, true)), cx);
    }

    fn ensure_complete(&mut self, play: Option<(Option<usize>, bool)>, cx: &mut Context<Self>) {
        if play.is_none()
            && !self.needs_complete
            && self
                .progress
                .as_ref()
                .is_some_and(PlaylistProgress::complete)
        {
            return;
        }
        if self.requests.is_none() {
            if self.progress.is_none()
                && self.loaded
                && let Some((index, shuffled)) = play
            {
                self.play_loaded(index, shuffled, cx);
            }
            return;
        }
        if let Some((default_index, shuffled)) = play {
            self.pending_play = Some(PendingPlay {
                default_index,
                shuffled,
                expected_revision: default_index
                    .and(self.progress.as_ref().map(|p| p.revision.clone())),
                ready_revision: None,
            });
        }
        self.needs_complete = true;
        if self.completion.is_some() {
            return;
        }
        let (respond, reply) = tokio::sync::oneshot::channel();
        if self
            .requests
            .as_ref()
            .expect("stream checked above")
            .try_send(PlaylistRequest::Complete { respond })
            .is_err()
        {
            self.error = Some("Playlist loading is busy. Try again.".to_owned());
            cx.notify();
            return;
        }
        self.error = None;
        self.completion = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.completion = None;
                match result {
                    Ok(Ok(snapshot)) => {
                        if page.progress.as_ref().is_some_and(|progress| {
                            progress.complete() && progress.revision == snapshot.revision
                        }) {
                            page.needs_complete = false;
                        }
                        if let Some(play) = page.pending_play.as_mut() {
                            play.ready_revision = Some(snapshot.revision);
                        }
                        page.finish_play(cx);
                    }
                    Ok(Err(error)) => page.error = Some(format!("{error:#}")),
                    Err(_) => page.error = Some("Playlist loading stopped. Try again.".to_owned()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn finish_play(&mut self, cx: &mut Context<Self>) {
        let (Some(play), Some(progress)) = (&self.pending_play, &self.progress) else {
            return;
        };
        if !progress.complete()
            || play.ready_revision.as_deref() != Some(progress.revision.as_str())
        {
            return;
        }
        let play = self
            .pending_play
            .take()
            .expect("pending play checked above");
        if play
            .expected_revision
            .is_some_and(|revision| revision != progress.revision)
        {
            cx.emit(PageEvent::Notice((
                "Playlist changed. Select the track again.".to_owned(),
                NoticeSeverity::Confirmation,
            )));
            return;
        }
        self.play_loaded(play.default_index, play.shuffled, cx);
    }

    fn apply_snapshot(&mut self, snapshot: PlaylistSnapshot, cx: &mut Context<Self>) {
        if self.progress.as_ref().is_some_and(|p| {
            p.revision == snapshot.revision && p.next_offset > snapshot.next_offset
        }) {
            return;
        }
        self.progress = Some(PlaylistProgress {
            revision: snapshot.revision,
            next_offset: snapshot.next_offset,
            total: snapshot.total,
        });
        self.tracks = snapshot.tracks.into();
        self.loaded = true;
        self.loading_more = false;
        self.error = None;
        if let Some(playlist) = self.selected.as_mut() {
            playlist.track_count = snapshot.total;
        }
        if self.completion.is_none()
            && self
                .progress
                .as_ref()
                .is_some_and(PlaylistProgress::complete)
        {
            self.needs_complete = false;
        }
        self.show_tracks(self.tracks.clone(), cx);
        self.finish_play(cx);
        if !self.search_input.read(cx).value().trim().is_empty()
            || self.track_list.read(cx).sorted()
        {
            self.ensure_complete(None, cx);
        }
        cx.emit(PageEvent::Loaded);
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading_more
            || self.needs_complete
            || self.error.is_some()
            || self
                .progress
                .as_ref()
                .is_some_and(PlaylistProgress::complete)
        {
            return;
        }
        let Some(requests) = &self.requests else {
            return;
        };
        if requests.try_send(PlaylistRequest::NextPage).is_ok() {
            self.loading_more = true;
        } else {
            self.error = Some("Playlist loading is busy. Try again.".to_owned());
        }
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.resume(cx);
        if self.needs_complete
            || self
                .progress
                .as_ref()
                .is_some_and(PlaylistProgress::complete)
        {
            self.needs_complete = true;
            self.ensure_complete(None, cx);
        } else {
            self.load_more(cx);
        }
    }

    pub(super) fn suspend(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.requests = None;
        self.completion = None;
        self.pending_play = None;
        self.loading_more = false;
        self.needs_complete = false;
        cx.notify();
    }

    pub(super) fn resume(&mut self, cx: &mut Context<Self>) {
        if self.requests.is_none()
            && let Some(playlist) = self.selected.clone()
            && !dj::matches(&playlist.source_id)
        {
            self.start_stream(playlist, cx);
        }
    }

    fn start_stream(&mut self, playlist: model::Playlist, cx: &mut Context<Self>) {
        let (requests, receiver) = tokio::sync::mpsc::channel(8);
        let (updates, mut snapshots) = tokio::sync::mpsc::unbounded_channel();
        if !self.backend.send(BackendCommand::StreamPlaylist {
            playlist,
            requests: receiver,
            updates,
        }) {
            self.error = Some("Cadence backend is busy or not running".to_owned());
            cx.notify();
            return;
        }
        self.requests = Some(requests);
        self.request = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = snapshots.recv().await {
                if this
                    .update(cx, |page, cx| match result {
                        Ok(snapshot) => page.apply_snapshot(snapshot, cx),
                        Err(error) => {
                            page.loading_more = false;
                            page.error = Some(format!("{error:#}"));
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            let _ = this.update(cx, |page, cx| {
                page.requests = None;
                page.request = None;
                page.loading_more = false;
                if page.error.is_none() {
                    page.error = Some("Playlist loading stopped. Try again.".to_owned());
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Pins or unpins the open playlist on the account. Past the limit the
    /// listener is warned and the write is sent anyway; see
    /// [`library::PIN_WARNING_LIMIT`] for why the number is only a warning.
    fn toggle_pin(&mut self, pinned: bool, cx: &mut Context<Self>) {
        let Some(playlist) = self.selected.clone() else {
            return;
        };
        if !pinned && self.library.read(cx).pins_at_limit() {
            cx.emit(PageEvent::Notice((
                format!(
                    "Spotify usually stops at {} pins. Cadence will still try to pin this one.",
                    library::PIN_WARNING_LIMIT
                ),
                NoticeSeverity::Confirmation,
            )));
        }
        self.library.update(cx, |library, cx| {
            library.set_playlist_pinned(&playlist, !pinned, cx)
        });
        cx.notify();
    }

    /// Adds the open playlist to the account's library, or takes it out.
    fn toggle_saved(&mut self, saved: bool, cx: &mut Context<Self>) {
        let Some(playlist) = self.selected.clone() else {
            return;
        };
        self.library.update(cx, |library, cx| {
            library.set_playlist_saved(&playlist, !saved, cx)
        });
        cx.notify();
    }

    /// The Spotify uri of the open playlist, which playback reports so the
    /// playlist moves to the top of the library list.
    fn context_uri(&self) -> Option<String> {
        self.selected
            .as_ref()
            .map(|playlist| library_index::playlist_uri(&playlist.source_id))
    }

    fn show_tracks(&self, tracks: Arc<[model::ListedTrack]>, cx: &mut Context<Self>) {
        if let Some(playlist) = self.selected.as_ref() {
            let list_id = (
                ElementId::from("playlist-tracks"),
                playlist.source_id.clone(),
            );
            self.track_list.update(cx, |list, cx| {
                list.show(
                    list_id,
                    tracks,
                    track_list::ListContext::playlist(playlist),
                    cx,
                );
                list.set_paging(
                    self.progress.as_ref().is_some_and(|p| !p.complete()),
                    self.loading_more
                        || self.needs_complete
                        || self.completion.is_some()
                        || self.error.is_some(),
                    cx,
                );
            });
        }
    }

    pub(super) fn open(
        &mut self,
        playlist: model::Playlist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_source_id() != Some(playlist.source_id.as_str()) {
            self.clear_search(window, cx);
        }
        self.suspend(cx);
        self.progress = None;
        self.selected = Some(playlist.clone());
        self.tracks = Arc::default();
        self.show_tracks(self.tracks.clone(), cx);
        self.loaded = false;
        self.error = None;
        // A playing station is already showing what is ahead; asking the
        // session again would answer about a spot it has moved past.
        if dj::matches(&playlist.source_id) && self.player.read(cx).station() {
            self.request = None;
            self.station_tracks = station_tracks(self.player.read(cx));
            cx.notify();
            return;
        }
        if !dj::matches(&playlist.source_id) {
            self.start_stream(playlist, cx);
            return;
        }
        let reply = request(&self.backend, |respond| BackendCommand::LoadPlaylist {
            playlist,
            respond,
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.request = None;
                page.loaded = true;
                match result {
                    Ok(PlaylistContents::Loaded {
                        playlist: refreshed,
                        tracks,
                    }) => {
                        if let Some(refreshed) = refreshed {
                            page.selected = Some(refreshed);
                        }
                        page.tracks = tracks.into();
                        page.show_tracks(page.tracks.clone(), cx);
                        page.error = None;
                        cx.emit(PageEvent::Loaded);
                    }
                    Err(error) => {
                        page.error = Some(error.clone());
                        cx.emit(PageEvent::Failed(error));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_some() {
            window.focus(&self.search_input.read(cx).focus_handle(cx), cx);
        }
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.track_list
            .update(cx, |list, cx| list.set_filter("", cx));
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suspend(cx);
        self.progress = None;
        self.clear_search(window, cx);
        self.tracks = Arc::default();
        self.show_tracks(self.tracks.clone(), cx);
        self.selected = None;
        self.loaded = false;

        self.error = None;
        cx.notify();
    }
}

impl Render for PlaylistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let tracks = self.tracks.clone();
        let loaded = self.loaded;
        let pinned = self
            .selected
            .as_ref()
            .is_some_and(|playlist| self.library.read(cx).is_playlist_pinned(playlist));
        let saved = self
            .selected
            .as_ref()
            .is_some_and(|playlist| self.library.read(cx).is_playlist_saved(playlist));
        // The listener's own playlists are in the library by definition;
        // taking one out would be deleting it, which is not this button.
        let own = self.selected.as_ref().is_some_and(|playlist| {
            services::AppServices::session(cx)
                .read(cx)
                .profile()
                .is_some_and(|profile| profile.display_name == playlist.owner)
        });
        // The identity drives every page decision: which actions are hidden,
        // the artwork fallback, and whether the header already has a count.
        let source_id = self
            .selected
            .as_ref()
            .map(|playlist| playlist.source_id.clone())
            .unwrap_or_default();
        let is_dj = dj::matches(&source_id);
        let show_save = !is_dj && !own;
        // While the station plays, its page is a window on the live queue:
        // asking the session for a stretch would answer with the one after
        // the songs already queued.
        let live_station = is_dj && self.player.read(cx).station();
        let tracks = if live_station {
            self.station_tracks.clone()
        } else {
            tracks
        };
        let (name, detail) = match self.selected.as_ref() {
            None => ("Playlist".to_owned(), "Spotify playlist".to_owned()),
            Some(playlist) if is_dj => (
                playlist.name.clone(),
                match live_station {
                    true => "Spotify station · on air".to_owned(),
                    false => "Spotify station".to_owned(),
                },
            ),
            Some(playlist) => (
                playlist.name.clone(),
                format!("Spotify playlist · {} tracks", playlist.track_count),
            ),
        };
        let artwork_url = self
            .selected
            .as_ref()
            .and_then(|playlist| playlist.artwork_url.clone());
        let list = if let Some(error) = self.error.as_deref().filter(|_| !loaded) {
            components::empty_state(palette, format!("Unable to load playlist: {error}"))
                .into_any_element()
        } else if self.selected.is_some() && !tracks.is_empty() {
            self.show_tracks(tracks.clone(), cx);
            self.track_list.clone().into_any_element()
        } else if self.selected.is_none() {
            components::empty_state(palette, "No playlist open").into_any_element()
        } else if live_station {
            components::empty_state(palette, "Waiting for the DJ…").into_any_element()
        } else if loaded {
            components::empty_state(palette, "No tracks in this playlist").into_any_element()
        } else {
            components::empty_state(palette, "Loading playlist…").into_any_element()
        };
        components::page("playlist-page")
            .pt(px(8.))
            .child(
                page_header()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        artwork_url.as_deref(),
                        176.,
                        28.,
                        if is_dj {
                            CadenceIcon::Bot
                        } else {
                            CadenceIcon::ListMusic
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap(px(8.))
                            .child(components::page_title(palette, name))
                            .child(components::page_detail(palette, detail))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .mt(px(8.))
                                    .child(
                                        components::pill(palette, "playlist-play", "Play", true)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.play(cx);
                                            })),
                                    )
                                    .when(!dj::shuffle_hidden(&source_id), |actions| {
                                        actions.child(
                                            components::icon_button(
                                                palette,
                                                "playlist-shuffle",
                                                CadenceIcon::Shuffle,
                                                "Shuffle",
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.play_shuffled(cx);
                                                }),
                                            ),
                                        )
                                    })
                                    .when(show_save, |actions| {
                                        // "Add to Your Library": the same
                                        // write the official client makes,
                                        // so the playlist appears on every
                                        // device.
                                        actions.child(
                                            components::icon_button(
                                                palette,
                                                "playlist-save",
                                                if saved {
                                                    CadenceIcon::CircleCheck
                                                } else {
                                                    CadenceIcon::Plus
                                                },
                                                if saved {
                                                    "Remove from Your Library"
                                                } else {
                                                    "Add to Your Library"
                                                },
                                            )
                                            .bg(rgb(if saved {
                                                palette.selection
                                            } else {
                                                palette.control
                                            }))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.toggle_saved(saved, cx);
                                                }),
                                            ),
                                        )
                                    })
                                    .when(!dj::pin_hidden(&source_id), |actions| {
                                        // The pin belongs to the Spotify
                                        // account, so this button writes to
                                        // the account: what it does here it
                                        // does on every device.
                                        actions.child(
                                            components::icon_button(
                                                palette,
                                                "playlist-pin",
                                                if pinned {
                                                    CadenceIcon::PinFill
                                                } else {
                                                    CadenceIcon::Pin
                                                },
                                                if pinned { "Unpin playlist" } else { "Pin" },
                                            )
                                            .bg(rgb(if pinned {
                                                palette.selection
                                            } else {
                                                palette.control
                                            }))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.toggle_pin(pinned, cx);
                                                }),
                                            ),
                                        )
                                    }),
                            ),
                    ),
            )
            .when(self.selected.is_some(), |page| {
                page.child(
                    components::text_field_frame(palette, &self.search_input, window, cx)
                        .id("playlist-search-field")
                        .w_full()
                        .max_w(px(400.))
                        .min_w_0()
                        .h(tokens::CONTROL_SIZE)
                        .flex_none()
                        .gap(px(10.))
                        .mb(px(16.))
                        .child(components::icon(
                            CadenceIcon::Search,
                            16.,
                            palette.text_muted,
                        ))
                        .child(
                            Input::new(&self.search_input)
                                .id("playlist-search-input")
                                .aria_label("Search in this playlist")
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .cleanable(true)
                                .flex_1()
                                .min_w_0()
                                .h_full(),
                        ),
                )
            })
            .when(
                self.error.is_some()
                    || self.needs_complete
                    || self.loading_more
                    || self.progress.as_ref().is_some_and(|p| !p.complete()),
                |page| {
                    let status = if let Some(error) = &self.error {
                        format!("Unable to load more tracks: {error}")
                    } else if self.needs_complete {
                        if self
                            .progress
                            .as_ref()
                            .is_some_and(PlaylistProgress::complete)
                        {
                            "Checking playlist…".to_owned()
                        } else {
                            "Loading the full playlist…".to_owned()
                        }
                    } else if self.loading_more {
                        "Loading more tracks…".to_owned()
                    } else if let Some(progress) = &self.progress {
                        if progress.complete() {
                            String::new()
                        } else {
                            format!(
                                "{} of {} items loaded",
                                progress.next_offset, progress.total
                            )
                        }
                    } else {
                        String::new()
                    };
                    page.child(
                        div()
                            .id("playlist-loading-status")
                            .test_support()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .py(px(8.))
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(rgb(palette.text_muted))
                            .child(status)
                            .when(self.error.is_some(), |row| {
                                row.child(
                                    components::pill(
                                        palette,
                                        "playlist-retry-more",
                                        "Retry",
                                        false,
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                                )
                            })
                            .when(
                                self.error.is_none()
                                    && !self.loading_more
                                    && !self.needs_complete
                                    && self.progress.as_ref().is_some_and(|p| !p.complete()),
                                |row| {
                                    row.child(
                                        components::pill(
                                            palette,
                                            "playlist-load-more",
                                            "Load more",
                                            false,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                                    )
                                },
                            ),
                    )
                },
            )
            .child(list)
    }
}
