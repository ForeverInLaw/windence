use super::test_support::{initialize, settle, workspace};
use crate::app::{
    BackendEvent, NOTICE_CONFIRMATION_LIFETIME, Notice, NoticeSeverity, Workspace, assets,
    next_request_id,
};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{HeadlessAppContext, NoopTextSystem, WeakEntity};
use spotify_gpui_client::storage::ThemePreference;
use std::sync::Arc;
use std::time::Duration;

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

    /// Runs `f` with the workspace, its window, and its own `Context`, then
    /// draws. The context form is what the notice methods take.
    fn workspace<R>(
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

    /// The visible banner's label, if one is up. Draws a frame first so the
    /// facts observed are from this turn, not the last one.
    fn banner_label(&mut self) -> Option<String> {
        settle(&mut self.cx, self.window.into());
        self.cx
            .update_window(self.window.into(), |_, window, _| {
                window
                    .try_find("action-notice")
                    .map(|banner| banner.label().unwrap_or_default().to_owned())
            })
            .expect("fixture window")
    }

    /// Handled a radio event straight onto the workspace, the way the event
    /// pump would deliver it.
    fn radio_event(&mut self, event: BackendEvent) {
        self.workspace(|workspace, _, cx| {
            workspace.handle_backend_events(vec![event], cx);
        });
    }

    fn click_dismiss(&mut self) {
        let window = self.window.into();
        self.cx
            .update_window(window, |_, window, cx| {
                window.click("dismiss-action-notice", cx)
            })
            .expect("fixture window");
        settle(&mut self.cx, self.window.into());
    }
}

/// A confirmation dismisses itself once its window has passed.
#[test]
fn a_confirmation_dismisses_itself_after_its_window() {
    let mut fixture = Fixture::new();
    fixture.workspace(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        )
    });
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Redirect URI copied"),
        "the banner is up right after the notice"
    );

    // Just before the window closes the notice must still be there.
    fixture
        .cx
        .advance_clock(NOTICE_CONFIRMATION_LIFETIME - Duration::from_millis(1));
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Redirect URI copied")
    );

    // One tick later the timer has fired and the banner is gone.
    fixture.cx.advance_clock(Duration::from_millis(1));
    fixture.cx.run_until_parked();
    assert_eq!(fixture.banner_label(), None);
}

/// A failure stays until the listener dismisses it.
#[test]
fn a_failure_outlives_the_same_advance_and_leaves_on_dismiss() {
    let mut fixture = Fixture::new();
    fixture.workspace(|workspace, _, cx| {
        workspace.show_notice(
            "Unable to start track radio".to_owned(),
            NoticeSeverity::Failure,
            cx,
        )
    });

    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Unable to start track radio"),
        "a failure is not on a timer"
    );

    fixture.click_dismiss();
    assert_eq!(fixture.banner_label(), None);
}

/// A notice that replaces a timed one is not closed by the older timer.
#[test]
fn a_replacing_notice_is_safe_from_the_previous_timer() {
    let mut fixture = Fixture::new();
    fixture.workspace(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        );
        workspace.show_notice(
            "Unable to start track radio".to_owned(),
            NoticeSeverity::Failure,
            cx,
        );
    });

    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Unable to start track radio"),
        "the first timer must not close its replacement"
    );
}

/// A second confirmation with the same words still gets its own lifetime:
/// the first timer must not close it early, and its own timer must close
/// it once that lifetime has passed.
#[test]
fn an_identical_confirmation_gets_its_own_lifetime() {
    let mut fixture = Fixture::new();
    fixture.workspace(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        );
    });
    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME / 2);
    fixture.workspace(|workspace, _, cx| {
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        );
    });

    // The first timer fires here; the second banner must still be up.
    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME / 2);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Redirect URI copied"),
        "the first timer must not close an identical confirmation early"
    );

    // The second banner's own timer then closes it.
    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME / 2);
    fixture.cx.run_until_parked();
    assert_eq!(fixture.banner_label(), None);
}

/// The radio pending notice is event-managed: no timer closes it.
#[test]
fn the_radio_pending_notice_is_never_timed() {
    let mut fixture = Fixture::new();
    fixture.workspace(|workspace, _, cx| workspace.set_notice(Notice::RadioPending, cx));

    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Starting track radio…"),
        "an event-managed notice does not dismiss itself"
    );
}

/// RadioStarted resolves the pending banner through its own lifecycle.
#[test]
fn radio_started_still_clears_its_notice() {
    let mut fixture = Fixture::new();
    let request_id = fixture.workspace(|workspace, _, cx| {
        workspace.set_notice(Notice::RadioPending, cx);
        let request_id = next_request_id(&mut workspace.radio_request_id);
        workspace.pending_radio_request = Some(request_id);
        request_id
    });

    fixture.radio_event(BackendEvent::RadioStarted { request_id });
    assert_eq!(
        fixture.banner_label(),
        None,
        "RadioStarted must clear the pending banner"
    );
}

/// A RadioFailed swaps the pending banner for a failure that stays up.
#[test]
fn radio_failed_swaps_the_pending_banner_for_a_failure() {
    let mut fixture = Fixture::new();
    let request_id = fixture.workspace(|workspace, _, cx| {
        workspace.set_notice(Notice::RadioPending, cx);
        let request_id = next_request_id(&mut workspace.radio_request_id);
        workspace.pending_radio_request = Some(request_id);
        request_id
    });

    fixture.radio_event(BackendEvent::RadioFailed {
        request_id,
        error: "no recommendations".to_owned(),
    });
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Track radio unavailable: no recommendations"),
    );

    // The failure is not on a timer either.
    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label().as_deref(),
        Some("Track radio unavailable: no recommendations"),
    );
}

/// A RadioCancelled clears the pending banner without a timer touching it.
#[test]
fn radio_cancelled_still_clears_its_notice() {
    let mut fixture = Fixture::new();
    let request_id = fixture.workspace(|workspace, _, cx| {
        workspace.set_notice(Notice::RadioPending, cx);
        let request_id = next_request_id(&mut workspace.radio_request_id);
        workspace.pending_radio_request = Some(request_id);
        request_id
    });

    fixture.radio_event(BackendEvent::RadioCancelled { request_id });
    assert_eq!(fixture.banner_label(), None);
}

/// A confirmation that lands while a radio request is pending replaces the
/// pending banner and is timed; its expiry does not resurrect the radio one.
#[test]
fn a_confirmation_over_the_radio_pending_banner_is_timed_and_not_resurrected() {
    let mut fixture = Fixture::new();
    let request_id = fixture.workspace(|workspace, _, cx| {
        workspace.set_notice(Notice::RadioPending, cx);
        let request_id = next_request_id(&mut workspace.radio_request_id);
        workspace.pending_radio_request = Some(request_id);
        workspace.show_notice(
            "Redirect URI copied".to_owned(),
            NoticeSeverity::Confirmation,
            cx,
        );
        request_id
    });

    fixture.cx.advance_clock(NOTICE_CONFIRMATION_LIFETIME);
    fixture.cx.run_until_parked();
    assert_eq!(
        fixture.banner_label(),
        None,
        "the timed confirmation went away, and the pending banner never returned"
    );

    // The radio request is still pending, so its lifecycle still resolves it.
    fixture.radio_event(BackendEvent::RadioStarted { request_id });
    assert_eq!(fixture.banner_label(), None);
}
