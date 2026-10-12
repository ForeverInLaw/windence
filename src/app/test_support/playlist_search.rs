use super::*;
use spotify_gpui_client::backend::PlaylistContents;

fn playlist(source_id: &str, count: usize) -> model::Playlist {
    model::Playlist {
        provider: model::Provider::Spotify,
        source_id: source_id.into(),
        name: "Searchable playlist".into(),
        owner: "Cadence Ensemble".into(),
        track_count: count as u32,
        artwork_url: None,
    }
}

impl Fixture {
    fn open_playlist(&mut self, source_id: &str, tracks: Vec<model::Track>) {
        let playlist = playlist(source_id, tracks.len());
        let workspace = self.workspace.clone().expect("workspace fixture");
        self.update(|window, cx| {
            workspace
                .upgrade()
                .expect("live workspace")
                .update(cx, |workspace, cx| {
                    workspace.load_playlist(playlist, window, cx);
                    workspace.open_playlist(Route::LikedSongs, cx);
                });
        });
        match self.backend.commands.try_recv().expect("playlist request") {
            BackendCommand::LoadPlaylist { respond, .. } => {
                respond
                    .send(Ok(PlaylistContents::Loaded {
                        playlist: None,
                        tracks: tracks
                            .into_iter()
                            .map(model::ListedTrack::undated)
                            .collect(),
                    }))
                    .expect("live playlist reply");
            }
            other => panic!("expected playlist request, got {other:?}"),
        }
        self.update(|_, _| {});
    }

    fn filter_playlist(&mut self, query: &str) {
        self.update(|window, cx| {
            window.click("playlist-search-input", cx);
            window.press(SECONDARY_A, cx);
            window.input(query, cx);
        });
    }
}

#[test]
fn typing_and_clearing_playlist_search_filters_rows_without_commands() {
    let mut fixture = Fixture::new(false);
    fixture.open_playlist("searchable", vec![track(10), track(11)]);
    fixture.filter_playlist("12");
    fixture.update(|window, cx| {
        assert!(window.try_find(("spotify-track", 0usize)).is_none());
        assert!(window.find(("spotify-track", 1usize)).visible());
        window.within("playlist-search-input").click("clean", cx);
    });
    fixture.update(|window, _| {
        assert_eq!(window.find("playlist-search-input").value(), Some(""));
        assert!(window.find(("spotify-track", 0usize)).visible());
        assert!(window.find(("spotify-track", 1usize)).visible());
    });
    fixture.no_commands();
}

#[test]
fn playlist_search_matches_words_across_unicode_title_artist_and_album() {
    let mut fixture = Fixture::new(false);
    let mut french = track(10);
    french.title = "Beyoncé Nuit".into();
    french.artist = "ZAZ".into();
    french.album = "ÉTÉ".into();
    let mut russian = track(11);
    russian.title = "ДоМоЙ".into();
    russian.artist = "КиНо".into();
    russian.album = "Ночь".into();
    let mut drive = track(12);
    drive.title = "Night Drive".into();
    drive.album = "Été".into();
    fixture.open_playlist("unicode", vec![french, russian, drive]);

    for (query, expected) in [
        ("BEYONCÉ", vec![0]),
        ("zaZ", vec![0]),
        ("été", vec![0, 2]),
        ("  домой КИНО  ", vec![1]),
        ("nuit été", vec![0]),
        ("nuit ночь", vec![]),
    ] {
        fixture.filter_playlist(query);
        fixture.update(|window, _| {
            for index in 0..3usize {
                assert_eq!(
                    window.try_find(("spotify-track", index)).is_some(),
                    expected.contains(&index),
                    "query {query:?}, row {index}"
                );
            }
            assert_eq!(
                window.try_find("track-list-no-results").is_some(),
                expected.is_empty()
            );
        });
    }
    fixture.no_commands();
}

#[test]
fn filtered_duplicate_menu_and_header_play_keep_the_complete_sorted_context() {
    let mut fixture = Fixture::new(false);
    let mut zulu = track(10);
    zulu.title = "Zulu".into();
    let mut beta = track(11);
    beta.title = "Beta".into();
    let mut alpha = track(12);
    alpha.title = "Alpha".into();
    fixture.open_playlist(
        "duplicates",
        vec![zulu.clone(), beta.clone(), alpha.clone(), beta.clone()],
    );
    fixture.update(|window, cx| window.click("sort-title", cx));
    fixture.filter_playlist("Beta");
    fixture.update(|window, cx| window.hover(("spotify-track", 2usize), cx));
    fixture.update(|window, cx| window.click(("track-actions", 2usize), cx));
    fixture.update(|window, cx| window.click(("track-menu-play", 2usize), cx));
    for (index, shuffled) in [(2, false), (0, false), (0, true)] {
        match fixture.backend.commands.try_recv().expect("play command") {
            BackendCommand::PlayContext {
                tracks,
                index: selected,
                shuffled: actual_shuffle,
                context_uri,
                ..
            } => {
                assert_eq!(
                    tracks,
                    vec![alpha.clone(), beta.clone(), beta.clone(), zulu.clone()]
                );
                assert_eq!(selected, index);
                assert_eq!(actual_shuffle, shuffled);
                assert_eq!(context_uri.as_deref(), Some("spotify:playlist:duplicates"));
            }
            other => panic!("expected full context, got {other:?}"),
        }
        if index == 2 {
            fixture.update(|window, cx| window.click("playlist-play", cx));
        } else if !shuffled {
            fixture.update(|window, cx| window.click("playlist-shuffle", cx));
        }
    }
    fixture.no_commands();
}

#[test]
fn another_playlist_resets_search_but_refreshing_the_same_playlist_keeps_it() {
    let mut fixture = Fixture::new(false);
    fixture.open_playlist("first", vec![track(10), track(11)]);
    fixture.filter_playlist("12");
    fixture.open_playlist("first", vec![track(12), track(11), track(11)]);
    fixture.update(|window, _| {
        assert_eq!(window.find("playlist-search-input").value(), Some("12"));
        assert!(window.try_find(("spotify-track", 0usize)).is_none());
        assert!(window.find(("spotify-track", 1usize)).visible());
        assert!(window.find(("spotify-track", 2usize)).visible());
    });
    fixture.open_playlist("second", vec![track(13), track(14)]);
    fixture.update(|window, _| {
        assert_eq!(window.find("playlist-search-input").value(), Some(""));
        assert!(window.find(("spotify-track", 0usize)).visible());
        assert!(window.find(("spotify-track", 1usize)).visible());
    });
    fixture.no_commands();
}

#[cfg(target_os = "macos")]
const SECONDARY_F: &str = "cmd-f";
#[cfg(not(target_os = "macos"))]
const SECONDARY_F: &str = "ctrl-f";

#[test]
fn playlist_shortcut_focuses_local_search_and_keeps_global_search_and_editor_keys() {
    let mut fixture = Fixture::new(false);
    fixture.press(SECONDARY_F);
    assert_eq!(fixture.route(), Route::LikedSongs);
    fixture.open_playlist("keyboard", vec![track(10), track(11)]);
    fixture.update(|window, cx| window.click("search-input", cx));
    fixture.press(SECONDARY_F);
    fixture.update(|window, cx| {
        assert_eq!(window.find("playlist-search-input").focused(), Some(true));
        window.input("Song", cx);
    });
    fixture.press("space");
    fixture.update(|window, _| {
        assert_eq!(window.find("playlist-search-input").value(), Some("Song "));
    });
    fixture.no_commands();
    fixture.press(SECONDARY_K);
    assert_eq!(fixture.route(), Route::Search);
    fixture.update(|window, _| assert_eq!(window.find("search-input").focused(), Some(true)));
    fixture.press(SECONDARY_F);
    assert_eq!(fixture.route(), Route::Search);
    fixture.update(|window, _| assert_eq!(window.find("search-input").focused(), Some(true)));
    fixture.no_commands();
}

#[test]
fn filtering_a_scrolled_playlist_returns_to_the_first_match() {
    let mut fixture = Fixture::new(false);
    fixture.open_playlist("long", (10..70).map(track).collect());
    fixture.update(|window, cx| {
        let position = window.find(("spotify-track", 0usize)).bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-2400.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
    fixture.update(|window, _| assert!(window.try_find(("spotify-track", 0usize)).is_none()));
    fixture.filter_playlist("Song");
    fixture.update(|window, _| assert!(window.find(("spotify-track", 0usize)).visible()));
    fixture.filter_playlist("Song 11");
    fixture.update(|window, _| {
        assert!(window.find(("spotify-track", 0usize)).visible());
        assert!(window.try_find(("spotify-track", 1usize)).is_none());
    });
    fixture.no_commands();
}

#[test]
fn filtered_rows_keep_liked_actions_and_keyboard_playback() {
    let mut fixture = Fixture::new(false);
    fixture.open_playlist("actions", vec![track(10), track(11), track(12)]);
    fixture.filter_playlist("12");
    fixture.tab_to(("spotify-track", 1usize));
    fixture.press("enter");
    match fixture.backend.commands.try_recv().expect("keyboard play") {
        BackendCommand::PlayContext { tracks, index, .. } => {
            assert_eq!(tracks, vec![track(10), track(11), track(12)]);
            assert_eq!(index, 1);
        }
        other => panic!("expected keyboard playback, got {other:?}"),
    }
    fixture.update(|window, cx| window.hover(("spotify-track", 1usize), cx));
    fixture.update(|window, cx| window.click(("spotify-liked", 1usize), cx));
    match fixture.backend.commands.try_recv().expect("liked action") {
        BackendCommand::SetLiked {
            track: liked,
            liked: true,
        } => assert_eq!(liked, track(11)),
        other => panic!("expected like for selected row, got {other:?}"),
    }
    fixture.no_commands();
}

#[test]
fn playlist_search_fits_full_compact_and_folded_content_tiers() {
    for (width, collapsed) in MATRIX {
        let cx = HeadlessAppContext::with_asset_source(
            Arc::new(NoopTextSystem),
            Arc::new(assets::AppAssets),
        );
        let mut fixture = Fixture::with_layout(cx, false, ThemePreference::Light, width, collapsed);
        fixture.open_playlist("layout", vec![track(10), track(11)]);
        fixture.filter_playlist("missing");
        let workspace = fixture.workspace.clone().expect("workspace fixture");
        fixture.update(|window, cx| {
            let rail = workspace
                .upgrade()
                .expect("live workspace")
                .read(cx)
                .sidebar
                .read(cx)
                .target_width();
            let field = window.find("playlist-search-input").bounds();
            assert!(field.left() >= px(rail + 32.));
            assert!(field.right() <= px(width - 32.));
            assert!(field.size.width > px(0.));
            assert!(field.size.height > px(0.));
            assert!(window.find("track-list-no-results").visible());
        });
        fixture.no_commands();
    }
}

#[test]
fn filtering_the_live_dj_queue_preserves_station_playback_guards() {
    let mut fixture = Fixture::new(false);
    fixture.open_playlist(
        spotify_gpui_client::dj::SOURCE_ID,
        vec![track(10), track(11)],
    );
    fixture.filter_playlist("12");
    fixture.playback_event(BackendEvent::StationChanged(true));
    fixture.update(|window, _| assert!(window.find("track-list-no-results").visible()));
    fixture.filter_playlist("Song 2");
    fixture.update(|window, cx| {
        assert!(window.try_find("playlist-shuffle").is_none());
        assert!(window.try_find("playlist-pin").is_none());
        window.click(("spotify-track", 1usize), cx);
    });
    fixture.no_commands();
    fixture.update(|window, cx| window.click("playlist-play", cx));
    assert!(matches!(
        fixture.backend.commands.try_recv(),
        Ok(BackendCommand::PlayDj)
    ));
    fixture.no_commands();
}
