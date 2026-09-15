use super::*;

use super::icons::CadenceIcon;
use gpui_kit::TestSupportExt as _;

/// The transport strip pinned to the bottom of the window.
///
/// It redraws because it reads the `Player` entity, which gpui tracks per
/// window. Adding `.cached(..)` here would break that: `Player` is a model and
/// has no dispatch node, so it cannot mark this view dirty on its own.
pub(super) struct PlayerBar {
    player: Entity<player::Player>,
    library: Entity<library::Library>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    queue_open: bool,
    /// The width the bar's content spans: the window minus the sidebar.
    /// The workspace keeps it current every frame.
    content_width: f32,
}

/// Raised when the listener asks to see or hide the queue.
pub(super) struct ToggleQueue;

impl EventEmitter<ToggleQueue> for PlayerBar {}

impl EventEmitter<page::PageEvent> for PlayerBar {}

/// The album the playing track came from, when Spotify knows which it is.
fn playing_album(track: Option<&model::Track>) -> Option<model::AlbumRef> {
    track?
        .album_ref
        .clone()
        .filter(|album| album.source_id.is_some())
}

/// Who the bar credits, one entry per artist. A track stored with only its
/// joined artist names credits that whole line as a single plain entry.
fn artist_credits(track: Option<&model::Track>) -> Vec<model::ArtistRef> {
    let plain = |name: &str| model::ArtistRef {
        name: name.to_owned(),
        source_id: None,
        spotify_uri: None,
    };
    match track {
        Some(track) if !track.artists.is_empty() => track.artists.clone(),
        Some(track) => vec![plain(&track.artist)],
        None => vec![plain("")],
    }
}

impl PlayerBar {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            player: services::AppServices::player(cx),
            library: services::AppServices::library(cx),
            image_cache: services::AppServices::image_cache(cx),
            queue_open: false,
            // Replaced with the real value before the first paint; the
            // default only has to keep the layout math sane.
            content_width: 0.,
        }
    }

    pub(super) fn queue_open(&self) -> bool {
        self.queue_open
    }

    pub(super) fn set_queue_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.queue_open != open {
            self.queue_open = open;
            cx.notify();
        }
    }

    /// Takes the content width the workspace derived for this frame. The
    /// bar's tiers and its timeline fold read it instead of the window
    /// bounds, so a rail collapse or window resize lands in the same frame.
    pub(super) fn set_content_width(&mut self, width: f32, cx: &mut Context<Self>) {
        if self.content_width != width {
            self.content_width = width;
            cx.notify();
        }
    }

    fn navigate_on_click(
        event: page::PageEvent,
        cx: &mut Context<Self>,
    ) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |_, _, _, cx| cx.emit(event.clone()))
    }

    /// The artwork opens the album for the pointer only. The title beside it
    /// reaches the same page from the keyboard, and a focus ring drawn under
    /// the art would never show.
    fn artwork_link(
        &self,
        palette: CadencePalette,
        track: Option<&model::Track>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(track) = track else {
            return div()
                .size(px(56.))
                .rounded(px(12.))
                .bg(rgb(palette.surface_raised))
                .border_1()
                .border_color(palette.media_border)
                .into_any_element();
        };
        let artwork = components::artwork(
            palette,
            &self.image_cache,
            track.artwork_url.as_deref(),
            56.,
            12.,
            CadenceIcon::Music,
        );
        match playing_album(Some(track)) {
            Some(album) => gpui_kit::base::Button::new("player-artwork")
                .role(gpui_kit::Role::Link)
                .tab_stop(false)
                .accessibility_label(album.name.clone())
                .flex_none()
                .cursor_pointer()
                .on_click(Self::navigate_on_click(
                    page::PageEvent::OpenAlbum(album),
                    cx,
                ))
                .child(artwork)
                .into_any_element(),
            None => artwork,
        }
    }

    fn title_line(
        palette: CadencePalette,
        track: Option<&model::Track>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let title = SharedString::from(
            track
                .map_or("Nothing playing", |track| track.title.as_str())
                .to_owned(),
        );
        let text = div()
            .min_w_0()
            .truncate()
            .text_size(px(14.))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text_primary))
            .child(title);
        div().flex().min_w_0().child(match playing_album(track) {
            Some(album) => components::link(palette, "player-title", window)
                .min_w_0()
                .on_click(Self::navigate_on_click(
                    page::PageEvent::OpenAlbum(album),
                    cx,
                ))
                .child(text)
                .into_any_element(),
            None => text.into_any_element(),
        })
    }

    fn credits_line(
        palette: CadencePalette,
        track: Option<&model::Track>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut credits: Vec<gpui_kit::AnyElement> = Vec::new();
        for (index, artist) in artist_credits(track).into_iter().enumerate() {
            if index > 0 {
                credits.push(div().flex_none().child(", ").into_any_element());
            }
            credits.push(Self::artist_link(palette, index, artist, window, cx));
        }
        div()
            .flex()
            .min_w_0()
            .text_size(px(12.))
            .text_color(rgb(palette.text_muted))
            .children(credits)
    }

    fn artist_link(
        palette: CadencePalette,
        index: usize,
        artist: model::ArtistRef,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let name = div()
            .min_w_0()
            .truncate()
            .child(SharedString::from(artist.name.clone()));
        if artist.source_id.is_none() {
            return name.into_any_element();
        }
        components::link(palette, ("player-artist", index), window)
            .min_w_0()
            .on_click(Self::navigate_on_click(
                page::PageEvent::OpenArtist(artist),
                cx,
            ))
            .child(name)
            .into_any_element()
    }

    fn bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let content_width = self.content_width;
        let compact = uses_compact_player_layout(content_width);
        let timeline = compact_progress_slider_width(content_width);
        let player = self.player.read(cx);
        let now_playing = player.now_playing().cloned();
        let playing = player.playing();
        let loading = player.loading();
        let position_ms = player.position_ms();
        let volume = player.volume();
        let shuffle_mode = player.shuffle_mode();
        let shuffle_supported = player.shuffle_supported();
        let shuffle_smart_supported = player.shuffle_smart_supported();
        let duration = if let Some(track) = &now_playing {
            SharedString::from(format_duration(track.duration_ms))
        } else {
            SharedString::from("0:00")
        };
        let volume_icon = if volume == 0. {
            CadenceIcon::VolumeX
        } else {
            CadenceIcon::Volume2
        };
        let duration_ms = now_playing.as_ref().map_or(0, |track| track.duration_ms);
        let progress = if duration_ms == 0 {
            0.
        } else {
            (position_ms as f32 / duration_ms as f32).clamp(0., 1.)
        };
        div()
            .h(px(96.))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(PLAYER_BAR_GAP))
            .px(px(PLAYER_BAR_PADDING))
            .bg(rgb(palette.surface))
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .w(px(if compact {
                        COMPACT_PLAYER_LEFT_WIDTH
                    } else {
                        PLAYER_LEFT_WIDTH
                    }))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(self.artwork_link(palette, now_playing.as_ref(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Self::title_line(palette, now_playing.as_ref(), window, cx))
                            .child(Self::credits_line(
                                palette,
                                now_playing.as_ref(),
                                window,
                                cx,
                            )),
                    )
                    .child(self.liked_toggle(palette, now_playing, cx)),
            )
            .child(
                div()
                    .w(px(match timeline {
                        Some(slider_width) if compact => {
                            slider_width + 2. * PROGRESS_TIME_WIDTH + 2. * PROGRESS_GAP
                        }
                        Some(_) => PLAYER_CENTER_WIDTH,
                        // The timeline folded away: the transport stands
                        // alone at its own width.
                        None => TRANSPORT_CLUSTER_WIDTH,
                    }))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                shuffle_toggle(
                                    palette,
                                    shuffle_mode,
                                    shuffle_supported,
                                    shuffle_smart_supported,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.player
                                            .update(cx, |player, cx| player.cycle_shuffle(cx));
                                    },
                                )),
                            )
                            .child(
                                components::icon_button(palette, "previous", CadenceIcon::SkipBack)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.player.update(cx, |player, cx| player.previous(cx));
                                    })),
                            )
                            .child(
                                components::button(palette, "play-toggle")
                                    .test_support()
                                    .size(px(TRANSPORT_BUTTON_SIZE))
                                    .rounded(px(TRANSPORT_BUTTON_SIZE / 2.))
                                    .bg(rgb(palette.text_primary))
                                    .child(if loading {
                                        Spinner::new()
                                            .color(rgb(palette.on_accent).into())
                                            .into_any_element()
                                    } else {
                                        components::icon(
                                            if playing {
                                                CadenceIcon::Pause
                                            } else {
                                                CadenceIcon::Play
                                            },
                                            16.,
                                            palette.on_accent,
                                        )
                                        .into_any_element()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.player.update(cx, |player, cx| player.toggle(cx));
                                    })),
                            )
                            .child(
                                components::icon_button(palette, "next", CadenceIcon::SkipForward)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.player.update(cx, |player, cx| player.next(cx));
                                    })),
                            ),
                    )
                    .when_some(timeline, |centre, slider_width| {
                        centre.child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .text_size(px(11.))
                                .text_color(rgb(palette.text_muted))
                                .child(
                                    div()
                                        .w(px(PROGRESS_TIME_WIDTH))
                                        .flex_none()
                                        .text_right()
                                        .child(format_duration(position_ms)),
                                )
                                .child(
                                    div()
                                        .id("progress-slider")
                                        .test_support()
                                        .h(px(5.))
                                        .w(px(slider_width))
                                        .flex_none()
                                        .rounded(px(3.))
                                        .bg(rgb(palette.surface_raised))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            gpui_kit::MouseButton::Left,
                                            cx.listener(
                                                |this,
                                                 event: &gpui_kit::MouseDownEvent,
                                                 window,
                                                 cx| {
                                                    this.player.update(cx, |player, cx| {
                                                        let Some(duration_ms) = player
                                                            .now_playing()
                                                            .map(|track| track.duration_ms)
                                                        else {
                                                            return;
                                                        };
                                                        let position = seek_for_pointer(
                                                            f32::from(event.position.x),
                                                            f32::from(
                                                                window
                                                                    .window_bounds()
                                                                    .get_bounds()
                                                                    .size
                                                                    .width,
                                                            ),
                                                            this.content_width,
                                                            duration_ms,
                                                        );
                                                        player.seek(position, cx);
                                                    });
                                                },
                                            ),
                                        )
                                        .child(
                                            div()
                                                .w(relative(progress))
                                                .h_full()
                                                .rounded(px(3.))
                                                .bg(rgb(palette.text_primary)),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(PROGRESS_TIME_WIDTH))
                                        .flex_none()
                                        .child(duration),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(if compact {
                        COMPACT_PLAYER_RIGHT_WIDTH
                    } else {
                        PLAYER_RIGHT_WIDTH
                    }))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        components::icon_button_with(
                            palette,
                            "queue-toggle",
                            CadenceIcon::ListMusic,
                            17.,
                        )
                        .test_support()
                        .when(self.queue_open, |button| button.bg(rgb(palette.selection)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let open = !this.queue_open;
                            this.set_queue_open(open, cx);
                            cx.emit(ToggleQueue);
                        })),
                    )
                    .child(
                        components::icon_button_with(palette, "volume", volume_icon, 17.)
                            .test_support()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.player.update(cx, |player, cx| player.toggle_mute(cx));
                            })),
                    )
                    .when(!compact, |controls| {
                        controls.child(
                            div()
                                .id("volume-slider")
                                .w(px(VOLUME_SLIDER_WIDTH))
                                .h(px(24.))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_mouse_down(
                                    gpui_kit::MouseButton::Left,
                                    cx.listener(
                                        |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                                            this.player.update(cx, |player, cx| {
                                                player.begin_volume_drag(
                                                    event.position.x,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        },
                                    ),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .w_full()
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(rgb(palette.surface_raised))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(px(VOLUME_SLIDER_WIDTH * volume))
                                                .rounded(px(2.))
                                                .bg(rgb(palette.text_primary)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .left(px((VOLUME_SLIDER_WIDTH - 12.) * volume))
                                                .top(px(-4.))
                                                .size(px(12.))
                                                .rounded(px(6.))
                                                .bg(rgb(palette.text_primary))
                                                .border_2()
                                                .border_color(rgb(palette.surface)),
                                        ),
                                ),
                        )
                    }),
            )
    }

    /// The heart for whatever is playing: the same control the track rows
    /// carry, so the two read as one action. Inert with nothing playing.
    fn liked_toggle(
        &self,
        palette: CadencePalette,
        track: Option<model::Track>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let liked = track
            .as_ref()
            .is_some_and(|track| self.library.read(cx).is_liked(track));
        components::liked_heart(palette, "player-liked", liked)
            .when(track.is_none(), |button| button.opacity(0.5))
            .when_some(track, |button, track| {
                button
                    .hover(|style| style.bg(rgb(palette.control)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library.update(cx, |library, cx| {
                            library.set_liked(track.clone(), !liked, cx)
                        });
                    }))
            })
    }
}

impl Render for PlayerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.bar(window, cx)
    }
}

/// The three-state shuffle toggle left of the transport cluster: dimmed
/// while off, highlighted on the selection pill while on (shuffle arrows,
/// or the sparkles once Smart Shuffle is weaving recommendations in), and
/// half-faded where toggling would be a no-op.
fn shuffle_toggle(
    palette: CadencePalette,
    mode: ShuffleMode,
    supported: bool,
    smart_supported: bool,
) -> Stateful<Div> {
    let active = mode.shuffles();
    let icon = match mode {
        ShuffleMode::Smart => CadenceIcon::Sparkles,
        _ => CadenceIcon::Shuffle,
    };
    components::button(palette, "shuffle-toggle")
        .size(px(40.))
        .flex_none()
        .rounded(px(20.))
        .when(active, |button| button.bg(rgb(palette.selection)))
        .when(!active && supported, |button| {
            button.hover(|style| style.bg(rgb(palette.control)))
        })
        // Smart stays reachable only where the backend can act on it; the
        // plain shuffle state is always offered on a live context.
        .when(mode == ShuffleMode::Smart && !smart_supported, |button| {
            button.opacity(0.5)
        })
        .child(components::icon(
            icon,
            16.,
            if active {
                palette.text_primary
            } else {
                palette.text_muted
            },
        ))
        .when(!supported, |button| button.opacity(0.5))
}

/// Raised when the listener dismisses the queue panel.
pub(super) struct CloseQueue;

impl EventEmitter<CloseQueue> for QueueDrawer {}

/// The slide-over queue panel.
pub(super) struct QueueDrawer {
    player: Entity<player::Player>,
    image_cache: Entity<image_cache::BoundedImageCache>,
}

impl QueueDrawer {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            player: services::AppServices::player(cx),
            image_cache: services::AppServices::image_cache(cx),
        }
    }

    fn drawer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let player = self.player.read(cx);
        let queue = player.queue().clone();
        let queue_count = queue.len();
        let context_offset = usize::from(player.now_playing().is_some());
        let playback_context = player.context().clone();
        let now_playing = player.now_playing().cloned();
        let now_playing_injected = player.now_playing_injected();

        div()
            .occlude()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(420.))
            .p(px(24.))
            .bg(rgb(palette.surface))
            .border_l_1()
            .border_color(rgb(palette.border))
            .shadow_xl()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb(px(24.))
                    .child(
                        div()
                            .text_size(px(32.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(rgb(palette.text_primary))
                            .child("Queue"),
                    )
                    .child(
                        components::icon_button(palette, "close-queue", CadenceIcon::Close)
                            .test_support()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseQueue))),
                    ),
            )
            .child(components::section_label(palette, "Now playing"))
            .child(
                now_playing
                    .map(|track| {
                        self.row(palette, "queue-current", track, true, now_playing_injected)
                            .into_any_element()
                    })
                    .unwrap_or_else(|| {
                        components::empty_state(palette, "Nothing playing").into_any_element()
                    }),
            )
            .child(div().h(px(24.)))
            .child(components::section_label(palette, "Next"))
            .child(
                div()
                    .id("queue-scroll")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        uniform_list(
                            "queue-tracks",
                            queue_count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| {
                                        let track = queue[index].clone();
                                        let injected =
                                            this.player.read(cx).queue_track_injected(index);
                                        let playback_context = playback_context.clone();
                                        this.row(
                                            palette,
                                            ("queue-track", index),
                                            track,
                                            false,
                                            injected,
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.player.update(cx, |player, cx| {
                                                player.play_context(
                                                    playback_context.to_vec(),
                                                    index + context_offset,
                                                    player.context_kind(),
                                                    // The queue is already
                                                    // playing this context;
                                                    // jumping inside it is
                                                    // not a fresh start.
                                                    None,
                                                    cx,
                                                )
                                            });
                                        }))
                                        .into_any_element()
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
            )
    }

    fn row(
        &self,
        palette: CadencePalette,
        id: impl Into<ElementId>,
        track: model::Track,
        current: bool,
        injected: bool,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .w_full()
            .h(px(if current { 72. } else { 62. }))
            .flex_none()
            .mt(px(8.))
            .px(px(10.))
            .rounded(px(16.))
            .bg(if current {
                rgb(palette.selection)
            } else {
                rgb(palette.surface)
            })
            .when(!current, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(rgb(palette.surface_hover)))
            })
            .flex()
            .items_center()
            .gap(px(12.))
            .child(components::artwork(
                palette,
                &self.image_cache,
                track.artwork_url.as_deref(),
                if current { 48. } else { 40. },
                8.,
                CadenceIcon::Music,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(13.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(rgb(if current {
                                palette.text
                            } else {
                                palette.text_muted
                            }))
                            .child(track.artist.clone()),
                    ),
            )
            .child(if injected {
                // Smart Shuffle injections carry their own mark, so the
                // listener can tell recommendations from context tracks.
                div()
                    .flex_none()
                    .child(components::icon(CadenceIcon::Sparkles, 14., palette.link))
                    .into_any_element()
            } else {
                div().flex_none().into_any_element()
            })
            .child(
                div()
                    .w(px(44.))
                    .flex_none()
                    .text_right()
                    .text_size(px(12.))
                    .text_color(rgb(palette.text_muted))
                    .child(format_duration(track.duration_ms)),
            )
    }
}

impl Render for QueueDrawer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawer(cx)
    }
}
