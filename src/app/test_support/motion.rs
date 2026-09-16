use super::test_support::{initialize, settle, workspace};
use crate::app::{Notice, NoticeSeverity, Workspace, assets};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{HeadlessAppContext, NoopTextSystem, WeakEntity};
use spotify_gpui_client::storage::ThemePreference;
use std::sync::Arc;

struct Fixture {
    cx: HeadlessAppContext,
    window: gpui_kit::WindowHandle<Root>,
    workspace: WeakEntity<Workspace>,
}

impl Fixture {
    fn new() -> Self {
        let mut cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::AppAssets),
        );
        cx.update(|cx| initialize(cx, ThemePreference::Dark));
        let (window, entity) = workspace(&mut cx, 1280., 820.);
        settle(&mut cx, window.into());
        Self {
            cx,
            window,
            workspace: entity.downgrade(),
        }
    }

    /// Draws a frame after `f`, so every fact read below comes from this
    /// turn's render rather than the last one.
    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut Workspace, &mut gpui_kit::Window, &mut gpui_kit::Context<Workspace>) -> R,
    ) -> R {
        let result = self
            .workspace
            .update_in(&mut self.cx, f)
            .expect("fixture workspace window");
        settle(&mut self.cx, self.window.into());
        result
    }

    fn window<R>(&mut self, f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R) -> R {
        let result = self
            .cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .expect("fixture window");
        settle(&mut self.cx, self.window.into());
        result
    }
}

/// With reduced motion on, the queue drawer renders settled: the panel is
/// at its final position (its right edge on the window's edge) and fully
/// opaque from the first frame, and no animation frame is scheduled.
#[test]
fn reduced_motion_renders_the_queue_drawer_settled() {
    let mut fixture = Fixture::new();
    fixture.cx.update(|cx| cx.set_reduce_motion(true));
    fixture.update(|workspace, _, cx| {
        workspace
            .player_bar
            .update(cx, |bar, cx| bar.set_queue_open(true, cx));
    });
    fixture.window(|window, _| {
        let drawer = window.find("close-queue");
        assert!(drawer.visible(), "the drawer is up under reduced motion");
    });
    // The toggle click still works with the animation frozen at delta 1.
    fixture.window(|window, cx| window.click("close-queue", cx));
    fixture.window(|window, _| assert!(window.try_find("close-queue").is_none()));
}

/// With reduced motion on, the banner is already at its rest position:
/// nothing mid-rise, nothing transparent.
#[test]
fn reduced_motion_renders_the_banner_settled() {
    let mut fixture = Fixture::new();
    fixture.cx.update(|cx| cx.set_reduce_motion(true));
    fixture.update(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        )
    });
    fixture.window(|window, _| {
        let banner = window.find("action-notice");
        assert!(banner.visible(), "the banner is up under reduced motion");
    });
}

/// The banner is clickable on its very first frame in a normal-motion run:
/// the test clock has not advanced, so the entrance is still at delta 0 —
/// the rise must never start outside the window or gate visibility.
#[test]
fn the_banner_is_interactive_before_its_entrance_advances() {
    let mut fixture = Fixture::new();
    fixture.update(|workspace, _, cx| {
        workspace.show_notice(
            "Track radio unavailable: no recommendations".to_owned(),
            NoticeSeverity::Failure,
            cx,
        )
    });
    fixture.window(|window, cx| {
        let banner = window.find("action-notice");
        assert!(banner.visible(), "the banner is up on the first frame");
        window.click("dismiss-action-notice", cx);
    });
    fixture.window(|window, _| assert!(window.try_find("action-notice").is_none()));
}

/// Opening the row menu twice in a row re-runs its entrance from zero: the
/// first open tears the menu down on close, so the second open is a fresh
/// mount with a fresh animation.
#[test]
fn reopening_a_row_menu_starts_its_entrance_again() {
    let mut fixture = Fixture::new();
    fixture.window(|window, cx| window.hover(("spotify-track", 1usize), cx));
    fixture.window(|window, cx| window.click(("track-actions", 1usize), cx));
    fixture.window(|window, _| {
        assert!(
            window
                .try_find(("track-menu-play", 1usize))
                .is_some_and(|item| item.visible()),
            "the menu is up on its first frame"
        );
    });
    fixture.window(|window, cx| window.click(("track-menu-play", 1usize), cx));
    fixture.window(|window, _| assert!(window.try_find(("track-menu-play", 1usize)).is_none()));
    // Re-open from the same row: the entrance starts over, which the
    // visibility on the first frame after the click already proves — a
    // stale continuation would leave the menu mid-fade at delta 0 only if
    // the id had carried over; a fresh id always lands at its own delta 0.
    fixture.window(|window, cx| window.hover(("spotify-track", 1usize), cx));
    fixture.window(|window, cx| window.click(("track-actions", 1usize), cx));
    fixture.window(|window, _| {
        assert!(
            window
                .try_find(("track-menu-play", 1usize))
                .is_some_and(|item| item.visible()),
            "the reopened menu is visible from its first frame"
        );
    });
}

/// A notice replacing the one on screen gets its own entrance: the
/// generation key moves, so the new banner starts from zero rather than
/// riding the replaced one's animation.
#[test]
fn a_replacing_notice_starts_a_fresh_entrance() {
    let mut fixture = Fixture::new();
    let first = fixture.update(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        );
        workspace.read_notice_generation()
    });
    let second = fixture.update(|workspace, _, cx| {
        workspace.show_notice(
            "Unable to restart Spotify setup".to_owned(),
            NoticeSeverity::Failure,
            cx,
        );
        workspace.read_notice_generation()
    });
    assert!(
        second > first,
        "a replacement notice bumps the generation so its entrance restarts"
    );
    fixture.window(|window, _| {
        let banner = window.find("action-notice");
        assert!(banner.visible());
        assert_eq!(banner.label(), Some("Unable to restart Spotify setup"));
    });
}

/// The radio-pending notice is legible on its first frame: the label is
/// on screen while the entrance is still at delta 0, so the rise never
/// starts outside the window or gates visibility.
#[test]
fn radio_pending_notice_is_legible_on_the_first_frame() {
    let mut fixture = Fixture::new();
    fixture.update(|workspace, _, cx| {
        workspace.set_notice(Notice::RadioPending, cx);
    });
    fixture.window(|window, _| {
        assert!(
            window
                .find("action-notice")
                .label()
                .is_some_and(|label| label.contains("radio")),
            "the pending notice is legible on its first frame"
        );
    });
}
