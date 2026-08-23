use super::*;

use page::PageEvent;

/// A virtualized table of tracks with a per-row action menu.
///
/// The list owns which row's menu is open, and a click landing anywhere else
/// dismisses it. Dismissal that no click drives -- a keyboard route change, a
/// scroll outside the table -- is the workspace's to trigger, via
/// `close_menu`, because the menu outlives the page going off screen.
///
/// Lists whose context dates and sorts its tracks (playlists, liked songs)
/// carry a `sort_key`; clicking their headers cycles Title/Album/Date added
/// through A-Z, Z-A, and back to the default order, and the choice survives
/// restarts. The rows always play in the displayed order.
pub(super) struct TrackList {
    /// Set by `show`, which always runs before the list is first painted.
    id: Option<ElementId>,
    /// The context's tracks in default order: the sequence Spotify reports.
    listed: Arc<[model::ListedTrack]>,
    /// Display position to index into `listed`; identity while unsorted.
    order: Arc<[usize]>,
    /// Where this list's playback starts from, which gates Smart Shuffle.
    context_kind: ContextKind,
    /// This list's sort persistence key, when its context sorts at all.
    sort_key: Option<String>,
    sort: Option<model::ListSort>,
    /// The row whose action menu is open, keyed by source ID and row index so
    /// the same track appearing twice opens only the row that was clicked.
    menu_open: Option<String>,
    /// Album already on screen, whose "Go to album" entry would go nowhere.
    current_album_id: Option<String>,
    library: Entity<library::Library>,
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
}

impl EventEmitter<PageEvent> for TrackList {}

impl TrackList {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            id: None,
            listed: Arc::default(),
            order: Arc::default(),
            context_kind: ContextKind::default(),
            sort_key: None,
            sort: None,
            menu_open: None,
            current_album_id: None,
            library: services::AppServices::library(cx),
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
        }
    }

    /// Shows `listed` under `id`, which pages vary per playlist or album so
    /// that opening a different one starts back at the top of the list.
    /// `context_kind` rides along so starting playback from any row carries
    /// the right Smart Shuffle gate. `sort_key` enables the sortable headers
    /// and names the row the sort persists under; `None` keeps plain labels.
    ///
    /// Pages call this from `render`, so the early return below is what keeps
    /// the notify cycle finite: callers must pass a stored `Arc` clone, not a
    /// slice rebuilt every frame, or every render schedules another one.
    pub(super) fn show(
        &mut self,
        id: impl Into<ElementId>,
        listed: Arc<[model::ListedTrack]>,
        sort_key: Option<&str>,
        context_kind: ContextKind,
        cx: &mut Context<Self>,
    ) {
        let id = Some(id.into());
        let sort_key = sort_key.map(str::to_owned);
        if self.id == id && self.sort_key == sort_key && Arc::ptr_eq(&self.listed, &listed) {
            return;
        }
        if self.sort_key != sort_key {
            // A different context took over the list; restore whatever the
            // listener last chose for it. Storage trouble degrades silently
            // to the default order rather than blocking the page.
            self.sort_key = sort_key;
            self.sort = self
                .sort_key
                .as_deref()
                .and_then(|key| services::AppServices::list_sort(key, cx));
        }
        self.id = id;
        self.listed = listed;
        self.context_kind = context_kind;
        self.menu_open = None;
        self.refresh_order();
        cx.notify();
    }

    /// Recomputes the display permutation from the stored sort. Called on
    /// every content or sort change, so the two can never disagree.
    fn refresh_order(&mut self) {
        self.order = model::list_order(&self.listed, self.sort).into();
    }

    /// The context's tracks in displayed order — what the rows show right
    /// now, and what starting playback from the top should play.
    pub(super) fn displayed_tracks(&self) -> Vec<model::Track> {
        self.order
            .iter()
            .filter_map(|&default_index| self.listed.get(default_index))
            .map(|entry| entry.track.clone())
            .collect()
    }

    /// Moves `column`'s header to its next state and remembers the choice.
    fn toggle_sort(&mut self, column: model::ListSortColumn, cx: &mut Context<Self>) {
        let Some(key) = self.sort_key.clone() else {
            return;
        };
        self.sort = model::ListSort::cycle(self.sort, column);
        services::AppServices::set_list_sort(&key, self.sort, cx);
        self.menu_open = None;
        self.refresh_order();
        cx.notify();
    }

    /// Suppresses the "Go to album" entry for the album the page is showing.
    pub(super) fn set_current_album_id(
        &mut self,
        source_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.current_album_id != source_id {
            self.current_album_id = source_id;
            cx.notify();
        }
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_open.take().is_some() {
            cx.notify();
        }
    }

    fn row(
        &mut self,
        index: usize,
        columns: TrackTableColumns,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(&default_index) = self.order.get(index) else {
            return div().into_any_element();
        };
        let Some(entry) = self.listed.get(default_index).cloned() else {
            return div().into_any_element();
        };
        let palette = appearance::Appearance::palette(cx);
        let is_current_track = self.player.read(cx).is_current_track(&entry.track);
        let favorite = self.library.read(cx).is_favorite(&entry.track);
        let menu_key = format!("{}:{index}", entry.track.source_id);
        let menu_open = self.menu_open.as_deref() == Some(menu_key.as_str());
        let menu = menu_open
            .then(|| self.action_menu(&entry.track, index, favorite, is_current_track, cx))
            .map(IntoElement::into_any_element);
        let favorite_track = entry.track.clone();
        let mut row = track_row::TrackRow::new(
            index,
            default_index + 1,
            entry.track,
            palette,
            self.image_cache.clone(),
            columns,
        )
        .current(is_current_track)
        .favorite(favorite)
        .menu(menu_open, menu)
        .on_play(cx.listener(move |this, _, _, cx| this.play_from(index, cx)))
        .on_favorite(cx.listener(move |this, _, _, cx| {
            this.library.update(cx, |library, cx| {
                library.set_favorite(favorite_track.clone(), !favorite, cx)
            });
        }))
        .on_toggle_menu(cx.listener(move |this, _, _, cx| {
            this.menu_open = (!menu_open).then(|| menu_key.clone());
            cx.notify();
        }));
        if let Some(added_at) = entry.added_at {
            row = row.added_label(track_row::format_added_at(chrono::Utc::now(), added_at));
        }
        row.into_any_element()
    }

    /// Plays from `index` as displayed: the queue starts as the visible
    /// ordering of the context, sorted or default alike.
    fn play_from(&mut self, index: usize, cx: &mut Context<Self>) {
        let tracks = self.displayed_tracks();
        let kind = self.context_kind;
        self.player.update(cx, |player, cx| {
            player.play_context(tracks, index, kind, cx)
        });
    }

    fn action_menu(
        &self,
        track: &model::Track,
        index: usize,
        favorite: bool,
        is_current_track: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let palette = appearance::Appearance::palette(cx);
        let has_playback_context = self.player.read(cx).now_playing().is_some();
        let next_track = track.clone();
        let queue_track = track.clone();
        let radio_track = track.clone();
        let favorite_track = track.clone();
        let artist = track
            .artists
            .iter()
            .find(|artist| artist.source_id.is_some())
            .cloned();
        let album = track
            .album_ref
            .clone()
            .filter(|album| album.source_id.is_some())
            .filter(|album| album.source_id != self.current_album_id);
        let track_url = format!("https://open.spotify.com/track/{}", track.source_id);
        let separator = || {
            div()
                .mx(px(4.))
                .my(px(4.))
                .border_t_1()
                .border_color(rgb(palette.border))
        };

        components::menu_surface(palette)
            .on_mouse_up_out(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_menu(cx)),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(
                components::text_menu_item(palette, ("track-menu-play", index), "Play now")
                    .when(is_current_track, |item| {
                        item.cursor_default().text_color(rgb(palette.text_muted))
                    })
                    .when(!is_current_track, |item| {
                        item.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu_open = None;
                            this.play_from(index, cx);
                        }))
                    }),
            )
            .child(
                components::text_menu_item(palette, ("track-menu-next", index), "Play next")
                    .when(!has_playback_context, |item| {
                        item.cursor_default().text_color(rgb(palette.text_muted))
                    })
                    .when(has_playback_context, |item| {
                        item.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu_open = None;
                            this.player
                                .update(cx, |player, cx| player.play_next(next_track.clone(), cx));
                            cx.notify();
                        }))
                    }),
            )
            .child(
                components::text_menu_item(palette, ("track-menu-queue", index), "Add to queue")
                    .when(!has_playback_context, |item| {
                        item.cursor_default().text_color(rgb(palette.text_muted))
                    })
                    .when(has_playback_context, |item| {
                        item.on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu_open = None;
                            this.player.update(cx, |player, cx| {
                                player.append_to_queue(queue_track.clone(), cx)
                            });
                            cx.notify();
                        }))
                    }),
            )
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-radio", index),
                    "Start track radio",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    cx.emit(PageEvent::StartRadio(radio_track.clone()));
                    cx.notify();
                })),
            )
            .child(separator())
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-favorite", index),
                    if favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.library.update(cx, |library, cx| {
                        library.set_favorite(favorite_track.clone(), !favorite, cx)
                    });
                    cx.notify();
                })),
            )
            .child(separator())
            .when_some(artist, |menu, artist| {
                menu.child(
                    components::text_menu_item(
                        palette,
                        ("track-menu-artist", index),
                        "Go to artist",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.menu_open = None;
                        cx.emit(PageEvent::OpenArtist(artist.clone()));
                    })),
                )
            })
            .when_some(album, |menu, album| {
                menu.child(
                    components::text_menu_item(palette, ("track-menu-album", index), "Go to album")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu_open = None;
                            cx.emit(PageEvent::OpenAlbum(album.clone()));
                        })),
                )
            })
            .child(
                components::text_menu_item(
                    palette,
                    ("track-menu-spotify", index),
                    "Open track in Spotify",
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    cx.open_url(&track_url);
                    cx.notify();
                })),
            )
    }
}

impl Render for TrackList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let mut columns = track_table_columns(f32::from(window.viewport_size().width));
        // A context without dates never shows the column, however wide the
        // window: albums and search results have nothing to put in it.
        columns.date_added &= self.sort_key.is_some();
        let sortable = self.sort_key.is_some();
        // A list whose context does not sort renders inert labels even if a
        // stale sort survived in memory; the two never combine.
        let active_sort = sortable.then_some(self.sort).flatten();
        let header_action = |column| -> Option<track_row::RowCallback> {
            sortable.then(|| {
                Box::new(cx.listener(move |this, _, _, cx| this.toggle_sort(column, cx)))
                    as track_row::RowCallback
            })
        };
        let actions = track_row::TrackHeaderActions {
            title: header_action(model::ListSortColumn::Title),
            album: columns
                .album
                .then(|| header_action(model::ListSortColumn::Album))
                .flatten(),
            date_added: columns
                .date_added
                .then(|| header_action(model::ListSortColumn::DateAdded))
                .flatten(),
        };
        div()
            .id("track-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(20.))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border))
            .child(track_row::track_list_header(
                palette,
                columns,
                active_sort,
                actions,
            ))
            .child(
                uniform_list(
                    self.id
                        .clone()
                        .expect("show sets the id before first paint"),
                    self.listed.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        range.map(|index| this.row(index, columns, cx)).collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

/// A virtualized table of playlists.
pub(super) struct PlaylistList {
    /// Set by `show`, which always runs before the list is first painted.
    id: Option<ElementId>,
    playlists: Arc<[model::Playlist]>,
    image_cache: Entity<image_cache::BoundedImageCache>,
}

impl EventEmitter<PageEvent> for PlaylistList {}

impl PlaylistList {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            id: None,
            playlists: Arc::default(),
            image_cache: services::AppServices::image_cache(cx),
        }
    }

    /// Shows `playlists` under `id`, which pages vary per content so that a
    /// new set starts back at the top. Same render-time contract as
    /// [`TrackList::show`]: pass a stored `Arc` clone, not a fresh slice.
    pub(super) fn show(
        &mut self,
        id: impl Into<ElementId>,
        playlists: Arc<[model::Playlist]>,
        cx: &mut Context<Self>,
    ) {
        let id = Some(id.into());
        if self.id == id && Arc::ptr_eq(&self.playlists, &playlists) {
            return;
        }
        self.id = id;
        self.playlists = playlists;
        cx.notify();
    }
}

impl Render for PlaylistList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(20.))
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border))
            .child(
                uniform_list(
                    self.id
                        .clone()
                        .expect("show sets the id before first paint"),
                    self.playlists.len(),
                    cx.processor(move |this, range: Range<usize>, _, cx| {
                        let playlists = this.playlists.clone();
                        range
                            .filter_map(|index| {
                                playlists.get(index).cloned().map(|playlist| {
                                    let selected = playlist.clone();
                                    track_row::PlaylistRow::new(
                                        index,
                                        playlist,
                                        palette,
                                        this.image_cache.clone(),
                                    )
                                    .on_open(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PageEvent::OpenPlaylist(selected.clone()));
                                    }))
                                    .into_any_element()
                                })
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
    }
}
