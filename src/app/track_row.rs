use super::*;

use gpui::ClickEvent;

/// Breathing room between the Title column and whatever follows it.
const COLUMN_GUTTER: f32 = 16.;
/// The fixed columns; Title and Album flex to share whatever remains.
const INDEX_COLUMN_WIDTH: f32 = 44.;
const HEART_COLUMN_WIDTH: f32 = components::LIKED_HEART_SIZE;
const TIME_COLUMN_WIDTH: f32 = 60.;
const ACTIONS_COLUMN_WIDTH: f32 = 36.;
/// Wide enough for "Sep 28, 2026"; never squeezed below its content.
const DATE_ADDED_COLUMN_WIDTH: f32 = 110.;
/// The corner radius a track list is cut to. The header carries it on its
/// own top corners as well: clipping is rectangular, so a square header
/// would paint into the rounded corners it sits inside.
pub(super) const LIST_CORNER_RADIUS: f32 = 20.;
/// What a track list is built from, top to bottom: its frame's border,
/// the column header, and one row per track.
const LIST_BORDER_WIDTH: f32 = 1.;
const HEADER_HEIGHT: f32 = 40.;
const ROW_HEIGHT: f32 = 64.;

/// How tall a track list is with `rows` rows in it. A list caps its height
/// here, so a short one ends where its last row does instead of drawing
/// its bottom border under a stretch of empty space.
pub(super) fn list_height(rows: usize) -> f32 {
    2. * LIST_BORDER_WIDTH + HEADER_HEIGHT + rows as f32 * ROW_HEIGHT
}

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
        .h(px(HEADER_HEIGHT))
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
        // The heart has no header: it belongs to the row under the pointer,
        // not to a column of its own. The width is still held, so the
        // columns beside it line up with the rows below.
        .child(div().w(px(HEART_COLUMN_WIDTH)).flex_none())
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
/// Stateless: the list that shows it decides whether it is current, liked
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
    liked: bool,
    menu_open: bool,
    /// Rendered beside the actions button while the menu is open.
    menu: Option<AnyElement>,
    on_play: Option<RowCallback>,
    on_liked: Option<RowCallback>,
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
            liked: false,
            menu_open: false,
            menu: None,
            on_play: None,
            on_liked: None,
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

    pub(super) fn liked(mut self, liked: bool) -> Self {
        self.liked = liked;
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

    pub(super) fn on_liked(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_liked = Some(Box::new(handler));
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
            .h(px(ROW_HEIGHT))
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
                components::liked_heart(palette, ("spotify-liked", index), self.liked)
                    // Reachable on the row under the pointer and nowhere
                    // else: the column keeps its width so the ones beside
                    // it do not shift, but stays empty until then.
                    .invisible()
                    .group_hover(row_group.clone(), |style| style.visible())
                    .hover(|style| style.bg(rgb(palette.control)))
                    .when_some(self.on_liked, |button, handler| {
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

/// How far one folder level indents the rows inside it.
const FOLDER_INDENT: f32 = 22.;

/// The left padding a row at `depth` starts its contents at.
fn row_indent(depth: usize) -> f32 {
    12. + depth as f32 * FOLDER_INDENT
}

/// The shell both playlist-list rows share: one tall clickable band, ruled
/// off from the row above and indented by the folders it sits inside.
fn library_row(
    palette: CadencePalette,
    id: impl Into<ElementId>,
    depth: usize,
    first: bool,
) -> Stateful<Div> {
    components::button(palette, id)
        .w_full()
        .h(px(76.))
        .pr(px(12.))
        .pl(px(row_indent(depth)))
        .justify_start()
        .gap(px(14.))
        .rounded(px(0.))
        // The top row sits in the frame's rounded corners, and clipping is
        // rectangular: without the radius its hover fill paints into them.
        // It also needs no rule above it — the frame's own border is there.
        .when(first, |row| row.rounded_t(px(LIST_CORNER_RADIUS)))
        .when(!first, |row| {
            row.border_t_1().border_color(rgb(palette.border))
        })
        .hover(|style| style.bg(rgb(palette.surface_hover)))
}

/// The mark a row carries when the account has it pinned, so the list says
/// what the sidebar's own section already shows.
fn pin_marker(palette: CadencePalette, pinned: bool) -> Option<Div> {
    pinned.then(|| {
        div()
            .flex_none()
            .child(components::icon("pin-fill", 15., palette.text_muted))
    })
}

/// One playlist in a list. Stateless, like `TrackRow`.
#[derive(IntoElement)]
pub(super) struct PlaylistRow {
    index: usize,
    playlist: model::Playlist,
    /// How many folders the row sits inside, which is what indents it.
    depth: usize,
    palette: CadencePalette,
    image_cache: Entity<image_cache::BoundedImageCache>,
    on_open: Option<RowCallback>,
    /// Set when the row is a pin, which is what makes it draggable.
    drag: Option<components::PinDrag>,
}

impl PlaylistRow {
    pub(super) fn new(
        index: usize,
        playlist: model::Playlist,
        depth: usize,
        palette: CadencePalette,
        image_cache: Entity<image_cache::BoundedImageCache>,
    ) -> Self {
        Self {
            index,
            playlist,
            depth,
            palette,
            image_cache,
            on_open: None,
            drag: None,
        }
    }

    pub(super) fn on_open(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Box::new(handler));
        self
    }

    /// Lets the row be dragged into a new place in the pinned section, and
    /// be the place another pin is dropped on.
    pub(super) fn draggable(mut self, drag: components::PinDrag) -> Self {
        self.drag = Some(drag);
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
        // A pin is the only thing that makes a row draggable, so it is also
        // what says the row is pinned.
        let pinned = self.drag.is_some();
        library_row(
            palette,
            ("spotify-playlist", self.index),
            self.depth,
            self.index == 0,
        )
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
        .child(div().flex_1())
        .children(pin_marker(palette, pinned))
        .when_some(self.on_open, |row, handler| row.on_click(handler))
        .when_some(self.drag, |row, drag| {
            components::draggable_pin(row, palette, drag)
        })
    }
}

/// A folder in the playlist list. Clicking it opens or closes it where it
/// stands, rather than navigating anywhere.
#[derive(IntoElement)]
pub(super) struct FolderRow {
    index: usize,
    name: String,
    /// How many entries the folder holds directly.
    children: usize,
    depth: usize,
    expanded: bool,
    palette: CadencePalette,
    on_toggle: Option<RowCallback>,
    /// Set when the folder is a pin, which is what makes it draggable.
    drag: Option<components::PinDrag>,
}

impl FolderRow {
    pub(super) fn new(
        index: usize,
        name: String,
        children: usize,
        depth: usize,
        expanded: bool,
        palette: CadencePalette,
    ) -> Self {
        Self {
            index,
            name,
            children,
            depth,
            expanded,
            palette,
            on_toggle: None,
            drag: None,
        }
    }

    pub(super) fn on_toggle(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }

    /// Lets the row be dragged into a new place in the pinned section, and
    /// be the place another pin is dropped on.
    pub(super) fn draggable(mut self, drag: components::PinDrag) -> Self {
        self.drag = Some(drag);
        self
    }
}

impl RenderOnce for FolderRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let palette = self.palette;
        let detail = match self.children {
            1 => "1 item".to_owned(),
            children => format!("{children} items"),
        };
        let pinned = self.drag.is_some();
        library_row(
            palette,
            ("playlist-folder", self.index),
            self.depth,
            self.index == 0,
        )
        .child(
            div()
                .size(px(48.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .bg(rgb(palette.control))
                .child(components::icon(
                    if self.expanded {
                        "folder-open"
                    } else {
                        "folder"
                    },
                    22.,
                    palette.text_muted,
                )),
        )
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
                        .child(self.name),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(palette.text_muted))
                        .child(detail),
                ),
        )
        .child(div().flex_1())
        .children(pin_marker(palette, pinned))
        .child(components::icon(
            if self.expanded {
                "chevron-down"
            } else {
                "chevron-right"
            },
            17.,
            palette.text_muted,
        ))
        .when_some(self.on_toggle, |row, handler| row.on_click(handler))
        .when_some(self.drag, |row, drag| {
            components::draggable_pin(row, palette, drag)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{format_added_at, list_height, row_indent};
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

    #[test]
    fn list_height_covers_the_header_the_rows_and_the_frame() {
        assert_eq!(list_height(0), 42.);
        assert_eq!(list_height(3), 234.);
    }

    #[test]
    fn every_folder_level_indents_a_row_one_step_further() {
        assert_eq!(row_indent(0), 12.);
        assert_eq!(row_indent(1), 34.);
        assert_eq!(row_indent(2), 56.);
    }
}
