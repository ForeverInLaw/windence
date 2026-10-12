use super::{initialize, settle, track, workspace};
use crate::app::{appearance::Appearance, assets, page::PageEvent, services::AppServices, windows};
use gpui_kit::component::Theme;
use gpui_kit::{HeadlessAppContext, NoopTextSystem};
use spotify_gpui_client::{backend::BackendCommand, model, storage::ThemePreference};
use std::sync::Arc;

fn context() -> HeadlessAppContext {
    HeadlessAppContext::with_asset_source(Arc::new(NoopTextSystem), Arc::new(assets::AppAssets))
}

#[test]
fn opening_and_reopening_windows_keeps_the_bundled_font_across_theme_changes() {
    let mut cx = context();
    cx.update(|cx| {
        initialize(cx, ThemePreference::Dark);
        super::ready(cx);
        windows::open_main_window(cx);
    });

    for preference in [ThemePreference::Light, ThemePreference::Dark] {
        cx.update(|cx| {
            let handle = AppServices::main_window(cx).expect("main window");
            handle
                .update(cx, |_, window, cx| {
                    Appearance::set_preference(preference, window, cx);
                    assert_eq!(Theme::global(cx).font_family.as_ref(), "Inter");
                    window.remove_window();
                })
                .unwrap();
        });
        cx.run_until_parked();
        cx.update(windows::show_app_window);
    }
    cx.update(|cx| {
        assert!(AppServices::main_window(cx).is_some());
        assert_eq!(Theme::global(cx).font_family.as_ref(), "Inter");
    });
}

#[test]
fn closing_a_workspace_releases_its_views_and_pending_home_request() {
    let mut cx = context();
    let mut backend = cx.update(|cx| initialize(cx, ThemePreference::Dark));
    let (window, workspace) = workspace(&mut cx, 1280., 820.);
    let weak_workspace = workspace.downgrade();
    let weak_home = cx.update(|cx| {
        let home = workspace.read(cx).home.clone();
        home.update(cx, |home, cx| home.revalidate(cx));
        home.downgrade()
    });
    let BackendCommand::LoadHomeFeed { respond } = backend.commands.try_recv().unwrap() else {
        panic!("expected a Home request");
    };
    drop(workspace);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    assert!(weak_workspace.upgrade().is_none());
    assert!(weak_home.upgrade().is_none());
    assert!(
        respond
            .send(Ok(model::HomeFeed {
                greeting: None,
                shelves: vec![]
            }))
            .is_err()
    );
}

#[test]
fn reopening_a_workspace_keeps_process_playback_and_routes_home_events_once() {
    let mut cx = context();
    let mut backend = cx.update(|cx| initialize(cx, ThemePreference::Dark));
    let (window, old_workspace) = workspace(&mut cx, 1280., 820.);
    let old_home = cx.update(|cx| old_workspace.read(cx).home.clone());
    let player = cx.update(|cx| AppServices::player(cx));
    let library = cx.update(|cx| AppServices::library(cx));
    drop(old_workspace);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    // A page retained by a caller cannot act through a closed workspace.
    cx.update(|cx| {
        old_home.update(cx, |_, cx| cx.emit(PageEvent::StartRadio(track(0))));
        AppServices::player(cx).update(cx, |player, cx| player.toggle(cx));
    });
    assert!(matches!(
        backend.commands.try_recv(),
        Ok(BackendCommand::Resume)
    ));
    assert!(backend.commands.try_recv().is_err());

    let (window, new_workspace) = workspace(&mut cx, 1280., 820.);
    cx.update(|cx| {
        assert_eq!(AppServices::player(cx), player);
        assert_eq!(AppServices::library(cx), library);
        new_workspace.read(cx).home.clone().update(cx, |_, cx| {
            cx.emit(PageEvent::OpenArtist(model::ArtistRef {
                name: "Artist".into(),
                source_id: Some("artist-1".into()),
                spotify_uri: Some("spotify:artist:artist-1".into()),
            }));
        });
    });
    settle(&mut cx, window.into());
    let BackendCommand::LoadArtist { source_id, .. } = backend.commands.try_recv().unwrap() else {
        panic!("expected the Home card's artist request");
    };
    assert_eq!(source_id, "artist-1");
    assert!(backend.commands.try_recv().is_err());
}
