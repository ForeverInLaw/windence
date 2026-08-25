use super::*;

use gpui::ClickEvent;

/// Breathing room between the Title column and whatever follows it.
const COLUMN_GUTTER: f32 = 16.;
/// The fixed columns; Title and Album flex to share whatever remains.
const INDEX_COLUMN_WIDTH: f32 = 44.;
const STAR_COLUMN_WIDTH: f32 = 36.;
const TIME_COLUMN_WIDTH: f32 = 60.;
const ACTIONS_COLUMN_WIDTH: f32 = 36.;
/// Wide enough for "Sep 28, 2026"; never squeezed below its content.
const DATE_ADDED_COLUMN_WIDTH: f32 = 110.;
/// The corner radius a track list is cut to. The header carries it on its
/// own top corners as well: clipping is rectangular, so a square header
/// would paint into the rounded corners it sits inside.
pub(super) const LIST_CORNER_RADIUS: f32 = 20.;

/// What a header click does. One handler per sortable column; a missing
/// handler renders the label inert, as lists without sorting do.
#[derive(Default)]
pub(super) struct TrackHeaderActions {
    pub(super) title: Option<RowCallback>,
    pub(super) album: Option<RowCallback>,
    pub(super) date_added: Option<RowCallback>,
}

/// The column header for a track list. Lives beside `TrackRow` so the fixed
/// columns cannot drift out of step with the rows they label. `active`
/// marks which column is sorted and which way; those headers draw an arrow.
pub(super) fn track_list_header(
    palette: CadencePalette,
    columns: TrackTableColumns,
    active: Option<model::ListSort>,
    actions: TrackHeaderActions,
) -> Div {
    let arrow = |direction| {
        components::icon(
            match direction {
                model::ListSortDirection::Ascending => "arrow-up",
                model::ListSortDirection::Descending => "arrow-down",
            },
            11.,
            palette.text_primary,
        )
    };
    let sortable = |id: &'static str,
                    label: &'static str,
                    sort: Option<model::ListSortDirection>,
                    handler: Option<RowCallback>| {
        let button = components::button(palette, id)
            .h(px(22.))
            .px(px(6.))
            .mx(px(-6.))
            .rounded(px(6.))
            .gap(px(3.))
            .justify_start()
            .text_color(rgb(palette.text_muted))
            .child(label);
        let button = if let Some(direction) = sort {
            button.child(arrow(direction))
        } else {
            button
        };
        match handler {
            Some(handler) => {
                let route = handler;
                // Hover lifts the label's color a step toward full contrast,
                // rather than painting the cell: the header stays quiet.
                button
                    .hover(move |style| style.text_color(rgb(palette.text)))
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        route(event, window, cx);
                    })
            }
            None => button.cursor_default(),
        }
    };
    let direction_for = |column: model::ListSortColumn| {
        active
            .filter(|sort| sort.column == column)
            .map(|sort| sort.direction)
    };
    div()
        .h(px(40.))
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .rounded_t(px(LIST_CORNER_RADIUS))
        .bg(rgb(palette.canvas))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text_muted))
        .child(div().w(px(INDEX_COLUMN_WIDTH)).flex_none().child("#"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr(px(COLUMN_GUTTER))
                .child(sortable(
                    "sort-title",
                    "Title",
                    direction_for(model::ListSortColumn::Title),
                    actions.title,
                )),
        )
        .when(columns.album, |header| {
            header.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(px(COLUMN_GUTTER))
                    .child(sortable(
                        "sort-album",
                        "Album",
                        direction_for(model::ListSortColumn::Album),
                        actions.album,
                    )),
            )
        })
        .when(columns.date_added, |header| {
            header.child(
                div()
                    .w(px(DATE_ADDED_COLUMN_WIDTH))
                    .flex_none()
                    .child(sortable(
                        "sort-date-added",
                        "Date added",
                        direction_for(model::ListSortColumn::DateAdded),
                        actions.date_added,
                    )),
            )
        })
        .child(
            div()
                .w(px(STAR_COLUMN_WIDTH))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(components::icon("star", 12., palette.text_muted)),
        )
        .child(
            div()
                .w(px(TIME_COLUMN_WIDTH))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .pr(px(8.))
                .child("Time"),
        )
        .child(div().w(px(ACTIONS_COLUMN_WIDTH)).flex_none())
}

/// How long ago a track entered its listing, Spotify-style: relative for
/// the first month ("3 weeks ago"), an absolute date after that.
pub(super) fn format_added_at(
    now: chrono::DateTime<chrono::Utc>,
    added_at: chrono::DateTime<chrono::Utc>,
) -> String {
    let elapsed = now.signed_duration_since(added_at);
    let ago =
        |count: i64, unit: &str| format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" });
    if elapsed.num_hours() < 1 {
        "Just now".to_owned()
    } else if elapsed.num_days() < 1 {
        ago(elapsed.num_hours(), "hour")
    } else if elapsed.num_days() < 7 {
        ago(elapsed.num_days(), "day")
    } else if elapsed.num_days() <= 28 {
        ago(elapsed.num_days() / 7, "week")
    } else {
        added_at.format("%b %-d, %Y").to_string()
    }
}

/// A single line that ellipsizes at the column edge. Wrapping text with a
/// one-line clamp rather than `.truncate()`: gpui 0.2's text-measure cache
/// never recomputes truncation for nowrap text first measured at indefinite
/// width (as happens inside nested flex), while a wrap-width change between
/// measure passes forces the recompute.
fn ellipsized_line(text_size: f32) -> Div {
    div().text_ellipsis().line_clamp(1).text_size(px(text_size))
}

pub(super) type RowCallback = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One track in a list.
///
/// Stateless: the list that shows it decides whether it is current, favorited
/// or has its menu open, and supplies the behaviour as callbacks. The row never
/// reaches for the player or the library itself.
#[derive(IntoElement)]
pub(super) struct TrackRow {
    index: usize,
    /// The track's position in its context's default order, which the `#`
    /// column keeps showing however the view is sorted.
    default_position: usize,
    track: model::Track,
    palette: CadencePalette,
    image_cache: Entity<image_cache::BoundedImageCache>,
    columns: TrackTableColumns,
    /// The preformatted "3 weeks ago" label, when the context dates tracks.
    added_label: Option<SharedString>,
    current: bool,
    favorite: bool,
    menu_open: bool,
    /// Rendered beside the actions button while the menu is open.
    menu: Option<AnyElement>,
    on_play: Option<RowCallback>,
    on_favorite: Option<RowCallback>,
    on_toggle_menu: Option<RowCallback>,
}

impl TrackRow {
    pub(super) fn new(
        index: usize,
        default_position: usize,
        track: model::Track,
        palette: CadencePalette,
        image_cache: Entity<image_cache::BoundedImageCache>,
        columns: TrackTableColumns,
    ) -> Self {
        Self {
            index,
            default_position,
            track,
            palette,
            image_cache,
            columns,
            added_label: None,
            current: false,
            favorite: false,
            menu_open: false,
            menu: None,
            on_play: None,
            on_favorite: None,
            on_toggle_menu: None,
        }
    }

    pub(super) fn added_label(mut self, added_label: impl Into<SharedString>) -> Self {
        self.added_label = Some(added_label.into());
        self
    }

    /// Marks the row as the track currently playing, which also stops it
    /// responding to clicks.
    pub(super) fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    pub(super) fn favorite(mut self, favorite: bool) -> Self {
        self.favorite = favorite;
        self
    }

    pub(super) fn menu(mut self, open: bool, menu: Option<AnyElement>) -> Self {
        self.menu_open = open;
        self.menu = menu;
        self
    }

    pub(super) fn on_play(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_play = Some(Box::new(handler));
        self
    }

    pub(super) fn on_favorite(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_favorite = Some(Box::new(handler));
        self
    }

    pub(super) fn on_toggle_menu(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_menu = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TrackRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let index = self.index;
        let row_group: SharedString =
            format!("spotify-track-row:{}:{index}", self.track.source_id).into();
        components::button(palette, ("spotify-track", index))
            .group(row_group.clone())
            .w_full()
            .h(px(64.))
            .px(px(12.))
            .rounded(px(0.))
            .justify_start()
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if self.current {
                palette.selection
            } else {
                palette.surface
            }))
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(
                div()
                    .w(px(INDEX_COLUMN_WIDTH))
                    .flex_none()
                    .text_size(px(13.))
                    .text_color(rgb(palette.text_muted))
                    .child(self.default_position.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr(px(COLUMN_GUTTER))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(components::artwork(
                        palette,
                        &self.image_cache,
                        self.track.artwork_url.as_deref(),
                        40.,
                        8.,
                        "music",
                    ))
                    .child(
                        // Cross-axis stretch (the default) hands each line a
                        // definite width, which text layout needs to ellipsize
                        // instead of clipping mid-glyph.
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                ellipsized_line(13.)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text_primary))
                                    .child(self.track.title.clone()),
                            )
                            .child(
                                ellipsized_line(12.)
                                    .text_color(rgb(palette.text_muted))
                                    .child(self.track.artist.clone()),
                            ),
                    ),
            )
            .when(self.columns.album, |row| {
                row.child(
                    div().flex_1().min_w_0().flex().flex_col().child(
                        ellipsized_line(13.)
                            .text_color(rgb(palette.text))
                            .child(self.track.album.clone()),
                    ),
                )
            })
            .when(self.columns.date_added, |row| {
                row.child({
                    let date = div()
                        .w(px(DATE_ADDED_COLUMN_WIDTH))
                        .flex_none()
                        .text_size(px(13.))
                        .text_color(rgb(palette.text_muted));
                    match self.added_label.clone() {
                        Some(label) => date.child(label),
                        None => date,
                    }
                })
            })
            .child(
                components::button(palette, ("spotify-favorite", index))
                    .size(px(STAR_COLUMN_WIDTH))
                    .flex_none()
                    .rounded(px(18.))
                    .hover(|style| style.bg(rgb(palette.control)))
                    .child(components::icon(
                        if self.favorite { "star-fill" } else { "star" },
                        15.,
                        if self.favorite {
                            palette.text_primary
                        } else {
                            palette.text
                        },
                    ))
                    .when_some(self.on_favorite, |button, handler| {
                        button.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            handler(event, window, cx);
                        })
                    }),
            )
            .child(
                div()
                    .w(px(TIME_COLUMN_WIDTH))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr(px(8.))
                    .text_size(px(13.))
                    .text_color(rgb(palette.text_muted))
                    .child(format_duration(self.track.duration_ms)),
            )
            .child(
                div()
                    .relative()
                    .size(px(ACTIONS_COLUMN_WIDTH))
                    .flex_none()
                    .child(
                        components::button(palette, ("track-actions", index))
                            .size(px(ACTIONS_COLUMN_WIDTH))
                            .rounded(px(18.))
                            .hover(|style| style.bg(rgb(palette.control)))
                            .active(|style| style.bg(rgb(palette.control_hover)))
                            .when(self.menu_open, |button| button.bg(rgb(palette.control)))
                            .when(!self.menu_open, |button| {
                                button
                                    .invisible()
                                    .group_hover(row_group, |style| style.visible())
                            })
                            .child(components::icon("ellipsis", 17., palette.text_primary))
                            .when_some(self.on_toggle_menu, |button, handler| {
                                button.on_click(move |event, window, cx| {
                                    cx.stop_propagation();
                                    handler(event, window, cx);
                                })
                            }),
                    )
                    .when_some(self.menu, |anchor, menu| {
                        anchor.child(deferred(
                            anchored()
                                .offset(point(px(ACTIONS_COLUMN_WIDTH), px(4.)))
                                .anchor(Anchor::TopRight)
                                .snap_to_window_with_margin(px(8.))
                                .child(menu),
                        ))
                    }),
            )
            .when_some(self.on_play.filter(|_| !self.current), |row, handler| {
                row.on_click(handler)
            })
    }
}

/// One playlist in a list. Stateless, like `TrackRow`.
#[derive(IntoElement)]
pub(super) struct PlaylistRow {
    index: usize,
    playlist: model::Playlist,
    palette: CadencePalette,
    image_cache: Entity<image_cache::BoundedImageCache>,
    on_open: Option<RowCallback>,
}

impl PlaylistRow {
    pub(super) fn new(
        index: usize,
        playlist: model::Playlist,
        palette: CadencePalette,
        image_cache: Entity<image_cache::BoundedImageCache>,
    ) -> Self {
        Self {
            index,
            playlist,
            palette,
            image_cache,
            on_open: None,
        }
    }

    pub(super) fn on_open(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for PlaylistRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let detail = format!(
            "{} tracks · {}",
            self.playlist.track_count, self.playlist.owner
        );
        components::button(palette, ("spotify-playlist", self.index))
            .w_full()
            .h(px(76.))
            .px(px(12.))
            .justify_start()
            .gap(px(14.))
            .rounded(px(0.))
            .border_t_1()
            .border_color(rgb(palette.border))
            .hover(|style| style.bg(rgb(palette.surface_hover)))
            .child(components::artwork(
                palette,
                &self.image_cache,
                self.playlist.artwork_url.as_deref(),
                48.,
                10.,
                "list-music",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text_primary))
                            .child(self.playlist.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(palette.text_muted))
                            .child(detail),
                    ),
            )
            .when_some(self.on_open, |row, handler| row.on_click(handler))
    }
}

#[cfg(test)]
mod tests {
    use super::format_added_at;
    use chrono::{TimeZone, Utc};

    fn at(seconds: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).unwrap()
    }

    #[test]
    fn fresh_additions_show_hours_then_days_then_weeks() {
        let now = at(10_000_000);
        assert_eq!(format_added_at(now, now), "Just now");
        assert_eq!(
            format_added_at(now, now - chrono::Duration::hours(3)),
            "3 hours ago"
        );
        assert_eq!(
            format_added_at(now, now - chrono::Duration::hours(1)),
            "1 hour ago"
        );
        assert_eq!(
            format_added_at(now, now - chrono::Duration::days(2)),
            "2 days ago"
        );
        assert_eq!(
            format_added_at(now, now - chrono::Duration::days(7)),
            "1 week ago"
        );
        assert_eq!(
            format_added_at(now, now - chrono::Duration::days(28)),
            "4 weeks ago"
        );
    }

    #[test]
    fn older_additions_show_an_absolute_date() {
        let now = Utc.with_ymd_and_hms(2026, 8, 23, 12, 0, 0).unwrap();
        let added = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        assert_eq!(format_added_at(now, added), "Jul 18, 2026");
    }
}
