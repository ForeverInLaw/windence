use super::*;

use page::PageEvent;

/// One of the track collections the library keeps for the signed-in account.
///
/// The two read different slices of `Library` and word themselves
/// differently, but the page around them is the same, so they share one entity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LibrarySection {
    LikedSongs,
    Recent,
}

/// The persistence key under which Liked Songs remembers its sort.
const LIKED_SORT_KEY: &str = "liked";

impl LibrarySection {
    fn page_id(self) -> &'static str {
        match self {
            Self::LikedSongs => "liked-songs-page",
            Self::Recent => "recent-page",
        }
    }

    fn list_id(self) -> &'static str {
        match self {
            Self::LikedSongs => "liked-tracks",
            Self::Recent => "recent-tracks",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::LikedSongs => "Liked Songs",
            Self::Recent => "Recently played",
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::LikedSongs => "No liked songs",
            Self::Recent => "No listening history yet",
        }
    }

    fn loading_message(self) -> &'static str {
        match self {
            Self::LikedSongs => "Loading liked songs…",
            Self::Recent => "Loading listening history…",
        }
    }

    fn tracks(self, library: &library::Library) -> Arc<[model::ListedTrack]> {
        match self {
            Self::LikedSongs => library.liked_tracks().clone(),
            Self::Recent => library.recently_played().clone(),
        }
    }

    /// Which lists remember their sort: Spotify's own collections. The local
    /// history keeps plain headers.
    fn context_id(self) -> Option<&'static str> {
        match self {
            Self::LikedSongs => Some(LIKED_SORT_KEY),
            Self::Recent => None,
        }
    }

    /// Whether an empty collection means "nothing here" rather than "not yet".
    /// Liked songs come from Spotify; the history is local state.
    fn loaded(self, library: &library::Library) -> bool {
        match self {
            Self::LikedSongs => library.loaded(),
            Self::Recent => library.local_loaded(),
        }
    }

    fn detail(self, library: &library::Library, track_count: usize) -> String {
        match self {
            Self::LikedSongs => components::revalidating_detail(
                if library.loaded() {
                    format!("{track_count} tracks loaded from Spotify")
                } else {
                    "Liked on Spotify".to_owned()
                },
                library.reloading(),
            ),
            Self::Recent => "Listening history".to_owned(),
        }
    }
}

/// A saved collection of tracks, listed straight from the library.
pub(super) struct LibraryTracksPage {
    section: LibrarySection,
    library: Entity<library::Library>,
    tracks: Entity<track_list::TrackList>,
    _tracks_subscription: Subscription,
}

impl EventEmitter<PageEvent> for LibraryTracksPage {}

impl LibraryTracksPage {
    pub(super) fn new(section: LibrarySection, cx: &mut Context<Self>) -> Self {
        let tracks = cx.new(|cx| track_list::TrackList::new(cx));
        Self {
            section,
            library: services::AppServices::library(cx),
            _tracks_subscription: page::forward(&tracks, cx),
            tracks,
        }
    }

    /// Takes down any open row menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.tracks.update(cx, |list, cx| list.close_menu(cx));
    }
}

impl Render for LibraryTracksPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let section = self.section;
        let (tracks, loaded, detail) = {
            let library = self.library.read(cx);
            let tracks = section.tracks(library);
            (
                tracks.clone(),
                section.loaded(library),
                section.detail(library, tracks.len()),
            )
        };
        let content = if tracks.is_empty() {
            let message = if loaded {
                section.empty_message()
            } else {
                section.loading_message()
            };
            components::empty_state(palette, message).into_any_element()
        } else {
            self.tracks.update(cx, |list, cx| {
                list.show(
                    section.list_id(),
                    tracks,
                    track_list::ListContext {
                        id: section.context_id().map(str::to_owned),
                        ..Default::default()
                    },
                    cx,
                )
            });
            self.tracks.clone().into_any_element()
        };

        components::page(section.page_id())
            .pt(px(12.))
            .child(components::page_heading(palette, section.title(), detail))
            .child(content)
    }
}

/// Every playlist the account follows on Spotify, in Spotify's own order.
pub(super) struct PlaylistsPage {
    library: Entity<library::Library>,
    playlists: Entity<track_list::PlaylistList>,
    /// Whether the sort menu is open. A click anywhere else closes it.
    sort_menu_open: bool,
    _playlists_subscription: Subscription,
}

impl EventEmitter<PageEvent> for PlaylistsPage {}

impl PlaylistsPage {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        let playlists = cx.new(|cx| track_list::PlaylistList::new(cx));
        Self {
            library: services::AppServices::library(cx),
            sort_menu_open: false,
            _playlists_subscription: page::forward(&playlists, cx),
            playlists,
        }
    }

    /// Takes down the sort menu, for a route change no click drove.
    pub(super) fn close_menus(&mut self, cx: &mut Context<Self>) {
        if self.sort_menu_open {
            self.sort_menu_open = false;
            cx.notify();
        }
    }

    /// The control that names the order and offers the others. Only the
    /// modes that can work in the current state are listed.
    fn sort_control(&mut self, palette: CadencePalette, cx: &mut Context<Self>) -> Div {
        let active = self.library.read(cx).playlist_sort();
        let available = self.library.read(cx).available_sorts();
        let menu = self.sort_menu_open.then(|| {
            available
                .into_iter()
                .fold(
                    components::menu_surface(palette)
                        .w(px(180.))
                        .on_mouse_up_out(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.close_menus(cx)),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|_, _, _, cx| cx.stop_propagation()),
                        ),
                    |menu, mode| {
                        menu.child(
                            components::text_menu_item(
                                palette,
                                (
                                    ElementId::from("playlist-sort-option"),
                                    SharedString::from(mode.as_str()),
                                ),
                                mode.label(),
                            )
                            .when(mode == active, |item| {
                                item.text_color(rgb(palette.text_primary))
                                    .bg(rgb(palette.selection))
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.sort_menu_open = false;
                                    this.library.update(cx, |library, cx| {
                                        library.set_playlist_sort(mode, cx)
                                    });
                                    cx.notify();
                                },
                            )),
                        )
                    },
                )
                .into_any_element()
        });

        div()
            .relative()
            .child(
                components::pill(palette, "playlist-sort", active.label(), false).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.sort_menu_open = !this.sort_menu_open;
                        cx.notify();
                    }),
                ),
            )
            .when_some(menu, |anchor, menu| {
                // Anchored elements start at the parent's own top-left, so
                // the drop is offset by the pill's height to sit under it.
                anchor.child(deferred(
                    anchored()
                        .offset(point(px(0.), px(components::PILL_HEIGHT + 4.)))
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(px(8.))
                        .child(menu),
                ))
            })
    }
}

impl Render for PlaylistsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let (rows, loaded, detail) = {
            let library = self.library.read(cx);
            (
                library.playlist_rows().clone(),
                library.loaded(),
                components::revalidating_detail("Your Spotify playlists", library.reloading()),
            )
        };
        let content = if rows.is_empty() {
            let message = if loaded {
                "No Spotify playlists"
            } else {
                "Loading playlists…"
            };
            components::empty_state(palette, message).into_any_element()
        } else {
            self.playlists
                .update(cx, |list, cx| list.show("spotify-playlists", rows, cx));
            self.playlists.clone().into_any_element()
        };
        let sort_control = self.sort_control(palette, cx);

        components::page("playlists-page")
            .pt(px(12.))
            .child(
                components::page_heading(palette, "Playlists", detail)
                    .child(div().flex_none().child(sort_control)),
            )
            .child(content)
    }
}
