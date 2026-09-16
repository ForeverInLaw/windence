use std::{
    cell::Cell,
    collections::HashSet,
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui_kit::component::{
    Root, Sizable, Theme, WindowExt,
    avatar::Avatar,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    switch::Switch,
    theme::ThemeMode,
};
use gpui_kit::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Bounds, ClipboardItem, Context, Div,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding, Pixels,
    RenderOnce, SharedString, Stateful, Subscription, Window, WindowAppearance, WindowBounds,
    WindowControlArea, WindowOptions, actions, anchored, deferred, div, img, point, prelude::*, px,
    relative, rgb, size, uniform_list,
};
use spotify_gpui_client::{
    backend::{
        Backend, BackendCommand, BackendEvent, BackendHandle, LibraryReload, PlaylistContents,
        Reply,
    },
    dj, library_index,
    lifecycle::{Instance, InstanceLifecycle},
    model,
    pins::Pins,
    shuffle::{ContextKind, ShuffleMode},
    spotify::{self, ClientIdSource, valid_client_id},
    storage::{AppPreferences, Store, ThemePreference},
};

use library_pages::LibrarySection;
use workspace::Workspace;

mod http;
mod icons;
mod image_cache;
mod motion;

use motion::*;

#[cfg(test)]
pub(crate) mod test_support;

actions!(
    cadence,
    [
        Tab,
        TabPrev,
        OpenSearch,
        TogglePlayback,
        SeekBack,
        SeekForward,
        SeekStart,
        SeekEnd,
        VolumeUp,
        VolumeDown,
        VolumeMute,
        Quit,
        CloseWindow,
        DismissOverlay,
        NoOp
    ]
);

#[derive(Clone, Copy)]
struct CadencePalette {
    canvas: u32,
    surface: u32,
    surface_raised: u32,
    surface_hover: u32,
    control: u32,
    control_hover: u32,
    selection: u32,
    text_primary: u32,
    text: u32,
    text_muted: u32,
    border: u32,
    focus_ring: u32,
    danger: u32,
    destructive: u32,
    on_destructive: u32,
    scrim: gpui_kit::Hsla,
    link: u32,
    accent_hover: u32,
    on_accent: u32,
    media_border: gpui_kit::Hsla,
}

impl CadencePalette {
    const LIGHT: Self = Self {
        canvas: 0xFBFAF9,
        surface: 0xFFFFFF,
        surface_raised: 0xF2F0ED,
        surface_hover: 0xF8F7F4,
        control: 0xF6F4EF,
        control_hover: 0xEAE6DD,
        selection: 0xB3CFEC,
        text_primary: 0x171717,
        text: 0x494440,
        text_muted: 0x757373,
        border: 0x8E8E8E,
        focus_ring: 0x6E6C6B,
        danger: 0xEF4444,
        destructive: 0xB42318,
        on_destructive: 0xFFFFFF,
        scrim: gpui_kit::Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 0.32,
        },
        link: 0x0066CC,
        accent_hover: 0x121212,
        on_accent: 0xFFFFFF,
        media_border: gpui_kit::Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 0.1,
        },
    };

    const DARK: Self = Self {
        canvas: 0x121212,
        surface: 0x1A1A1A,
        surface_raised: 0x292929,
        surface_hover: 0x242424,
        control: 0x303030,
        control_hover: 0x404040,
        selection: 0x183B56,
        text_primary: 0xF5F3EF,
        text: 0xD5D1CB,
        text_muted: 0xA09D99,
        border: 0x747474,
        focus_ring: 0xA8A5A1,
        danger: 0xF87171,
        destructive: 0xFF6961,
        on_destructive: 0x171717,
        scrim: gpui_kit::Hsla {
            h: 0.,
            s: 0.,
            l: 0.,
            a: 0.56,
        },
        link: 0x2997FF,
        accent_hover: 0xFFFFFF,
        on_accent: 0x171717,
        media_border: gpui_kit::Hsla {
            h: 0.,
            s: 0.,
            l: 1.,
            a: 0.12,
        },
    };
}

fn is_dark_appearance(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

fn resolve_dark_mode(preference: ThemePreference, appearance: WindowAppearance) -> bool {
    match preference {
        ThemePreference::System => is_dark_appearance(appearance),
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
    }
}
/// The drawn volume slider's width, shared by the two tiers.
pub(super) const VOLUME_SLIDER_WIDTH: f32 = 120.;
const VOLUME_SLIDER_RIGHT_INSET: f32 = 144.;
const PLAYER_LEFT_WIDTH: f32 = 360.;
const PLAYER_CENTER_WIDTH: f32 = 440.;
const PLAYER_RIGHT_WIDTH: f32 = 240.;
/// The full-tier progress slider's width: what the centre box reserves
/// for it, so the drawn track and the seek math agree.
pub(super) const PROGRESS_SLIDER_WIDTH: f32 = 340.;
const PROGRESS_TIME_WIDTH: f32 = 36.;
const PROGRESS_GAP: f32 = 8.;
/// The compact-content breakpoint, against the content width: the window
/// breakpoint (960) minus the full rail (232), so a window lands in the
/// same tier it did when the tiers were keyed on the window itself.
const COMPACT_BREAKPOINT: f32 = 960. - 232.;
/// The compact-player breakpoint against the content width, converted the
/// same way from the window breakpoint (1136).
const COMPACT_PLAYER_BREAKPOINT: f32 = 1136. - 232.;
/// Track-table breakpoints, against the content width. Below the first the
/// date-added column folds away; below the second the album column follows,
/// leaving `#`, title, and time. The heart and row actions survive every
/// width.
const TRACK_DATE_ADDED_BREAKPOINT: f32 = 1100.;
const TRACK_ALBUM_BREAKPOINT: f32 = 880.;

/// Which optional columns of a track table fit the content's width.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TrackTableColumns {
    album: bool,
    date_added: bool,
}

fn track_table_columns(content_width: f32) -> TrackTableColumns {
    TrackTableColumns {
        album: content_width >= TRACK_ALBUM_BREAKPOINT,
        date_added: content_width >= TRACK_DATE_ADDED_BREAKPOINT,
    }
}
/// The collapsed rail; the traffic-light cluster is positioned so its centre
/// sits on this rail's axis.
const COLLAPSED_SIDEBAR_WIDTH: f32 = 78.;
/// The sidebar container's padding; the top is overridden to clear the
/// traffic lights.
const SIDEBAR_CONTENT_PAD: f32 = 16.;
/// The brand row's expanded leading padding and its logo size.
const BRAND_ROW_PAD: f32 = 14.;
const BRAND_LOGO_SIZE: f32 = 32.;
/// A nav row's expanded leading padding, and the width of its glyph: the
/// glyph itself, not the 20pt box that holds it, which is the trap here.
const NAV_ROW_PAD: f32 = 12.;
const NAV_GLYPH_WIDTH: f32 = 17.;
/// One traffic light's edge length.
const TRAFFIC_LIGHT_SIZE: f32 = 12.;
/// Spacing between the traffic lights.
const TRAFFIC_LIGHT_GAP: f32 = 8.;
/// Span of the traffic-light cluster, close button through zoom (60pt on
/// macOS 26). Three dots plus their trailing gaps; the buttons' sizes and
/// spacing are AppKit metrics, their origin ours via `traffic_light_position`.
const TRAFFIC_LIGHT_CLUSTER_WIDTH: f32 = TRAFFIC_LIGHT_SIZE * 3. + TRAFFIC_LIGHT_GAP * 3.;
/// The Windows port's floating-cluster left inset, centred on the collapsed
/// rail axis like `traffic_light_position` does for the OS-drawn lights.
const TRAFFIC_LIGHT_INSET_X: f32 = (COLLAPSED_SIDEBAR_WIDTH - TRAFFIC_LIGHT_CLUSTER_WIDTH) / 2.;
/// The cluster's top inset, matching the OS default for this window style.
const TRAFFIC_LIGHT_INSET_Y: f32 = 9.;
/// Right edge of the cluster band, including the trailing button gap.
const TRAFFIC_LIGHT_BAND_RIGHT: f32 = TRAFFIC_LIGHT_INSET_X + TRAFFIC_LIGHT_CLUSTER_WIDTH;
/// Bottom edge of the cluster's dot row.
const TRAFFIC_LIGHT_BAND_BOTTOM: f32 = TRAFFIC_LIGHT_INSET_Y + TRAFFIC_LIGHT_SIZE;

/// One rect of a window drag strip, anchored by insets so it tracks animated
/// parents. A strip is built from rects like this so it never covers the
/// traffic-light dots: GPUI resolves overlapping control areas by paint
/// order, and any Drag area painted before the lights overlay would swallow
/// the dots' clicks (ADR-0002).
pub(super) fn window_drag_strip(top: f32, left: f32, height: f32) -> Div {
    div()
        .absolute()
        .top(px(top))
        .left(px(left))
        .right_0()
        .h(px(height))
        .window_control_area(WindowControlArea::Drag)
}

/// The hover-and-selection pill behind a collapsed sidebar row.
const SIDEBAR_FILL_COLLAPSED: f32 = 42.;
/// How far the collapsed pill sits in from the row's left edge.
const SIDEBAR_FILL_INSET: f32 = 2.;
const CATALOG_STALE_TIME: Duration = Duration::from_secs(5 * 60);
/// How long a confirmation notice stays before it dismisses itself.
pub(super) const NOTICE_CONFIRMATION_LIFETIME: Duration = Duration::from_secs(4);
const COMPACT_PLAYER_LEFT_WIDTH: f32 = 220.;
/// The compact player bar's right cluster: the queue and volume buttons
/// with the volume slider between them.
const COMPACT_PLAYER_RIGHT_WIDTH: f32 =
    2. * TRANSPORT_BUTTON_SIZE + 2. * TRANSPORT_BUTTON_GAP + VOLUME_SLIDER_WIDTH;

fn sidebar_transition_duration(
    current_width: f32,
    target_width: f32,
    expanded_width: f32,
) -> Duration {
    let remaining_fraction = ((target_width - current_width).abs()
        / (expanded_width - COLLAPSED_SIDEBAR_WIDTH))
        .clamp(0., 1.);
    Duration::from_millis(
        (SIDEBAR_OPEN_MILLIS * remaining_fraction)
            .round()
            .max(SIDEBAR_FLOOR_MILLIS) as u64,
    )
}

fn interpolate_sidebar_width(from: f32, target: f32, delta: f32) -> f32 {
    from + (target - from) * delta
}

/// Leading padding that puts a row's leading content on the collapsed rail
/// axis at progress 0 and back on its expanded padding at progress 1.
fn sidebar_row_pad(expanded_pad: f32, content_width: f32, progress: f32) -> f32 {
    let collapsed = COLLAPSED_SIDEBAR_WIDTH / 2. - SIDEBAR_CONTENT_PAD - content_width / 2.;
    collapsed + (expanded_pad - collapsed) * progress
}

/// The pill behind a sidebar row: a content-hugging box when collapsed, the
/// full row when expanded. Returns (width, left inset, leading padding).
fn sidebar_fill_geometry(
    expanded_pad: f32,
    content_width: f32,
    row_width: f32,
    progress: f32,
) -> (f32, f32, f32) {
    let left = SIDEBAR_FILL_INSET * (1. - progress);
    let width = SIDEBAR_FILL_COLLAPSED + (row_width - SIDEBAR_FILL_COLLAPSED) * progress;
    let pad = sidebar_row_pad(expanded_pad, content_width, progress) - left;
    (width, left, pad)
}

/// Close-button origin that centres the traffic-light cluster on the
/// collapsed rail axis, rather than trusting the OS default inset to land
/// there.
fn traffic_light_position() -> gpui_kit::Point<Pixels> {
    point(px(TRAFFIC_LIGHT_INSET_X), px(TRAFFIC_LIGHT_INSET_Y))
}

/// The width the window's content column gets: the window minus the width
/// the sidebar rail occupies. Every width-sensitive tier reads this instead
/// of the window width, so collapsing the rail hands its space to the
/// content.
fn content_width(window_width: f32, sidebar_width: f32) -> f32 {
    (window_width - sidebar_width).max(0.)
}

fn uses_compact_content_layout(content_width: f32) -> bool {
    content_width < COMPACT_BREAKPOINT
}

fn uses_compact_player_layout(content_width: f32) -> bool {
    content_width < COMPACT_PLAYER_BREAKPOINT
}

/// One transport button's edge length.
const TRANSPORT_BUTTON_SIZE: f32 = 40.;
/// The gap between transport buttons; the progress row uses it too.
const TRANSPORT_BUTTON_GAP: f32 = 8.;
/// The four transport buttons (shuffle, previous, play, next) with the
/// gaps between them.
const TRANSPORT_CLUSTER_WIDTH: f32 = 4. * TRANSPORT_BUTTON_SIZE + 3. * TRANSPORT_BUTTON_GAP;
/// The player bar's outer padding on each side.
const PLAYER_BAR_PADDING: f32 = 24.;
/// The player bar's gap between its three clusters.
const PLAYER_BAR_GAP: f32 = 24.;
/// What the compact player bar fixes across all its parts: its paddings,
/// the gap between the three clusters, the whole compact left cluster
/// (artwork, title, heart), the transport cluster, and the compact right
/// cluster (queue, volume, and the volume slider). The compact slider
/// takes what the content leaves.
const COMPACT_BAR_FIXED_WIDTH: f32 = 2. * PLAYER_BAR_PADDING
    + 2. * PLAYER_BAR_GAP
    + COMPACT_PLAYER_LEFT_WIDTH
    + TRANSPORT_CLUSTER_WIDTH
    + COMPACT_PLAYER_RIGHT_WIDTH;
/// Below this content width the player bar's timeline (the progress slider
/// and its time labels) folds away; artwork and transport keep working.
/// The floor sits inside the compact tier: the compact slider always shows
/// with its minimum at the floor, so the timeline only folds where the
/// content cannot spare even that.
const PLAYER_TIMELINE_FLOOR: f32 = COMPACT_BAR_FIXED_WIDTH + PROGRESS_SLIDER_MIN_WIDTH;
/// The least width the timeline's slider keeps when it shows at all.
const PROGRESS_SLIDER_MIN_WIDTH: f32 = 160.;

/// The compact progress slider's width at a given content width: the space
/// the content has left over after the bar's other fixed parts, at least
/// 160px. `None` below the timeline floor, where the timeline folds away.
fn compact_progress_slider_width(content_width: f32) -> Option<f32> {
    if content_width >= PLAYER_TIMELINE_FLOOR {
        Some((content_width - COMPACT_BAR_FIXED_WIDTH).max(PROGRESS_SLIDER_MIN_WIDTH))
    } else {
        None
    }
}

/// Whether the sidebar should hold its compact expanded width (200) rather
/// than the full one (232): true when the full rail would leave the content
/// area below the compact breakpoint. Asking with the full rail keeps the
/// answer stable while the rail is collapsed: the tier arms for the next
/// expansion instead of flip-flopping with the rail's current width.
fn sidebar_wants_compact_layout(window_width: f32) -> bool {
    uses_compact_content_layout(content_width(window_width, expanded_sidebar_width(false)))
}

/// The sidebar's expanded width: the compact one when the window's content
/// would fall below the compact breakpoint with the full rail, the full one
/// otherwise.
fn expanded_sidebar_width(compact_layout: bool) -> f32 {
    if compact_layout { 200. } else { 232. }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Home,
    LikedSongs,
    Recent,
    Search,
    Playlists,
    Playlist,
    Artist,
    Album,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchKind {
    Tracks,
    Playlists,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtistSection {
    Popular,
    Discography,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionState {
    Starting,
    Failed,
    SetupRequired,
    AuthorizationRequired,
    Connecting,
    Ready,
}

/// How a notice talks: what happened went well, or it did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoticeSeverity {
    /// A small win the listener only needed to hear once.
    Confirmation,
    /// Something went wrong and stays wrong until the listener dismisses it.
    Failure,
}

/// One line of news in the banner, plus who owns its going away.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Notice {
    /// A radio request is walking its way through the backend, and a later
    /// event resolves it: RadioStarted, RadioFailed or RadioCancelled.
    /// Nothing here may time the notice out.
    RadioPending,
    /// Tied to no later event, so the banner itself has to let it go.
    Timed(NoticeItem),
}

/// The words and severity of a notice that no event will resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NoticeItem {
    message: String,
    severity: NoticeSeverity,
}

/// What one confirmation timer is allowed to dismiss: the notice generation
/// it went up at. Two confirmations with the same words compare equal, so
/// the notice alone cannot keep one timer from closing the next; the
/// generation tells them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ArmedNotice {
    generation: usize,
}

impl NoticeItem {
    fn severity(&self) -> NoticeSeverity {
        self.severity
    }
}

impl Notice {
    fn item(&self) -> Option<&NoticeItem> {
        match self {
            Notice::RadioPending => None,
            Notice::Timed(item) => Some(item),
        }
    }

    /// The banner text, whichever way the notice is held.
    fn message(&self) -> &str {
        match self {
            Notice::RadioPending => STARTING_RADIO_MESSAGE,
            Notice::Timed(item) => &item.message,
        }
    }

    /// Confirmations dismiss themselves after a short while; failures and
    /// the radio pending state stay until resolved or replaced.
    fn auto_dismisses(&self) -> bool {
        matches!(
            self.item(),
            Some(NoticeItem {
                severity: NoticeSeverity::Confirmation,
                ..
            })
        )
    }
}

/// What the radio request notice says while the backend works on it.
const STARTING_RADIO_MESSAGE: &str = "Starting track radio…";

fn volume_for_pointer(pointer_x: f32, window_width: f32) -> f32 {
    ((pointer_x - (window_width - VOLUME_SLIDER_RIGHT_INSET)) / VOLUME_SLIDER_WIDTH).clamp(0., 1.)
}

/// Maps a pointer position to a track position on the progress slider.
///
/// The window width says how wide the whole window is; the content width
/// says what the bar spans. In the full tier the bar's centre cluster is
/// centred on the content and the slider keeps `PROGRESS_SLIDER_WIDTH` —
/// the same width the centre box reserves for it, so the drawn track and
/// this math agree. In the compact tier the slider takes what the content
/// has left over, floored at the slider minimum. Below the timeline floor
/// the slider is not on screen, so this never runs for one.
fn seek_for_pointer(
    pointer_x: f32,
    window_width: f32,
    content_width: f32,
    duration_ms: u32,
) -> u32 {
    let compact = uses_compact_player_layout(content_width);
    let slider_width = if compact {
        compact_progress_slider_width(content_width)
    } else {
        Some(PROGRESS_SLIDER_WIDTH)
    };
    let Some(slider_width) = slider_width else {
        return 0;
    };
    let center_left = if compact {
        PLAYER_BAR_PADDING + COMPACT_PLAYER_LEFT_WIDTH + PLAYER_BAR_GAP
    } else {
        window_width / 2. + (PLAYER_LEFT_WIDTH - PLAYER_RIGHT_WIDTH - PLAYER_CENTER_WIDTH) / 2.
    };
    let left = center_left + PROGRESS_TIME_WIDTH + PROGRESS_GAP;
    let fraction = ((pointer_x - left) / slider_width).clamp(0., 1.);
    (fraction * duration_ms as f32) as u32
}

fn format_duration(duration_ms: u32) -> String {
    let seconds = duration_ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn next_request_id(request_id: &mut u64) -> u64 {
    *request_id = request_id.wrapping_add(1);
    *request_id
}

/// Whether something loaded at `loaded_at` is younger than `max_age`. Wall
/// clock, not `Instant`: `Instant` does not advance while the machine is
/// asleep, so a sleep would leave stale data looking fresh.
fn is_fresh(loaded_at: Option<SystemTime>, max_age: Duration) -> bool {
    loaded_at.is_some_and(|loaded_at| loaded_at.elapsed().is_ok_and(|elapsed| elapsed < max_age))
}

fn catalog_data_is_fresh(loaded_at: Option<SystemTime>) -> bool {
    is_fresh(loaded_at, CATALOG_STALE_TIME)
}

/// The sidebar's distance-scaled variant of the motion scale, in millis so
/// the duration test can name its numbers. The open cap sits a step under
/// `motion::FAST` (the drawer's panel-arrival duration) because the rail
/// moves the whole page under the listener's eye; the floor is `motion`
/// half-MICRO. The numbers are the sidebar's own and predate the scale —
/// retiming them to different steps would change every collapse.
const SIDEBAR_OPEN_MILLIS: f32 = 180.;
const SIDEBAR_FLOOR_MILLIS: f32 = 60.;

async fn receive_backend_event_batch(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<BackendEvent>,
) -> Option<Vec<BackendEvent>> {
    let first = events.recv().await?;
    let mut batch = vec![first];
    while let Ok(event) = events.try_recv() {
        batch.push(event);
    }
    Some(batch)
}

/// Stream of backend events from the worker thread.
type BackendEvents = tokio::sync::mpsc::UnboundedReceiver<BackendEvent>;

mod actions;
mod appearance;
mod assets;
mod bootstrap;
mod catalog;
mod chrome;
mod components;
mod events;
mod home;
mod library;
mod library_pages;
mod media_controls;
mod onboarding;
mod page;
mod player;
mod player_bar;
mod router;
mod services;
mod session;
mod settings;
mod sidebar;
mod track_list;
mod track_row;
mod windows;
mod workspace;

pub fn run() {
    bootstrap::run();
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{
        BRAND_LOGO_SIZE, BRAND_ROW_PAD, COLLAPSED_SIDEBAR_WIDTH, COMPACT_BAR_FIXED_WIDTH,
        COMPACT_PLAYER_BREAKPOINT, NAV_GLYPH_WIDTH, NAV_ROW_PAD, PLAYER_TIMELINE_FLOOR,
        PROGRESS_SLIDER_MIN_WIDTH, PROGRESS_SLIDER_WIDTH, SIDEBAR_CONTENT_PAD,
        SIDEBAR_FILL_COLLAPSED, SIDEBAR_FILL_INSET, TRAFFIC_LIGHT_CLUSTER_WIDTH,
        compact_progress_slider_width, interpolate_sidebar_width, resolve_dark_mode,
        seek_for_pointer, sidebar_fill_geometry, sidebar_row_pad, sidebar_transition_duration,
        traffic_light_position, uses_compact_content_layout, uses_compact_player_layout,
        volume_for_pointer,
    };
    use gpui_kit::WindowAppearance;
    use spotify_gpui_client::storage::ThemePreference;

    #[test]
    fn theme_preference_resolves_against_window_appearance() {
        assert!(resolve_dark_mode(
            ThemePreference::System,
            WindowAppearance::Dark
        ));
        assert!(!resolve_dark_mode(
            ThemePreference::System,
            WindowAppearance::Light
        ));
        assert!(!resolve_dark_mode(
            ThemePreference::Light,
            WindowAppearance::Dark
        ));
        assert!(resolve_dark_mode(
            ThemePreference::Dark,
            WindowAppearance::Light
        ));
    }

    #[test]
    fn pointer_position_is_clamped_to_volume_range() {
        let window_width = 1280.;

        assert_eq!(volume_for_pointer(1100., window_width), 0.);
        assert_eq!(volume_for_pointer(1196., window_width), 0.5);
        assert_eq!(volume_for_pointer(1300., window_width), 1.);
    }

    #[test]
    fn pointer_position_is_mapped_to_track_duration() {
        // Full tier: the centre cluster is centred on the content.
        assert_eq!(seek_for_pointer(524., 1280., 1280., 200_000), 0);
        assert_eq!(seek_for_pointer(694., 1280., 1280., 200_000), 100_000);
        assert_eq!(seek_for_pointer(864., 1280., 1280., 200_000), 200_000);
        // Compact tier, expanded rail: the slider takes what the content
        // has left over. The bar's fixed parts leave the slider at
        // 488 - 316 = 172, so the row maps 312..484 onto the track.
        assert_eq!(seek_for_pointer(312., 720., 488., 200_000), 0);
        assert_eq!(seek_for_pointer(398., 720., 488., 200_000), 100_000);
        assert_eq!(seek_for_pointer(484., 720., 488., 200_000), 200_000);
        // Below the timeline floor the slider is not on screen, so a
        // pointer on where it would be seeks nowhere.
        assert_eq!(seek_for_pointer(368., 720., 600., 200_000), 0);
        // The pointer clamps to the slider's ends.
        assert_eq!(seek_for_pointer(200., 1280., 1280., 200_000), 0);
        assert_eq!(seek_for_pointer(1200., 1280., 1280., 200_000), 200_000);
    }

    #[test]
    fn responsive_breakpoints_are_exclusive() {
        assert!(uses_compact_content_layout(727.));
        assert!(!uses_compact_content_layout(728.));
        assert!(uses_compact_player_layout(903.));
        assert!(!uses_compact_player_layout(904.));
    }

    #[test]
    fn timeline_folds_below_the_floor_and_fills_the_space_above_it() {
        let floor = PLAYER_TIMELINE_FLOOR;
        // The floor is only honest inside the compact tier; above the tier
        // edge the full tier always shows the timeline anyway.
        assert!(
            floor < COMPACT_PLAYER_BREAKPOINT,
            "the floor must stay in the tier it guards"
        );
        // Just below the floor: no slider.
        assert_eq!(compact_progress_slider_width(floor - 1.), None);
        // Just above it: the slider keeps its minimum.
        assert_eq!(
            compact_progress_slider_width(floor + 1.),
            Some(PROGRESS_SLIDER_MIN_WIDTH)
        );
        // Wider content: the slider takes what the content has left over.
        assert_eq!(
            compact_progress_slider_width(floor + 100.),
            Some(floor + 100. - COMPACT_BAR_FIXED_WIDTH)
        );
    }

    #[test]
    fn track_table_columns_fold_from_the_widest_to_the_thinnest() {
        let full = track_table_columns(1100.);
        assert!(full.album && full.date_added);
        // The date column folds first.
        let no_date = track_table_columns(1099.);
        assert!(no_date.album && !no_date.date_added);
        // Then the album column.
        let no_album = track_table_columns(879.);
        assert!(!no_album.album && !no_album.date_added);
        // The minimal table keeps `#`, title, and time; heart and actions
        // survive every width, so nothing further folds.
        assert_eq!(track_table_columns(500.), no_album);
    }

    #[test]
    fn sidebar_reversal_starts_from_the_sampled_width() {
        assert_eq!(interpolate_sidebar_width(150., 232., 0.), 150.);
        assert_eq!(interpolate_sidebar_width(150., 232., 1.), 232.);
        assert_eq!(interpolate_sidebar_width(150., 72., 0.), 150.);
    }

    #[test]
    fn sidebar_transition_duration_scales_with_remaining_distance() {
        assert_eq!(
            sidebar_transition_duration(COLLAPSED_SIDEBAR_WIDTH, 232., 232.).as_millis(),
            180
        );
        assert_eq!(
            sidebar_transition_duration(155., 232., 232.).as_millis(),
            90
        );
        assert_eq!(
            sidebar_transition_duration(220., 232., 232.).as_millis(),
            60
        );
    }

    #[test]
    fn collapsed_sidebar_row_pads_match_the_verified_geometry() {
        // The measured values from cadence-5ym; a change to the rail width,
        // the content pad, or a row's content size must be a conscious one.
        assert_eq!(sidebar_row_pad(BRAND_ROW_PAD, BRAND_LOGO_SIZE, 0.), 7.);
        assert_eq!(sidebar_row_pad(NAV_ROW_PAD, NAV_GLYPH_WIDTH, 0.), 14.5);
    }

    #[test]
    fn expanded_sidebar_rows_keep_their_padding() {
        assert_eq!(
            sidebar_row_pad(BRAND_ROW_PAD, BRAND_LOGO_SIZE, 1.),
            BRAND_ROW_PAD
        );
        assert_eq!(
            sidebar_row_pad(NAV_ROW_PAD, NAV_GLYPH_WIDTH, 1.),
            NAV_ROW_PAD
        );
    }

    #[test]
    fn traffic_lights_sit_on_the_collapsed_rail_axis() {
        let origin = f32::from(traffic_light_position().x);
        assert_eq!(
            origin + TRAFFIC_LIGHT_CLUSTER_WIDTH / 2.,
            COLLAPSED_SIDEBAR_WIDTH / 2.
        );
        // The cluster must also fit inside the rail, not just centre on it.
        assert!(origin >= 0.);
    }

    #[test]
    fn collapsed_fill_centres_on_the_rail_axis() {
        // The pill hugs its content symmetrically only while its own centre
        // sits on the rail axis.
        assert_eq!(
            SIDEBAR_CONTENT_PAD + SIDEBAR_FILL_INSET + SIDEBAR_FILL_COLLAPSED / 2.,
            COLLAPSED_SIDEBAR_WIDTH / 2.
        );
    }

    #[test]
    fn sidebar_fill_hugs_content_collapsed_and_spans_the_row_expanded() {
        let (width, left, pad) = sidebar_fill_geometry(NAV_ROW_PAD, NAV_GLYPH_WIDTH, 200., 0.);
        assert_eq!((width, left), (SIDEBAR_FILL_COLLAPSED, SIDEBAR_FILL_INSET));
        assert_eq!(
            SIDEBAR_CONTENT_PAD + left + pad + NAV_GLYPH_WIDTH / 2.,
            COLLAPSED_SIDEBAR_WIDTH / 2.
        );

        let (width, left, pad) = sidebar_fill_geometry(NAV_ROW_PAD, NAV_GLYPH_WIDTH, 200., 1.);
        assert_eq!((width, left, pad), (200., 0., NAV_ROW_PAD));
    }
}

#[cfg(test)]
mod event_bridge_tests {
    use super::{
        BackendEvent, CATALOG_STALE_TIME, catalog_data_is_fresh, next_request_id,
        receive_backend_event_batch,
    };
    use std::time::{Duration, SystemTime};

    #[tokio::test]
    async fn batches_events_that_are_already_queued() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        sender.send(BackendEvent::SetupRequired).unwrap();
        sender
            .send(BackendEvent::CatalogReady { generation: 0 })
            .unwrap();

        let events = receive_backend_event_batch(&mut receiver).await.unwrap();

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], BackendEvent::SetupRequired));
        assert!(matches!(
            events[1],
            BackendEvent::CatalogReady { generation: 0 }
        ));
    }

    #[tokio::test]
    async fn closes_after_all_senders_are_dropped() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(sender);

        assert!(receive_backend_event_batch(&mut receiver).await.is_none());
    }

    #[test]
    fn generations_advance_with_wrapping_request_ids() {
        let mut generation = u64::MAX;
        assert_eq!(next_request_id(&mut generation), 0);
        assert_eq!(next_request_id(&mut generation), 1);
    }

    #[test]
    fn catalog_data_expires_after_the_stale_time() {
        assert!(!catalog_data_is_fresh(None));
        assert!(catalog_data_is_fresh(Some(SystemTime::now())));
        assert!(!catalog_data_is_fresh(Some(
            SystemTime::now() - CATALOG_STALE_TIME - Duration::from_secs(1)
        )));
    }
}

/// Contrast floors for the palette's structural colors, checked on every
/// platform. The ratios use the WCAG 2.x formula: relative luminance from
/// sRGB channels with the standard linearization, then the lighter value
/// plus 0.05 over the darker value plus 0.05. The app's colors are opaque
/// sRGB hex values and gpui does not gamma-correct them, so this plain
/// channel model matches what the surfaces actually are.
///
/// A future palette edit that drops a structural color below its floor
/// fails here instead of quietly reintroducing invisible structure.
#[cfg(test)]
mod palette_contrast_tests {
    use super::CadencePalette;

    /// Borders must separate a panel from the surface it sits on or in.
    const BORDER_FLOOR: f64 = 2.5;
    /// The focus ring must stay findable over anything it can encircle.
    const FOCUS_RING_FLOOR: f64 = 3.0;
    /// A selected fill must read as distinct from the canvas behind it.
    const SELECTION_FLOOR: f64 = 1.35;
    /// Danger keeps its distance from the selection fill so a red state is
    /// never mistaken for an active one.
    const DANGER_CLEARANCE_FLOOR: f64 = 2.0;

    /// The linear component of one sRGB channel, as the WCAG 2.x spec
    /// defines it.
    fn srgb_channel(value: u32) -> f64 {
        let c = f64::from(value) / 255.;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Relative luminance of an 0xRRGGBB color.
    fn relative_luminance(hex: u32) -> f64 {
        let (r, g, b) = ((hex >> 16) & 0xFF, (hex >> 8) & 0xFF, hex & 0xFF);
        0.2126 * srgb_channel(r) + 0.7152 * srgb_channel(g) + 0.0722 * srgb_channel(b)
    }

    /// WCAG 2.x contrast ratio between two 0xRRGGBB colors.
    fn contrast_ratio(a: u32, b: u32) -> f64 {
        let (l1, l2) = (relative_luminance(a), relative_luminance(b));
        let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn hex(color: u32) -> String {
        format!("{color:06X}")
    }

    /// The helper itself, against published reference ratios. Without these
    /// pins the threshold assertions below could pass vacuously.
    #[test]
    fn contrast_helper_matches_published_reference_ratios() {
        assert_eq!(contrast_ratio(0xFFFFFF, 0x000000), 21.0);
        assert_eq!(contrast_ratio(0x808080, 0x808080), 1.0);
        // The published ratios are rounded to two decimals.
        assert!((contrast_ratio(0xC0C0C0, 0xFFFFFF) - 1.82).abs() < 0.005);
        assert!((contrast_ratio(0x767676, 0xFFFFFF) - 4.54).abs() < 0.005);
        // Order of the two colors must not matter.
        assert_eq!(
            contrast_ratio(0x767676, 0xFFFFFF),
            contrast_ratio(0xFFFFFF, 0x767676)
        );
    }

    #[test]
    fn borders_separate_from_every_adjacent_surface_in_both_themes() {
        for (theme, palette) in [
            ("light", CadencePalette::LIGHT),
            ("dark", CadencePalette::DARK),
        ] {
            for (surface_name, surface) in [
                ("canvas", palette.canvas),
                ("surface", palette.surface),
                ("surface_raised", palette.surface_raised),
                ("control", palette.control),
            ] {
                assert!(
                    contrast_ratio(palette.border, surface) >= BORDER_FLOOR,
                    "{} theme border {} against {} {} is below the {} floor",
                    theme,
                    hex(palette.border),
                    surface_name,
                    hex(surface),
                    BORDER_FLOOR
                );
            }
        }
    }

    #[test]
    fn focus_ring_stays_visible_on_every_surface_in_both_themes() {
        for (theme, palette) in [
            ("light", CadencePalette::LIGHT),
            ("dark", CadencePalette::DARK),
        ] {
            for (surface_name, surface) in [
                ("canvas", palette.canvas),
                ("surface", palette.surface),
                ("surface_raised", palette.surface_raised),
                ("control", palette.control),
                ("selection", palette.selection),
            ] {
                assert!(
                    contrast_ratio(palette.focus_ring, surface) >= FOCUS_RING_FLOOR,
                    "{} theme focus ring {} against {} {} is below the {} floor",
                    theme,
                    hex(palette.focus_ring),
                    surface_name,
                    hex(surface),
                    FOCUS_RING_FLOOR
                );
            }
        }
    }

    #[test]
    fn selection_reads_as_distinct_from_the_canvas_in_both_themes() {
        for (theme, palette) in [
            ("light", CadencePalette::LIGHT),
            ("dark", CadencePalette::DARK),
        ] {
            assert!(
                contrast_ratio(palette.selection, palette.canvas) >= SELECTION_FLOOR,
                "{} theme selection {} against canvas {} is below the {} floor",
                theme,
                hex(palette.selection),
                hex(palette.canvas),
                SELECTION_FLOOR
            );
        }
    }

    #[test]
    fn danger_keeps_clear_of_the_selection_fill_in_both_themes() {
        for (theme, palette) in [
            ("light", CadencePalette::LIGHT),
            ("dark", CadencePalette::DARK),
        ] {
            assert!(
                contrast_ratio(palette.danger, palette.selection) >= DANGER_CLEARANCE_FLOOR,
                "{} theme danger {} against selection {} is below the {} floor",
                theme,
                hex(palette.danger),
                hex(palette.selection),
                DANGER_CLEARANCE_FLOOR
            );
        }
    }
}
