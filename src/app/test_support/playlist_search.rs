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
        self.update(|_, cx| {
            workspace
                .upgrade()
                .expect("live workspace")
                .update(cx, |workspace, cx| {
                    workspace.load_playlist(playlist, cx);
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
