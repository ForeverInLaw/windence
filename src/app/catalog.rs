use super::*;

use super::icons::CadenceIcon;
use page::PageEvent;
use track_list::{PlaylistList, TrackList};

pub(super) use super::playlist_page::PlaylistPage;

/// Sends `command` and hands back the reply channel to await.
///
/// Dropping the returned future is what cancels the request: the page holds it
/// in a `Task`, so starting a new request drops the previous one and its answer
/// is discarded rather than overwriting fresher state.
pub(super) fn request<T, C>(
    backend: &BackendHandle,
    command: C,
) -> impl Future<Output = Result<T, String>> + use<T, C>
where
    T: Send + 'static,
    C: FnOnce(Reply<T>) -> BackendCommand,
{
    let (respond, reply) = tokio::sync::oneshot::channel();
    let sent = backend.send(command(respond));
    async move {
        if !sent {
            return Err("Windence backend is busy or not running".to_owned());
        }
        match reply.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(_) => Err("Windence backend stopped before answering".to_owned()),
        }
    }
}

/// Search results for the current query.
pub(super) struct SearchPage {
    backend: BackendHandle,
    query: String,
    /// The query the current results answer, keying the result lists so a
    /// new search starts back at the top.
    results_query: String,
    kind: SearchKind,
    tracks: Arc<[model::ListedTrack]>,
    /// Search results have no folders and no index behind them, so they are
    /// held as the flat rows the list draws.
    playlists: Arc<[library_index::LibraryRow]>,
    loaded: bool,
    searching: bool,
    error: Option<String>,
    request: Option<gpui_kit::Task<()>>,
    track_list: Entity<TrackList>,
    playlist_list: Entity<PlaylistList>,
    _list_subscriptions: [Subscription; 2],
}

impl EventEmitter<PageEvent> for SearchPage {}

impl SearchPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        let playlist_list = cx.new(|cx| PlaylistList::new(cx));
        Self {
            backend,
            query: String::new(),
            results_query: String::new(),
            kind: SearchKind::Tracks,
            tracks: Arc::default(),
            playlists: Arc::default(),
            loaded: false,
            searching: false,
            error: None,
            request: None,
            _list_subscriptions: [
                page::forward(&track_list, cx),
                page::forward(&playlist_list, cx),
            ],
            track_list,
            playlist_list,
        }
    }

    pub(super) fn set_query(&mut self, query: String, cx: &mut Context<Self>) {
        self.query = query;
        cx.notify();
    }

    /// The track list the page can show, whatever is on screen.
    pub(super) fn track_list_entity(&self) -> Entity<track_list::TrackList> {
        self.track_list.clone()
    }

    pub(super) fn set_kind(&mut self, kind: SearchKind, cx: &mut Context<Self>) {
        self.kind = kind;
        cx.notify();
    }

    /// Runs the trimmed query, reporting whether there was one to run.
    pub(super) fn submit(&mut self, cx: &mut Context<Self>) -> bool {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return false;
        }
        self.loaded = false;
        self.searching = true;
        self.error = None;
        let reply = request(&self.backend, {
            let query = query.clone();
            |respond| BackendCommand::SearchCatalog { query, respond }
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.request = None;
                page.searching = false;
                page.loaded = true;
                match result {
                    Ok((tracks, playlists)) => {
                        page.results_query = query;
                        page.tracks = model::ListedTrack::undated_slice(tracks);
                        page.playlists = library_index::LibraryRow::flat(playlists).into();
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
        true
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.results_query.clear();
        self.tracks = Arc::default();
        self.playlists = Arc::default();
        self.loaded = false;
        self.searching = false;
        self.error = None;
        cx.notify();
    }
}

/// One artist, their popular tracks and their discography.
pub(super) struct ArtistPage {
    backend: BackendHandle,
    reference: Option<model::ArtistRef>,
    artist: Option<model::Artist>,
    tracks: Arc<[model::ListedTrack]>,
    albums: Arc<[model::Album]>,
    section: ArtistSection,
    loaded: bool,
    error: Option<String>,
    loaded_at: Option<SystemTime>,
    request: Option<gpui_kit::Task<()>>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for ArtistPage {}

impl ArtistPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        Self {
            backend,
            reference: None,
            artist: None,
            tracks: Arc::default(),
            albums: Arc::default(),
            section: ArtistSection::Popular,
            loaded: false,
            error: None,
            loaded_at: None,
            request: None,
            image_cache: services::AppServices::image_cache(cx),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    pub(super) fn set_section(&mut self, section: ArtistSection, cx: &mut Context<Self>) {
        self.section = section;
        cx.notify();
    }

    /// The track list the page can show, whatever is on screen.
    pub(super) fn track_list_entity(&self) -> Entity<track_list::TrackList> {
        self.track_list.clone()
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// Shows `artist`, refetching unless the cached copy is still fresh.
    /// Reports whether this is a different artist than the one already shown.
    pub(super) fn open(&mut self, artist: model::ArtistRef, cx: &mut Context<Self>) -> bool {
        let Some(source_id) = artist.source_id.clone() else {
            return false;
        };
        let same_artist = self
            .reference
            .as_ref()
            .and_then(|artist| artist.source_id.as_deref())
            == Some(source_id.as_str());
        let retrying_failure = same_artist && self.error.is_some();
        let loading = self.request.is_some();
        let should_refresh = !same_artist || (!loading && !catalog_data_is_fresh(self.loaded_at));
        self.reference = Some(artist);
        self.error = None;
        if !same_artist {
            self.artist = None;
            self.tracks = Arc::default();
            self.albums = Arc::default();
            self.loaded = false;
            self.loaded_at = None;
            self.section = ArtistSection::Popular;
        } else if retrying_failure {
            self.loaded = false;
        }
        if should_refresh {
            let reply = request(&self.backend, |respond| BackendCommand::LoadArtist {
                source_id,
                respond,
            });
            self.request = Some(cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |page, cx| {
                    page.request = None;
                    match result {
                        Ok((artist, tracks, albums)) => {
                            page.artist = Some(artist);
                            page.tracks = model::ListedTrack::undated_slice(tracks);
                            page.albums = albums.into();
                            page.loaded = true;
                            page.error = None;
                            page.loaded_at = Some(SystemTime::now());
                            cx.emit(PageEvent::Loaded);
                        }
                        Err(error) => {
                            if !page.loaded {
                                page.loaded = true;
                                page.error = Some(error.clone());
                            }
                            cx.emit(PageEvent::Failed(error));
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
        !same_artist
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.reference = None;
        self.artist = None;
        self.tracks = Arc::default();
        self.albums = Arc::default();
        self.loaded = false;
        self.error = None;
        self.loaded_at = None;
        cx.notify();
    }

    /// The release grid, laid out as rows of fixed-width cards so that the
    /// whole discography can be virtualized like a list.
    fn discography(
        &mut self,
        albums: Arc<[model::Album]>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let columns = if uses_compact_content_layout(self.track_list.read(cx).content_width()) {
            3
        } else {
            4
        };
        let row_count = albums.len().div_ceil(columns);
        uniform_list(
            "artist-discography",
            row_count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let start = row * columns;
                        let end = (start + columns).min(albums.len());
                        let mut cards =
                            div().h(px(256.)).w_full().flex().items_start().gap(px(12.));
                        for index in start..end {
                            cards = cards.child(this.album_card(&albums[index], index, cx));
                        }
                        for _ in end..start + columns {
                            cards = cards.child(div().flex_1().min_w_0());
                        }
                        cards
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
    }

    fn album_card(&self, album: &model::Album, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let year = album
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .unwrap_or("Release")
            .to_owned();

        components::button(palette, ("artist-album", index))
            .flex_1()
            .min_w_0()
            .h(px(244.))
            .p(px(10.))
            .flex_col()
            .items_center()
            .justify_start()
            .rounded(px(16.))
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(
                div()
                    .w(px(152.))
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(10.))
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        album.artwork_url.as_deref(),
                        152.,
                        14.,
                        CadenceIcon::Music,
                    ))
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(album.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(palette.text_muted))
                            .child(year),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(album) = this.albums.get(index) {
                    cx.emit(PageEvent::OpenAlbum(model::AlbumRef {
                        name: album.name.clone(),
                        source_id: Some(album.source_id.clone()),
                        spotify_uri: album.spotify_uri.clone(),
                        artwork_url: album.artwork_url.clone(),
                    }));
                }
            }))
            .into_any_element()
    }
}

/// One album and its tracks.
pub(super) struct AlbumPage {
    backend: BackendHandle,
    reference: Option<model::AlbumRef>,
    album: Option<model::Album>,
    tracks: Arc<[model::ListedTrack]>,
    loaded: bool,
    error: Option<String>,
    loaded_at: Option<SystemTime>,
    request: Option<gpui_kit::Task<()>>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    track_list: Entity<TrackList>,
    _list_subscription: Subscription,
}

impl EventEmitter<PageEvent> for AlbumPage {}

impl AlbumPage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        let track_list = cx.new(|cx| TrackList::new(cx));
        Self {
            backend,
            reference: None,
            album: None,
            tracks: Arc::default(),
            loaded: false,
            error: None,
            loaded_at: None,
            request: None,
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
            _list_subscription: page::forward(&track_list, cx),
            track_list,
        }
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.track_list.update(cx, |list, cx| list.close_menu(cx));
    }

    /// The track list the page can show, whatever is on screen.
    pub(super) fn track_list_entity(&self) -> Entity<track_list::TrackList> {
        self.track_list.clone()
    }

    /// Starts the page's contents from the top, if there is anything to play.
    pub(super) fn play(&mut self, tracks: &Arc<[model::ListedTrack]>, cx: &mut Context<Self>) {
        if tracks.is_empty() {
            return;
        }
        let tracks = model::ListedTrack::tracks(tracks);
        self.player.update(cx, |player, cx| {
            player.play_context(tracks, 0, ContextKind::Album, None, cx)
        });
    }

    /// Starts the album shuffled; albums get plain shuffle only.
    pub(super) fn play_shuffled(
        &mut self,
        tracks: &Arc<[model::ListedTrack]>,
        cx: &mut Context<Self>,
    ) {
        if tracks.is_empty() {
            return;
        }
        let tracks = model::ListedTrack::tracks(tracks);
        self.player.update(cx, |player, cx| {
            player.play_context_shuffled(tracks, 0, ContextKind::Album, None, cx)
        });
    }

    /// Shows `album`, refetching unless the cached copy is still fresh.
    /// Reports whether this is a different album than the one already shown.
    pub(super) fn open(&mut self, album: model::AlbumRef, cx: &mut Context<Self>) -> bool {
        let Some(source_id) = album.source_id.clone() else {
            return false;
        };
        let same_album = self
            .reference
            .as_ref()
            .and_then(|album| album.source_id.as_deref())
            == Some(source_id.as_str());
        let retrying_failure = same_album && self.error.is_some();
        let loading = self.request.is_some();
        let should_refresh = !same_album || (!loading && !catalog_data_is_fresh(self.loaded_at));
        self.reference = Some(album);
        self.error = None;
        self.track_list.update(cx, |list, cx| {
            list.set_current_album_id(Some(source_id.clone()), cx)
        });
        if !same_album {
            self.album = None;
            self.tracks = Arc::default();
            self.loaded = false;
            self.loaded_at = None;
        } else if retrying_failure {
            self.loaded = false;
        }
        if should_refresh {
            let reply = request(&self.backend, |respond| BackendCommand::LoadAlbum {
                source_id,
                respond,
            });
            self.request = Some(cx.spawn(async move |this, cx| {
                let result = reply.await;
                let _ = this.update(cx, |page, cx| {
                    page.request = None;
                    match result {
                        Ok((album, tracks)) => {
                            page.album = Some(album);
                            page.tracks = model::ListedTrack::undated_slice(tracks);
                            page.loaded = true;
                            page.error = None;
                            page.loaded_at = Some(SystemTime::now());
                            cx.emit(PageEvent::Loaded);
                        }
                        Err(error) => {
                            if !page.loaded {
                                page.loaded = true;
                                page.error = Some(error.clone());
                            }
                            cx.emit(PageEvent::Failed(error));
                        }
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
        !same_album
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.reference = None;
        self.album = None;
        self.tracks = Arc::default();
        self.loaded = false;
        self.error = None;
        self.loaded_at = None;
        self.track_list
            .update(cx, |list, cx| list.set_current_album_id(None, cx));
        cx.notify();
    }
}

impl Render for SearchPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let kind = self.kind;
        let query = self.query.clone();
        let tracks = self.tracks.clone();
        let playlists = self.playlists.clone();
        let searching = self.searching;
        let loaded = self.loaded;
        let results = if let Some(error) = self.error.as_deref() {
            components::empty_state(palette, format!("Unable to search Spotify: {error}"))
                .into_any_element()
        } else if kind == SearchKind::Tracks && !tracks.is_empty() {
            let list_id = (ElementId::from("search-tracks"), self.results_query.clone());
            self.track_list.update(cx, |list, cx| {
                list.show(
                    list_id,
                    tracks.clone(),
                    track_list::ListContext::default(),
                    cx,
                )
            });
            self.track_list.clone().into_any_element()
        } else if kind == SearchKind::Playlists && !playlists.is_empty() {
            let list_id = (
                ElementId::from("search-playlists"),
                self.results_query.clone(),
            );
            self.playlist_list
                .update(cx, |list, cx| list.show(list_id, playlists, cx));
            self.playlist_list.clone().into_any_element()
        } else {
            let message = match (loaded, searching) {
                (true, _) if kind == SearchKind::Tracks => "No tracks found",
                (true, _) => "No playlists found",
                (_, true) => "Searching…",
                _ => "Press Return to search",
            };
            components::empty_state(palette, message).into_any_element()
        };

        components::page("search-page")
            .pt(px(12.))
            .overflow_hidden()
            .child(components::page_heading(
                palette,
                "Search results",
                format!("Results for {query}"),
            ))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .mb(px(20.))
                    .child(
                        components::pill(
                            palette,
                            "tab-tracks",
                            "Tracks",
                            kind == SearchKind::Tracks,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_kind(SearchKind::Tracks, cx)),
                        ),
                    )
                    .child(
                        components::pill(
                            palette,
                            "tab-playlists",
                            "Playlists",
                            kind == SearchKind::Playlists,
                        )
                        .on_click(
                            cx.listener(|this, _, _, cx| this.set_kind(SearchKind::Playlists, cx)),
                        ),
                    ),
            )
            .child(results)
    }
}

impl Render for ArtistPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let name = self
            .artist
            .as_ref()
            .map(|artist| artist.name.clone())
            .or_else(|| self.reference.as_ref().map(|artist| artist.name.clone()))
            .unwrap_or_else(|| "Artist".to_owned());
        let artwork_url = self
            .artist
            .as_ref()
            .and_then(|artist| artist.artwork_url.clone());
        let source_id = self
            .reference
            .as_ref()
            .and_then(|artist| artist.source_id.clone())
            .unwrap_or_default();
        let detail = if self.loaded && self.error.is_none() {
            format!("Spotify artist · {} releases", self.albums.len())
        } else {
            "Spotify artist".to_owned()
        };
        let section = self.section;
        let tracks = self.tracks.clone();
        let albums = self.albums.clone();
        let content = if let Some(error) = self.error.as_ref() {
            components::empty_state(palette, format!("Unable to load artist: {error}"))
                .into_any_element()
        } else if !self.loaded {
            components::empty_state(palette, "Loading artist…").into_any_element()
        } else if section == ArtistSection::Popular && !tracks.is_empty() {
            let list_id = (ElementId::from("artist-popular"), source_id);
            self.track_list.update(cx, |list, cx| {
                list.show(
                    list_id,
                    tracks.clone(),
                    track_list::ListContext::default(),
                    cx,
                )
            });
            self.track_list.clone().into_any_element()
        } else if section == ArtistSection::Popular {
            components::empty_state(palette, "No popular tracks yet").into_any_element()
        } else if albums.is_empty() {
            components::empty_state(palette, "No releases yet").into_any_element()
        } else {
            self.discography(albums, cx).into_any_element()
        };

        components::page("artist-page")
            .pt(px(8.))
            .child(
                page_header()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        artwork_url.as_deref(),
                        144.,
                        72.,
                        CadenceIcon::User,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(components::page_title(palette, name))
                            .child(components::page_detail(palette, detail)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(8.))
                    .mb(px(16.))
                    .child(
                        components::pill(
                            palette,
                            "artist-tab-popular",
                            "Popular",
                            section == ArtistSection::Popular,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_section(ArtistSection::Popular, cx)
                        })),
                    )
                    .child(
                        components::pill(
                            palette,
                            "artist-tab-discography",
                            "Discography",
                            section == ArtistSection::Discography,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_section(ArtistSection::Discography, cx)
                        })),
                    ),
            )
            .child(content)
    }
}

impl Render for AlbumPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let name = self
            .album
            .as_ref()
            .map(|album| album.name.clone())
            .or_else(|| self.reference.as_ref().map(|album| album.name.clone()))
            .unwrap_or_else(|| "Album".to_owned());
        let artwork_url = self
            .album
            .as_ref()
            .and_then(|album| album.artwork_url.clone())
            .or_else(|| {
                self.reference
                    .as_ref()
                    .and_then(|album| album.artwork_url.clone())
            });
        let detail = self.album.as_ref().map_or_else(
            || "Spotify album".to_owned(),
            |album| {
                let artists = album
                    .artists
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut details = vec![artists];
                if let Some(year) = album.release_date.as_deref().and_then(|date| date.get(..4)) {
                    details.push(year.to_owned());
                }
                if let Some(track_count) = album.track_count {
                    details.push(format!("{track_count} tracks"));
                }
                details.join(" · ")
            },
        );
        let tracks = self.tracks.clone();
        let loaded = self.loaded;
        let list = if let Some(error) = self.error.as_ref() {
            components::empty_state(palette, format!("Unable to load album: {error}"))
                .into_any_element()
        } else if !tracks.is_empty() {
            let source_id = self
                .reference
                .as_ref()
                .and_then(|album| album.source_id.clone())
                .expect("open only loads albums that have a source id");
            let list_id = (ElementId::from("album-tracks"), source_id);
            self.track_list.update(cx, |list, cx| {
                list.show(
                    list_id,
                    tracks.clone(),
                    track_list::ListContext {
                        kind: ContextKind::Album,
                        ..Default::default()
                    },
                    cx,
                )
            });
            self.track_list.clone().into_any_element()
        } else if loaded {
            components::empty_state(palette, "No playable tracks in this album").into_any_element()
        } else {
            components::empty_state(palette, "Loading album…").into_any_element()
        };
        let playback_tracks = tracks;
        let shuffle_tracks = playback_tracks.clone();

        components::page("album-page")
            .pt(px(8.))
            .child(
                page_header()
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        artwork_url.as_deref(),
                        176.,
                        28.,
                        CadenceIcon::Music,
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
                                        components::pill(palette, "album-play", "Play", true)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.play(&playback_tracks, cx);
                                            })),
                                    )
                                    .child(
                                        components::icon_button(
                                            palette,
                                            "album-shuffle",
                                            CadenceIcon::Shuffle,
                                            "Shuffle",
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.play_shuffled(&shuffle_tracks, cx);
                                            }),
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(list)
    }
}

/// The artwork-and-title band a detail page opens with.
pub(super) fn page_header() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(28.))
        .mb(px(24.))
}
