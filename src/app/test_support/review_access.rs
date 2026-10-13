use super::{Fixture, appearance, assets, initialize, onboarding, services, settle};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, HeadlessAppContext, NoopTextSystem, px, size};
use spotify_gpui_client::{
    backend::{BackendCommand, BackendEvent},
    storage::ThemePreference,
};
use std::sync::Arc;

fn fixture() -> Fixture {
    let mut cx = HeadlessAppContext::with_asset_source(
        Arc::new(NoopTextSystem),
        Arc::new(assets::AppAssets),
    );
    let backend = cx.update(|cx| {
        let backend = initialize(cx, ThemePreference::Light);
        services::AppServices::session(cx).update(cx, |session, cx| {
            session.handle_backend_event(BackendEvent::SetupRequired, cx);
        });
        backend
    });
    let window = cx
        .open_window(size(px(1280.), px(820.)), |window, cx| {
            appearance::Appearance::attach(window, cx);
            let onboarding = cx.new(|cx| onboarding::Onboarding::new(window, cx));
            cx.new(|cx| Root::new(onboarding, window, cx))
        })
        .unwrap();
    settle(&mut cx, window.into());
    Fixture {
        cx,
        window,
        workspace: None,
        backend,
    }
}

#[test]
fn review_access_validates_the_code_and_keeps_it_out_of_diagnostics() {
    let mut fixture = fixture();
    fixture.update(|window, cx| window.click("open-review-access", cx));
    fixture.update(|window, cx| {
        window.click("review-code-input", cx);
        window.input("invalid", cx);
    });
    fixture.update(|window, cx| window.click("connect-review-access", cx));
    fixture.no_commands();
    fixture.update(|_, cx| {
        assert!(
            services::AppServices::session(cx)
                .read(cx)
                .setup_error()
                .is_some()
        )
    });
    fixture.update(|window, cx| window.click("leave-review-access", cx));
    fixture.update(|window, _| {
        window.find("client-id-input");
    });
    let code = format!("review-{}", "a".repeat(64));
    fixture.update(|window, cx| window.click("open-review-access", cx));
    fixture.update(|window, cx| {
        window.click("review-code-input", cx);
        window.input(&code, cx);
    });
    fixture.update(|window, cx| window.click("connect-review-access", cx));
    let command = fixture.backend.commands.try_recv().unwrap();
    assert!(matches!(command, BackendCommand::ConnectReview { .. }));
    assert!(!format!("{command:?}").contains(&code));
    fixture.update(|window, _| assert_eq!(window.find("review-code-input").value(), None));
}

#[test]
fn review_access_keeps_the_code_and_reports_failed_dispatch() {
    let mut fixture = fixture();
    fixture.backend.commands.close();
    fixture.update(|window, cx| window.click("open-review-access", cx));
    fixture.update(|window, cx| {
        window.click("review-code-input", cx);
        window.input(&format!("review-{}", "a".repeat(64)), cx);
    });
    // Retrying without typing again must still reach the closed backend.
    for _ in 0..2 {
        fixture.update(|window, cx| window.click("connect-review-access", cx));
        fixture.update(|_, cx| {
            assert_eq!(
                services::AppServices::session(cx)
                    .read(cx)
                    .setup_error()
                    .map(String::as_str),
                Some("Windence backend is busy or not running")
            );
        });
    }
}
