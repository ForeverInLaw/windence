use super::*;

fn snapshot(
    revision: &str,
    tracks: &[model::Track],
    next_offset: u32,
    total: u32,
) -> PlaylistSnapshot {
    PlaylistSnapshot {
        revision: revision.into(),
        tracks: tracks
            .iter()
            .cloned()
            .map(model::ListedTrack::undated)
            .collect(),
        next_offset,
        total,
    }
}

fn open_stream(fixture: &mut Fixture, source_id: &str, total: usize) -> PlaylistStream {
    match fixture.request_playlist(source_id, total) {
        BackendCommand::StreamPlaylist {
            playlist,
            requests,
            updates,
        } => {
            assert_eq!(playlist.source_id, source_id);
            PlaylistStream {
                requests,
                updates,
                latest: None,
            }
        }
        other => panic!("expected playlist stream, got {other:?}"),
    }
}

fn scroll_to_tail(fixture: &mut Fixture) {
    fixture.update(|window, cx| {
        let position = window.find("track-list").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-6000.))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    });
}

fn duplicate_tracks() -> Vec<model::Track> {
    let mut zulu = track(10);
    zulu.title = "Zulu".into();
    let mut beta = track(11);
    beta.title = "Beta".into();
    let mut alpha = track(12);
    alpha.title = "Alpha".into();
    let mut gamma = track(13);
    gamma.title = "Gamma".into();
    vec![zulu, beta.clone(), beta, alpha, gamma]
}

fn assert_playback(
    fixture: &mut Fixture,
    expected: &[model::Track],
    selected: usize,
    shuffled: bool,
) {
    match fixture
        .backend
        .commands
        .try_recv()
        .expect("full playlist playback")
    {
        BackendCommand::PlayContext {
            tracks,
            index,
            shuffled: actual,
            context_uri,
            ..
        } => {
            assert_eq!(tracks, expected);
            assert_eq!(index, selected);
            assert_eq!(actual, shuffled);
            assert_eq!(context_uri.as_deref(), Some("spotify:playlist:paged"));
        }
        other => panic!("expected playlist playback, got {other:?}"),
    }
    fixture.no_commands();
}

#[test]
fn a_cold_playlist_stops_after_its_first_page_until_the_listener_scrolls() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 100);
    let first_page = (0..50).map(track).collect::<Vec<_>>();
    stream.publish(&mut fixture, snapshot("v1", &first_page, 50, 100));
    for _ in 0..3 {
        fixture.update(|window, _| {
            assert!(window.find(("spotify-track", 0usize)).visible());
        });
    }
    stream.no_requests();
    fixture.no_commands();

    scroll_to_tail(&mut fixture);
    scroll_to_tail(&mut fixture);
    stream.next_page_request();
    stream.no_requests();
    fixture.update(|window, _| {
        assert!(window.find(("spotify-track", 49usize)).visible());
    });
    let complete = (0..100).map(track).collect::<Vec<_>>();
    stream.publish(&mut fixture, snapshot("v1", &complete, 100, 100));
    stream.no_requests();
    fixture.no_commands();
}

#[test]
fn search_and_sort_complete_a_partial_playlist_before_using_tracks_beyond_its_prefix() {
    for search in [true, false] {
        let mut fixture = Fixture::new(false);
        let mut stream = open_stream(&mut fixture, "paged", 60);
        let tracks = duplicate_tracks();
        // Unavailable raw items still count towards these Spotify offsets.
        stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
        if search {
            fixture.filter_playlist("Alpha");
        } else {
            fixture.update(|window, cx| window.click("sort-title", cx));
        }
        let respond = stream.complete_request();
        stream.no_requests();
        fixture.no_commands();
        let complete = snapshot("v1", &tracks, 60, 60);
        stream.publish(&mut fixture, complete.clone());
        respond.send(Ok(complete)).expect("live completion reply");
        fixture.update(|_, _| {});
        fixture.update(|window, cx| {
            let selected = if search { 3usize } else { 0usize };
            assert!(window.find(("spotify-track", selected)).visible());
            window.click("playlist-play", cx);
        });
        stream.acknowledge_complete(&mut fixture);
        let expected = if search {
            tracks
        } else {
            vec![
                tracks[3].clone(),
                tracks[1].clone(),
                tracks[2].clone(),
                tracks[4].clone(),
                tracks[0].clone(),
            ]
        };
        assert_playback(&mut fixture, &expected, 0, false);
        stream.no_requests();
    }
}

#[test]
fn partial_header_play_shuffle_and_duplicate_row_menu_wait_for_the_full_queue() {
    for action in ["play", "shuffle", "row"] {
        let mut fixture = Fixture::new(false);
        let mut stream = open_stream(&mut fixture, "paged", 60);
        let tracks = duplicate_tracks();
        stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
        if action == "row" {
            fixture.update(|window, cx| window.hover(("spotify-track", 2usize), cx));
            fixture.update(|window, cx| window.click(("track-actions", 2usize), cx));
            fixture.update(|window, cx| window.click(("track-menu-play", 2usize), cx));
        } else {
            fixture.update(|window, cx| {
                window.click(
                    if action == "shuffle" {
                        "playlist-shuffle"
                    } else {
                        "playlist-play"
                    },
                    cx,
                );
            });
        }
        let respond = stream.complete_request();
        stream.no_requests();
        fixture.no_commands();
        let complete = snapshot("v1", &tracks, 60, 60);
        stream.publish(&mut fixture, complete.clone());
        fixture.no_commands();
        respond.send(Ok(complete)).expect("live completion reply");
        fixture.update(|_, _| {});
        assert_playback(
            &mut fixture,
            &tracks,
            if action == "row" { 2 } else { 0 },
            action == "shuffle",
        );
        stream.no_requests();
    }
}

#[test]
fn sorting_filtering_and_row_play_share_completion_and_keep_the_selected_duplicate() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 60);
    let tracks = duplicate_tracks();
    stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
    fixture.update(|window, cx| window.click("sort-title", cx));
    let respond = stream.complete_request();
    fixture.filter_playlist("Beta");
    fixture.update(|window, cx| window.hover(("spotify-track", 1usize), cx));
    fixture.update(|window, cx| window.click(("track-actions", 1usize), cx));
    fixture.update(|window, cx| window.click(("track-menu-play", 1usize), cx));
    stream.no_requests();
    fixture.no_commands();

    let complete = snapshot("v1", &tracks, 60, 60);
    stream.publish(&mut fixture, complete.clone());
    respond.send(Ok(complete)).expect("live completion reply");
    fixture.update(|_, _| {});
    let sorted = vec![
        tracks[3].clone(),
        tracks[1].clone(),
        tracks[2].clone(),
        tracks[4].clone(),
        tracks[0].clone(),
    ];
    assert_playback(&mut fixture, &sorted, 2, false);
    stream.no_requests();
}

#[test]
fn complete_cached_header_and_row_play_wait_for_source_revalidation() {
    for row in [false, true] {
        let mut fixture = Fixture::new(false);
        let mut stream = open_stream(&mut fixture, "paged", 5);
        let cached = duplicate_tracks();
        stream.publish(&mut fixture, snapshot("cached", &cached, 5, 5));
        fixture.update(|window, cx| {
            if row {
                window.click(("spotify-track", 2usize), cx);
            } else {
                window.click("playlist-play", cx);
            }
        });
        let respond = stream.complete_request();
        fixture.update(|_, _| {});
        fixture.no_commands();
        stream.no_requests();

        let fresh = if row {
            cached
        } else {
            vec![track(80), track(81)]
        };
        let count = u32::try_from(fresh.len()).expect("fixture track count");
        let complete = snapshot(if row { "cached" } else { "fresh" }, &fresh, count, count);
        stream.publish(&mut fixture, complete.clone());
        fixture.no_commands();
        fixture.update(|window, _| {
            assert!(window.find("playlist-loading-status").visible());
        });
        respond.send(Ok(complete)).expect("live revalidation reply");
        fixture.update(|_, _| {});
        assert_playback(&mut fixture, &fresh, if row { 2 } else { 0 }, false);
        fixture.update(|window, _| {
            assert!(window.try_find("playlist-loading-status").is_none());
        });
        stream.no_requests();
    }
}

#[test]
fn a_later_row_click_supersedes_header_play_when_the_final_stream_precedes_its_reply() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 60);
    let tracks = duplicate_tracks();
    stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
    fixture.update(|window, cx| window.click("playlist-play", cx));
    let respond = stream.complete_request();
    let complete = snapshot("v1", &tracks, 60, 60);
    stream.publish(&mut fixture, complete.clone());
    fixture.no_commands();
    fixture.update(|window, _| {
        assert!(window.find("playlist-loading-status").visible());
    });
    fixture.update(|window, cx| window.click(("spotify-track", 2usize), cx));
    stream.no_requests();
    fixture.no_commands();
    respond.send(Ok(complete)).expect("live completion reply");
    fixture.update(|_, _| {});
    assert_playback(&mut fixture, &tracks, 2, false);
    fixture.update(|window, _| {
        assert!(window.try_find("playlist-loading-status").is_none());
    });
    stream.no_requests();
}

#[test]
fn a_completion_reply_cannot_play_before_its_final_snapshot_reaches_the_ui() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 60);
    let tracks = duplicate_tracks();
    stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
    fixture.update(|window, cx| window.click("playlist-play", cx));
    let respond = stream.complete_request();
    let complete = snapshot("v1", &tracks, 60, 60);
    respond
        .send(Ok(complete.clone()))
        .expect("live completion reply");
    fixture.update(|_, _| {});
    fixture.no_commands();
    stream.no_requests();

    stream.publish(&mut fixture, complete);
    assert_playback(&mut fixture, &tracks, 0, false);
    stream.no_requests();
}

#[test]
fn a_failed_page_keeps_its_prefix_and_retry_requests_only_one_more_page() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 100);
    let prefix = (0..50).map(track).collect::<Vec<_>>();
    stream.publish(&mut fixture, snapshot("v1", &prefix, 50, 100));
    scroll_to_tail(&mut fixture);
    stream.next_page_request();
    stream
        .updates
        .send(Err(anyhow::anyhow!("Spotify unavailable")))
        .expect("live stream");
    fixture.update(|_, _| {});
    fixture.update(|window, _| {
        assert!(window.find(("spotify-track", 49usize)).visible());
        assert!(window.find("playlist-retry-more").visible());
    });
    stream.no_requests();
    fixture.update(|window, cx| window.click("playlist-retry-more", cx));
    stream.next_page_request();
    stream.no_requests();
    let complete = (0..100).map(track).collect::<Vec<_>>();
    stream.publish(&mut fixture, snapshot("v1", &complete, 100, 100));
    fixture.update(|window, _| assert!(window.try_find("playlist-retry-more").is_none()));
    fixture.no_commands();
}

#[test]
fn a_complete_cached_playlist_retries_validation_without_discarding_rows_or_play_intent() {
    for pending_play in [false, true] {
        let mut fixture = Fixture::new(false);
        let mut stream = open_stream(&mut fixture, "paged", 5);
        let cached = duplicate_tracks();
        stream.publish(&mut fixture, snapshot("cached", &cached, 5, 5));
        let respond = if pending_play {
            fixture.update(|window, cx| window.click("playlist-shuffle", cx));
            Some(stream.complete_request())
        } else {
            None
        };
        stream
            .updates
            .send(Err(anyhow::anyhow!("Source validation failed")))
            .expect("live validation stream");
        if let Some(respond) = respond {
            respond
                .send(Err(anyhow::anyhow!("Source validation failed")))
                .expect("live failed completion reply");
        }
        fixture.update(|_, _| {});
        fixture.update(|window, _| {
            assert!(window.find(("spotify-track", 0usize)).visible());
            assert!(window.find("playlist-retry-more").visible());
        });
        fixture.no_commands();
        stream.no_requests();

        fixture.update(|window, cx| window.click("playlist-retry-more", cx));
        let respond = stream.complete_request();
        fixture.no_commands();
        stream.no_requests();
        let fresh = vec![track(80), track(81)];
        let complete = snapshot("fresh", &fresh, 2, 2);
        stream.publish(&mut fixture, complete.clone());
        fixture.no_commands();
        respond
            .send(Ok(complete))
            .expect("live retry completion reply");
        fixture.update(|_, _| {});
        if pending_play {
            assert_playback(&mut fixture, &fresh, 0, true);
        } else {
            fixture.no_commands();
        }
        fixture.update(|window, _| {
            assert!(window.find(("spotify-track", 0usize)).visible());
            assert!(window.try_find("playlist-retry-more").is_none());
        });
        stream.no_requests();
    }
}

#[test]
fn a_closed_complete_playlist_stream_offers_retry_and_restarts_source_validation() {
    let mut fixture = Fixture::new(false);
    let stream = fixture.open_complete_playlist("paged", duplicate_tracks());
    drop(stream);
    fixture.update(|_, _| {});
    fixture.update(|window, _| {
        assert!(window.find(("spotify-track", 0usize)).visible());
        assert!(window.find("playlist-retry-more").visible());
    });
    fixture.update(|window, cx| window.click("playlist-retry-more", cx));
    let BackendCommand::StreamPlaylist {
        playlist,
        requests,
        updates,
    } = fixture
        .backend
        .commands
        .try_recv()
        .expect("restarted playlist stream")
    else {
        panic!("retry must restart playlist source validation");
    };
    assert_eq!(playlist.source_id, "paged");
    let mut stream = PlaylistStream {
        requests,
        updates,
        latest: None,
    };
    let fresh = vec![track(80), track(81)];
    stream.publish(&mut fixture, snapshot("fresh", &fresh, 2, 2));
    fixture.no_commands();
    fixture.update(|window, cx| {
        assert!(window.try_find("playlist-retry-more").is_none());
        window.click("playlist-play", cx);
    });
    fixture.no_commands();
    stream.acknowledge_complete(&mut fixture);
    assert_playback(&mut fixture, &fresh, 0, false);
}

#[test]
fn route_playlist_and_account_changes_drop_the_previous_stream_and_pending_play() {
    for change in ["route", "playlist", "account"] {
        let mut fixture = Fixture::new(false);
        let mut stream = open_stream(&mut fixture, "paged", 60);
        let tracks = duplicate_tracks();
        stream.publish(&mut fixture, snapshot("v1", &tracks[..3], 50, 60));
        fixture.update(|window, cx| window.click("playlist-play", cx));
        let respond = stream.complete_request();
        let _replacement_stream = match change {
            "route" => {
                fixture.press(SECONDARY_K);
                None
            }
            "playlist" => fixture.open_playlist("replacement", vec![track(90)]),
            _ => {
                let workspace = fixture.workspace.clone().expect("workspace fixture");
                fixture.update(|window, cx| {
                    workspace
                        .upgrade()
                        .expect("live workspace")
                        .update(cx, |workspace, cx| {
                            workspace.clear_account_data(window, cx);
                        });
                });
                None
            }
        };
        assert!(
            stream.requests.is_closed(),
            "old page requests remain live after {change}"
        );
        assert!(
            stream.updates.is_closed(),
            "old stream remains live after {change}"
        );
        assert!(
            respond.is_closed(),
            "pending play remains live after {change}"
        );
        fixture.no_commands();
    }
}

#[test]
fn a_new_playlist_revision_cancels_a_pending_play_of_an_old_row() {
    let mut fixture = Fixture::new(false);
    let mut stream = open_stream(&mut fixture, "paged", 60);
    let old_tracks = duplicate_tracks();
    stream.publish(&mut fixture, snapshot("old", &old_tracks[..3], 50, 60));
    fixture.update(|window, cx| window.click(("spotify-track", 2usize), cx));
    let respond = stream.complete_request();
    let replacement = vec![track(80), track(81)];
    stream.publish(&mut fixture, snapshot("new", &replacement, 2, 2));
    let _ = respond.send(Ok(snapshot("old", &old_tracks, 60, 60)));
    fixture.update(|_, _| {});
    fixture.no_commands();
    fixture.update(|window, cx| window.click("playlist-play", cx));
    stream.acknowledge_complete(&mut fixture);
    assert_playback(&mut fixture, &replacement, 0, false);
    stream.no_requests();
}
