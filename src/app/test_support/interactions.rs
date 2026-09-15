use super::test_support::{BackendProbe, initialize, settle, track, workspace};
use crate::app::{appearance, assets, onboarding, services};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    App, AppContext as _, HeadlessAppContext, NoopTextSystem, Window, WindowHandle, px, size,
};
use spotify_gpui_client::backend::{BackendCommand, BackendEvent};
use spotify_gpui_client::model;
use spotify_gpui_client::storage::ThemePreference;
use std::sync::Arc;

struct Fixture {
    cx: HeadlessAppContext,
    window: WindowHandle<Root>,
    backend: BackendProbe,
}

impl Fixture {
    fn new(settings: bool) -> Self {
        let mut cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::AppAssets),
        );
        let backend = cx.update(|cx| initialize(cx, ThemePreference::Dark));
        let (window, workspace) = workspace(&mut cx, 1280., 820.);
        if settings {
            cx.update(|cx| workspace.update(cx, |workspace, cx| workspace.open_settings(cx)));
        }
        settle(&mut cx, window.into());
        Self {
            cx,
            window,
            backend,
        }
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let result = self
            .cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .expect("fixture window");
        settle(&mut self.cx, self.window.into());
        result
    }

    fn no_commands(&mut self) {
        assert!(matches!(
            self.backend.commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }
}

#[test]
fn search_shortcut_focuses_input_and_enter_submits_text_without_toggling_playback() {
    let mut fixture = Fixture::new(false);
    fixture.no_commands();
    // On non-macOS the "secondary" modifier is ctrl, so the binding parses
    // from the same string a real keyboard sends.
    fixture.update(|window, cx| window.press("ctrl-k", cx));
    fixture.update(|window, cx| {
        assert_eq!(window.find("search-input").focused(), Some(true));
        window.input("Blue", cx);
        window.press("space", cx);
        window.input("Train", cx);
        assert_eq!(window.find("search-input").value(), Some("Blue Train"));
    });
    fixture.no_commands();
    fixture.update(|window, cx| window.press("enter", cx));
    match fixture.backend.commands.try_recv().expect("search command") {
        BackendCommand::SearchCatalog { query, .. } => assert_eq!(query, "Blue Train"),
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
    fixture.update(|_, cx| assert!(!services::AppServices::player(cx).read(cx).playing()));
}

#[test]
fn client_id_validation_keeps_focus_and_only_submits_valid_input() {
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
        .expect("setup window");
    settle(&mut cx, window.into());
    let mut fixture = Fixture {
        cx,
        window,
        backend,
    };
    fixture.update(|window, cx| {
        window.click("client-id-input", cx);
        window.input("not-a-client-id", cx);
    });
    // The submit button runs the same validation the Enter path does.
    fixture.update(|window, cx| window.click("save-spotify-client-id", cx));
    fixture.no_commands();
    fixture.update(|window, cx| {
        assert_eq!(
            window.find("client-id-input").focused(),
            Some(true),
            "an invalid Client ID must keep the field focused"
        );
        assert_eq!(
            window.find("client-id-input").value(),
            Some("not-a-client-id")
        );
        assert!(
            services::AppServices::session(cx)
                .read(cx)
                .setup_error()
                .is_some()
        );
        window.click("client-id-input", cx);
        window.press("ctrl-a", cx);
        window.input("0123456789abcdef0123456789abcdef", cx);
    });
    fixture.update(|window, cx| {
        assert!(
            services::AppServices::session(cx)
                .read(cx)
                .setup_error()
                .is_none()
        );
        window.click("save-spotify-client-id", cx);
    });
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("configuration command")
    {
        BackendCommand::ConfigureSpotify { client_id, .. } => {
            assert_eq!(client_id, "0123456789abcdef0123456789abcdef");
        }
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
}

#[test]
fn queue_toggle_and_close_button_control_the_panel_without_backend_commands() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, cx| {
        assert!(window.try_find("close-queue").is_none());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| {
        assert!(window.find("close-queue").visible());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| {
        assert!(window.try_find("close-queue").is_none());
        window.click("queue-toggle", cx);
    });
    fixture.update(|window, cx| window.click("close-queue", cx));
    fixture.update(|window, _| assert!(window.try_find("close-queue").is_none()));
    fixture.no_commands();
}

#[test]
fn duplicate_track_actions_preserve_row_index_and_liked_does_not_start_playback() {
    let mut fixture = Fixture::new(false);
    // Our port carries no favorites list: the liked list itself carries the
    // liked state, so every rendered row starts liked and a heart click
    // unlikes it. Unlike the duplicated track(1) first: `set_liked` drops
    // every entry with that source id, collapsing both duplicates out of
    // the list, so the row's identity is proven by what the unlike names —
    // and the heart click must never toggle playback.
    fixture.update(|window, cx| window.hover(("spotify-track", 2usize), cx));
    fixture.update(|window, cx| window.click(("spotify-liked", 2usize), cx));
    match fixture.backend.commands.try_recv().expect("liked command") {
        BackendCommand::SetLiked {
            track: unliked,
            liked,
        } => {
            assert_eq!(unliked.source_id, track(1).source_id);
            assert!(!liked);
        }
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
    fixture.update(|_, cx| {
        assert!(!services::AppServices::player(cx).read(cx).playing());
    });
    // The collection arrives again with both duplicates, and the play
    // command started from the row menu preserves the displayed index.
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::LibraryLoaded {
                    generation: 0,
                    liked_tracks: vec![
                        model::ListedTrack::undated(track(0)),
                        model::ListedTrack::undated(track(1)),
                        model::ListedTrack::undated(track(1)),
                        model::ListedTrack::undated(track(2)),
                    ],
                    playlists: Vec::new(),
                },
                0,
                cx,
            );
        });
    });
    fixture.update(|window, cx| window.hover(("spotify-track", 2usize), cx));
    fixture.update(|window, cx| window.click(("track-actions", 2usize), cx));
    fixture.update(|window, cx| {
        assert!(window.try_find(("track-menu-play", 1usize)).is_none());
        window.click(("track-menu-play", 2usize), cx);
    });
    match fixture.backend.commands.try_recv().expect("play command") {
        BackendCommand::PlayContext { tracks, index, .. } => {
            assert_eq!(index, 2);
            assert_eq!(tracks.len(), 4);
            assert_eq!(tracks[1].source_id, tracks[2].source_id);
            assert_eq!(tracks[index].source_id, track(1).source_id);
        }
        command => panic!("unexpected command: {command:?}"),
    }
    fixture.no_commands();
}

#[test]
fn mute_sends_volume_and_restores_the_previous_level() {
    let mut fixture = Fixture::new(false);
    let initial = fixture.update(|_, cx| services::AppServices::player(cx).read(cx).volume());
    fixture.update(|window, cx| window.click("volume", cx));
    assert_eq!(*fixture.backend.volume.borrow_and_update(), 0.);
    fixture.update(|window, cx| window.click("volume", cx));
    assert_eq!(*fixture.backend.volume.borrow_and_update(), initial);
    fixture.no_commands();
}
