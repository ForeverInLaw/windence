use super::*;

/// Empty space above the brand row; also the height of the drag strips that
/// surround the Windows traffic lights floating inside it.
const SIDEBAR_TOP_PADDING: f32 = 52.;

/// Navigation the sidebar asks the workspace to perform.
pub(super) enum SidebarEvent {
    Navigate(Route),
    Failed(String),
    OpenPlaylist {
        playlist: model::Playlist,
        origin: Route,
    },
}

/// What clicking a library row does. Every row but one navigates; DJ X is
/// a permanent synthetic entry that opens its playlist page.
#[derive(Clone, Copy)]
enum NavTarget {
    Route(Route),
    DjX,
}

/// The library navigation rail.
pub(super) struct Sidebar {
    library: Entity<library::Library>,
    brand_mark: Arc<gpui::Image>,
    /// The route to highlight, pushed by the workspace when it navigates.
    route: Route,
    /// Where a pinned playlist should return to when the listener backs out.
    pinned_origin: Route,
    /// The playlist page is showing the DJ lineup, so its row highlights.
    dj_open: bool,
    compact_layout: bool,
    collapsed: bool,
    transition_generation: u64,
    visual_width: Rc<Cell<f32>>,
    transition_from: f32,
    transition_duration: Duration,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

/// A row's name, cut with an ellipsis rather than painted through the rail's
/// edge. The row itself has to allow it: a flex child will not shrink below
/// its text without `min_w_0`.
fn row_label(text: impl Into<SharedString>) -> Div {
    div().min_w_0().flex_1().truncate().child(text.into())
}

fn expanded_sidebar_width(compact_layout: bool) -> f32 {
    if compact_layout { 200. } else { 232. }
}

impl Sidebar {
    pub(super) fn new(collapsed: bool, cx: &mut App) -> Self {
        let width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_sidebar_width(false)
        };
        Self {
            library: services::AppServices::library(cx),
            brand_mark: services::AppServices::brand_mark(cx),
            route: Route::LikedSongs,
            pinned_origin: Route::LikedSongs,
            dj_open: false,
            compact_layout: false,
            collapsed,
            transition_generation: 0,
            visual_width: Rc::new(Cell::new(width)),
            transition_from: width,
            transition_duration: Duration::from_millis(1),
        }
    }

    pub(super) fn show_route(
        &mut self,
        route: Route,
        pinned_origin: Route,
        dj_open: bool,
        cx: &mut Context<Self>,
    ) {
        if self.route != route || self.pinned_origin != pinned_origin || self.dj_open != dj_open {
            self.route = route;
            self.pinned_origin = pinned_origin;
            self.dj_open = dj_open;
            cx.notify();
        }
    }

    pub(super) fn set_compact_layout(&mut self, compact: bool, cx: &mut Context<Self>) {
        if self.compact_layout != compact {
            self.compact_layout = compact;
            cx.notify();
        }
    }

    pub(super) fn set_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        if self.collapsed == collapsed {
            return;
        }
        let current_width = self.visual_width.get();
        let expanded_width = expanded_sidebar_width(self.compact_layout);
        let target_width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_width
        };
        self.transition_from = current_width;
        self.transition_duration =
            sidebar_transition_duration(current_width, target_width, expanded_width);
        self.collapsed = collapsed;
        self.transition_generation = self.transition_generation.wrapping_add(1);
        if let Some(Err(error)) = services::AppServices::set_sidebar_collapsed(collapsed, cx) {
            cx.emit(SidebarEvent::Failed(format!(
                "Could not save sidebar preference: {error}"
            )));
        }
        cx.notify();
    }

    /// One row of the pinned section: a playlist that opens its page, or a
    /// folder that opens and closes where it stands, like anywhere else the
    /// library is drawn.
    fn pinned_row(
        &self,
        index: usize,
        row: library_index::LibraryRow,
        palette: CadencePalette,
        origin: Route,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let button = components::button(palette, ("pinned-item", index))
            .h(px(32.))
            .w_full()
            .min_w_0()
            .justify_start()
            .pl(px(2. + row.depth() as f32 * 12.))
            .pr(px(2.))
            .gap(px(8.))
            .overflow_hidden()
            .text_size(px(14.))
            .text_color(rgb(palette.text));
        // Pin order is hand-made, so every row in this section can be
        // picked up and dropped on another to take its place.
        let uri = row.uri();
        let button = components::draggable_pin(
            button,
            palette,
            components::PinDrag::new(
                uri.clone(),
                row.label().to_owned(),
                cx.listener(move |this, dragged: &components::DraggedPin, _, cx| {
                    let dragged = dragged.uri.clone();
                    let target = uri.clone();
                    this.library
                        .update(cx, |library, cx| library.move_pin(&dragged, &target, cx));
                }),
            ),
        );
        match row {
            library_index::LibraryRow::Playlist { playlist, .. } => button
                .child(row_label(playlist.name.clone()))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(SidebarEvent::OpenPlaylist {
                        playlist: playlist.clone(),
                        origin,
                    });
                }))
                .into_any_element(),
            library_index::LibraryRow::Folder {
                uri,
                name,
                expanded,
                ..
            } => button
                .child(components::icon(
                    if expanded { "folder-open" } else { "folder" },
                    15.,
                    palette.text_muted,
                ))
                .child(row_label(name))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let uri = uri.clone();
                    this.library
                        .update(cx, |library, cx| library.toggle_pinned_folder(&uri, cx));
                    cx.notify();
                }))
                .into_any_element(),
        }
    }

    fn panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let route = self.route;
        let collapsed = self.collapsed;
        let pinned_origin = self.pinned_origin;
        let expanded_width = expanded_sidebar_width(self.compact_layout);
        let target_width = if collapsed {
            COLLAPSED_SIDEBAR_WIDTH
        } else {
            expanded_width
        };
        let start_width = self.transition_from;
        let animation_id = self.transition_generation as usize;
        let animation_duration = self.transition_duration;
        let visual_width = self.visual_width.clone();
        let width_range = expanded_width - COLLAPSED_SIDEBAR_WIDTH;
        let start_progress = ((start_width - COLLAPSED_SIDEBAR_WIDTH) / width_range).clamp(0., 1.);
        let row_width = expanded_width - 2. * SIDEBAR_CONTENT_PAD;
        let target_progress = if collapsed { 0. } else { 1. };
        let row_animation = Animation::new(animation_duration).with_easing(ease_out_quint());
        let nav_item = |id: &'static str,
                        fill_id: &'static str,
                        label: &'static str,
                        icon: &'static str,
                        selected_icon: &'static str,
                        target: NavTarget,
                        cx: &mut Context<Self>| {
            let selected = match target {
                NavTarget::Route(target) => {
                    route == target
                        || (target == Route::Playlists && route == Route::Playlist && !self.dj_open)
                }
                // DJ X shares the playlist page with every other playlist;
                // only its own row lights up when it is the one open.
                NavTarget::DjX => self.dj_open && route == Route::Playlist,
            };
            // The pill carries selection and hover, sized to what it visually
            // covers: the icon when collapsed, the whole row when expanded.
            let fill =
                div()
                    .h(px(42.))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .pr(px(NAV_ROW_PAD))
                    .when(selected, |fill| fill.bg(rgb(palette.selection)))
                    .hover(|style| style.bg(rgb(palette.surface_raised)))
                    .child(div().w(px(20.)).flex_none().flex().items_center().child(
                        components::icon(
                            if selected { selected_icon } else { icon },
                            17.,
                            palette.text_primary,
                        ),
                    ))
                    .child(div().whitespace_nowrap().child(label).with_animation(
                        (id, animation_id),
                        row_animation.clone(),
                        move |label, delta| {
                            label.opacity(
                                start_progress + (target_progress - start_progress) * delta,
                            )
                        },
                    ))
                    .with_animation(
                        (fill_id, animation_id),
                        row_animation.clone(),
                        move |fill, delta| {
                            let progress =
                                start_progress + (target_progress - start_progress) * delta;
                            let (width, left, pad) = sidebar_fill_geometry(
                                NAV_ROW_PAD,
                                NAV_GLYPH_WIDTH,
                                row_width,
                                progress,
                            );
                            fill.w(px(width)).ml(px(left)).pl(px(pad))
                        },
                    );
            components::button(palette, id)
                .w_full()
                .h(px(42.))
                .justify_start()
                .text_color(rgb(if selected {
                    palette.text_primary
                } else {
                    palette.text
                }))
                .text_size(px(14.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(fill)
                .on_click(cx.listener(move |_, _, _, cx| match target {
                    NavTarget::Route(target) => cx.emit(SidebarEvent::Navigate(target)),
                    NavTarget::DjX => cx.emit(SidebarEvent::OpenPlaylist {
                        playlist: dj::playlist(),
                        origin: pinned_origin,
                    }),
                }))
        };
        let mut pinned_section = div().flex().flex_col().gap(px(4.)).px(px(10.)).child(
            div()
                .px(px(2.))
                .pb(px(4.))
                .child(components::section_label(palette, "Pinned")),
        );
        let pinned_rows = self.library.read(cx).pinned_rows().clone();
        for (index, row) in pinned_rows.iter().cloned().enumerate() {
            pinned_section =
                pinned_section.child(self.pinned_row(index, row, palette, pinned_origin, cx));
        }
        let show_pinned = !pinned_rows.is_empty();
        let brand_fill = div()
            .h(px(48.))
            .rounded(px(12.))
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(16.5))
            .pr(px(BRAND_ROW_PAD))
            .hover(|style| style.bg(rgb(palette.control)))
            .child(
                img(self.brand_mark.clone())
                    .size(px(BRAND_LOGO_SIZE))
                    .flex_none(),
            )
            .child(div().whitespace_nowrap().child("Cadence").with_animation(
                ("sidebar-brand-label", animation_id),
                row_animation.clone(),
                move |label, delta| {
                    label.opacity(start_progress + (target_progress - start_progress) * delta)
                },
            ))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(17.))
                    .h(px(48.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(components::icon("chevron-left", 17., palette.text_primary))
                    .with_animation(
                        ("sidebar-chevron", animation_id),
                        row_animation.clone(),
                        move |button, delta| {
                            button.opacity(
                                start_progress + (target_progress - start_progress) * delta,
                            )
                        },
                    ),
            )
            .with_animation(
                ("sidebar-brand-fill", animation_id),
                row_animation.clone(),
                move |fill, delta| {
                    let progress = start_progress + (target_progress - start_progress) * delta;
                    let (width, left, pad) =
                        sidebar_fill_geometry(BRAND_ROW_PAD, BRAND_LOGO_SIZE, row_width, progress);
                    fill.w(px(width)).ml(px(left)).pl(px(pad))
                },
            );
        let brand = components::button(palette, "sidebar-toggle")
            .h(px(48.))
            .w_full()
            .flex_none()
            .justify_start()
            .items_center()
            .text_color(rgb(palette.text_primary))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(brand_fill)
            .on_click(cx.listener(|this, _, _, cx| {
                let collapsed = !this.collapsed;
                this.set_collapsed(collapsed, cx);
            }));

        div()
            .w(px(target_width))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .bg(rgb(palette.canvas))
            .border_r_1()
            .border_color(rgb(palette.border))
            .relative()
            .when(cfg!(target_os = "windows"), |sidebar| {
                // Empty padding above the brand row doubles as the drag strip
                // for the custom traffic lights, which float over it from the
                // window root. The strip stops short of the cluster: GPUI
                // resolves overlapping control areas by paint order, and this
                // panel paints before that overlay, so a strip touching the
                // dots would turn their clicks into window drags. Both rects
                // anchor on the panel edges and follow the animated width;
                // below the dot row the full width drags again.
                sidebar
                    .child(window_drag_strip(
                        0.,
                        TRAFFIC_LIGHT_BAND_RIGHT,
                        SIDEBAR_TOP_PADDING,
                    ))
                    .child(window_drag_strip(
                        TRAFFIC_LIGHT_BAND_BOTTOM,
                        0.,
                        SIDEBAR_TOP_PADDING - TRAFFIC_LIGHT_BAND_BOTTOM,
                    ))
            })
            .child(
                div()
                    .w(px(expanded_width))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .p(px(SIDEBAR_CONTENT_PAD))
                    .pt(px(SIDEBAR_TOP_PADDING))
                    .child(brand)
                    .child(
                        // Everything below the brand scrolls together: a long
                        // pinned section used to run off the bottom of the
                        // rail with no way to reach the rest of it.
                        div()
                            .id("sidebar-sections")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(28.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    // Home is Spotify's page, not a library
                                    // item, so it sits above the label.
                                    .child(nav_item(
                                        "nav-home",
                                        "nav-home-fill",
                                        "Home",
                                        "house",
                                        "house",
                                        NavTarget::Route(Route::Home),
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .px(px(12.))
                                            .pt(px(12.))
                                            .pb(px(4.))
                                            .child(components::section_label(palette, "Library"))
                                            .with_animation(
                                                ("sidebar-library-label", animation_id),
                                                row_animation.clone(),
                                                move |label, delta| {
                                                    label.opacity(
                                                        start_progress
                                                            + (target_progress - start_progress)
                                                                * delta,
                                                    )
                                                },
                                            ),
                                    )
                                    .child(nav_item(
                                        "nav-library",
                                        "nav-library-fill",
                                        "Liked Songs",
                                        "heart",
                                        "heart-fill",
                                        NavTarget::Route(Route::LikedSongs),
                                        cx,
                                    ))
                                    .child(nav_item(
                                        "nav-playlist",
                                        "nav-playlist-fill",
                                        "Playlists",
                                        "list-music",
                                        "list-music",
                                        NavTarget::Route(Route::Playlists),
                                        cx,
                                    ))
                                    .child(nav_item(
                                        "nav-recent",
                                        "nav-recent-fill",
                                        "Recently played",
                                        "clock",
                                        "clock",
                                        NavTarget::Route(Route::Recent),
                                        cx,
                                    ))
                                    .child(nav_item(
                                        "nav-dj",
                                        "nav-dj-fill",
                                        dj::DISPLAY_NAME,
                                        "bot",
                                        "bot",
                                        NavTarget::DjX,
                                        cx,
                                    )),
                            )
                            .when(show_pinned && !collapsed, |sections| {
                                sections.child(div().child(pinned_section).with_animation(
                                    ("sidebar-pinned", animation_id),
                                    row_animation.clone(),
                                    move |pinned, delta| {
                                        pinned
                                            .opacity(start_progress + (1. - start_progress) * delta)
                                    },
                                ))
                            })
                            .child(div().flex_none().h(px(SIDEBAR_CONTENT_PAD))),
                    ),
            )
            .with_animation(
                ("sidebar-width", animation_id),
                Animation::new(animation_duration).with_easing(ease_out_quint()),
                move |sidebar, delta| {
                    let width = interpolate_sidebar_width(start_width, target_width, delta);
                    visual_width.set(width);
                    sidebar.w(px(width))
                },
            )
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.panel(cx)
    }
}
