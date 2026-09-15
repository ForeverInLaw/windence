use super::test_support::{BackendProbe, initialize, settle, track, workspace};
use crate::app::{Route, Workspace, appearance, assets, onboarding, services};
use gpui_kit::InputEvent as _;
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    App, AppContext as _, HeadlessAppContext, KeyUpEvent, Keystroke, NoopTextSystem, WeakEntity,
    Window, WindowHandle, px, size,
};
use spotify_gpui_client::backend::{BackendCommand, BackendEvent};
use spotify_gpui_client::model;
use spotify_gpui_client::storage::ThemePreference;
use std::sync::Arc;

pub(super) struct Fixture {
    cx: HeadlessAppContext,
    window: WindowHandle<Root>,
    pub(super) workspace: Option<WeakEntity<Workspace>>,
    backend: BackendProbe,
}

/// The "secondary" modifier the bindings parse from, per platform: cmd on
/// macOS, ctrl elsewhere. Every test that presses a secondary shortcut
/// sends the same string a real keyboard would.
#[cfg(target_os = "macos")]
const SECONDARY_K: &str = "cmd-k";
#[cfg(not(target_os = "macos"))]
const SECONDARY_K: &str = "ctrl-k";
#[cfg(target_os = "macos")]
const SECONDARY_A: &str = "cmd-a";
#[cfg(not(target_os = "macos"))]
const SECONDARY_A: &str = "ctrl-a";

impl Fixture {
    pub(super) fn new(settings: bool) -> Self {
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
            workspace: Some(workspace.downgrade()),
            backend,
        }
    }

    fn route(&mut self) -> Route {
        let workspace = self.workspace.clone().expect("workspace fixture");
        self.update(|_, cx| {
            workspace
                .upgrade()
                .expect("live workspace")
                .read(cx)
                .router
                .route()
        })
    }

    fn play(&mut self, current: model::Track) {
        self.update(|_, cx| {
            services::AppServices::player(cx).update(cx, |player, cx| {
                player.handle_backend_event(
                    BackendEvent::PlaybackSnapshotLoaded {
                        current,
                        next: vec![track(1)],
                        injected: vec![false; 2],
                        position_ms: 0,
                    },
                    cx,
                );
            });
        });
    }

    fn requested_artist(&mut self) -> String {
        match self.backend.commands.try_recv().expect("artist request") {
            BackendCommand::LoadArtist { source_id, .. } => source_id,
            other => panic!("expected an artist request, got {other:?}"),
        }
    }

    fn requested_album(&mut self) -> String {
        match self.backend.commands.try_recv().expect("album request") {
            BackendCommand::LoadAlbum { source_id, .. } => source_id,
            other => panic!("expected an album request, got {other:?}"),
        }
    }

    pub(super) fn update<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let result = self
            .cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .expect("fixture window");
        settle(&mut self.cx, self.window.into());
        result
    }

    pub(super) fn no_commands(&mut self) {
        assert!(matches!(
            self.backend.commands.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    fn press(&mut self, key: &str) {
        self.update(|window, cx| {
            let keystroke = Keystroke::parse(key).expect("fixture key");
            window.dispatch_keystroke(keystroke.clone(), cx);
            window.dispatch_event(KeyUpEvent { keystroke }.to_platform_input(), cx);
        });
    }
}

fn artist_ref(name: &str, source_id: &str) -> model::ArtistRef {
    model::ArtistRef {
        name: name.into(),
        source_id: Some(source_id.into()),
        spotify_uri: Some(format!("spotify:artist:{source_id}")),
    }
}

/// A track Spotify fully described, with two credited artists and an album.
fn credited_track() -> model::Track {
    let mut track = track(0);
    track.artist = "Frankie Valli, The Four Seasons".into();
    track.artists = vec![
        artist_ref("Frankie Valli", "artist-valli"),
        artist_ref("The Four Seasons", "artist-seasons"),
    ];
    track.album_ref = Some(model::AlbumRef {
        name: "Grease".into(),
        source_id: Some("album-grease".into()),
        spotify_uri: Some("spotify:album:album-grease".into()),
        artwork_url: None,
    });
    track
}

#[test]
fn search_shortcut_focuses_input_and_enter_submits_text_without_toggling_playback() {
    let mut fixture = Fixture::new(false);
    fixture.no_commands();
    // The "secondary" modifier parses from the same string a real keyboard
    // sends, so the shortcut focuses the search input on every platform.
    fixture.update(|window, cx| window.press(SECONDARY_K, cx));
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
fn player_bar_opens_each_credited_artist_and_the_album_separately() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.no_commands();

    fixture.update(|window, cx| window.click(("player-artist", 1usize), cx));
    assert_eq!(fixture.requested_artist(), "artist-seasons");
    assert_eq!(fixture.route(), Route::Artist);

    fixture.update(|window, cx| window.click(("player-artist", 0usize), cx));
    assert_eq!(fixture.requested_artist(), "artist-valli");
    assert_eq!(fixture.route(), Route::Artist);

    fixture.update(|window, cx| window.click("player-artwork", cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);

    // Dropping the album reply above failed that load, so the title retries it.
    fixture.update(|window, cx| window.click("player-title", cx));
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);
    fixture.no_commands();
}

#[test]
fn player_bar_title_opens_the_album_from_the_keyboard() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.press(SECONDARY_K);
    assert_eq!(fixture.route(), Route::Search);

    fixture.press("tab");
    fixture.update(|window, _| {
        let title = window.find("player-title");
        assert_eq!(title.focused(), Some(true));
        assert_eq!(title.role(), Some(gpui_kit::Role::Link));
    });
    fixture.press("enter");
    assert_eq!(fixture.requested_album(), "album-grease");
    assert_eq!(fixture.route(), Route::Album);
    fixture.no_commands();
}

#[test]
fn player_bar_credits_without_references_stay_plain() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, _| {
        assert!(window.try_find("player-artwork").is_none());
        assert!(window.try_find("player-title").is_none());
        assert!(window.try_find(("player-artist", 0usize)).is_none());
    });
    fixture.no_commands();
}

#[test]
fn space_keeps_toggling_playback_and_leaves_the_title_link_alone() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.press(SECONDARY_K);
    fixture.press("tab");
    fixture.update(|window, _| {
        assert_eq!(window.find("player-title").focused(), Some(true));
    });
    fixture.press("space");

    // Space reaches the playback binding, not the focused link: the route is
    // unchanged and no page load went out.
    assert_eq!(fixture.route(), Route::Search);
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("playback command")
    {
        BackendCommand::Resume => {}
        other => panic!("expected playback to toggle, got {other:?}"),
    }
    fixture.no_commands();
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
        workspace: None,
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
        window.press(SECONDARY_A, cx);
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

/// Every icon-only control names its action for a screen reader. The
/// builders require the label, so these assert what the names say, not
/// that they exist.
#[test]
fn icon_buttons_announce_their_actions_in_human_words() {
    let mut fixture = Fixture::new(false);
    fixture.play(credited_track());
    fixture.update(|window, _| {
        let expected = [
            ("next", "Next track"),
            ("previous", "Previous track"),
            ("volume", "Volume"),
            ("queue-toggle", "Queue"),
            ("shuffle-toggle", "Shuffle"),
        ];
        for (id, label) in expected {
            assert_eq!(
                window.find(id).label(),
                Some(label),
                "the {id} button must announce itself as {label:?}"
            );
        }
    });
    // The heart follows the state the player bar already knows: the
    // snapshot track starts liked, so the heart offers removal.
    let liked = fixture.update(|_, cx| {
        services::AppServices::library(cx)
            .read(cx)
            .is_liked(&track(0))
    });
    let expected = if liked {
        "Remove from Liked Songs"
    } else {
        "Add to Liked Songs"
    };
    fixture.update(|window, _| {
        assert_eq!(window.find("player-liked").label(), Some(expected));
    });
    // The queue drawer's close button exists only while the drawer is open.
    fixture.update(|window, cx| window.click("queue-toggle", cx));
    fixture.update(|window, _| {
        assert_eq!(window.find("close-queue").label(), Some("Close queue"));
    });
    fixture.no_commands();
}

/// The track-row heart and actions button carry their names, and the
/// row heart follows the liked state the row already knows.
#[test]
fn track_row_controls_announce_their_actions() {
    let mut fixture = Fixture::new(false);
    fixture.update(|window, _| {
        assert_eq!(
            window.find(("spotify-liked", 0usize)).label(),
            // The fixture library starts with every rendered row liked.
            Some("Remove from Liked Songs")
        );
        assert_eq!(
            window.find(("track-actions", 0usize)).label(),
            Some("More actions")
        );
    });
    fixture.no_commands();
}

/// An overlay control announces its action. The notice banner exists only
/// while a notice is up, so the test raises one the way the workspace
/// itself would, then dismisses it.
#[test]
fn action_notice_dismiss_announces_itself() {
    let mut fixture = Fixture::new(false);
    let workspace = fixture.workspace.clone().expect("workspace fixture");
    fixture.update(|_, cx| {
        workspace
            .upgrade()
            .expect("live workspace")
            .update(cx, |workspace, cx| {
                workspace.action_notice = Some("Starting track radio…".to_owned());
                cx.notify();
            });
    });
    fixture.update(|window, _| {
        assert_eq!(
            window.find("dismiss-action-notice").label(),
            Some("Dismiss")
        );
    });
    fixture.update(|window, cx| window.click("dismiss-action-notice", cx));
    fixture.update(|window, _| assert!(window.try_find("dismiss-action-notice").is_none()));
    fixture.no_commands();
}

#[test]
fn a_cached_library_from_a_superseded_account_is_dropped_without_setting_boot_refreshing() {
    let mut fixture = Fixture::new(false);
    fixture.update(|_, cx| {
        services::AppServices::library(cx).update(cx, |library, cx| {
            library.handle_backend_event(
                BackendEvent::CachedLibrary {
                    // Not the generation the UI is on: the backend stamps
                    // its own account generation on the event, and a cache
                    // from another account must not paint over this one.
                    generation: 1,
                    liked_tracks: Vec::new(),
                    playlists: vec![model::Playlist {
                        provider: model::Provider::Spotify,
                        source_id: "leaked".into(),
                        name: "Leaked".into(),
                        owner: "Other Account".into(),
                        track_count: 3,
                        artwork_url: None,
                    }],
                },
                0,
                cx,
            );
        })
    });
    fixture.update(|_, cx| {
        let library = services::AppServices::library(cx).read(cx);
        // Nothing arrived: no cached rows, and no boot-revalidation banner
        // that a served cache would have set.
        assert!(library.playlist_rows().is_empty());
        assert!(!library.reloading());
    });
    fixture.no_commands();
}
