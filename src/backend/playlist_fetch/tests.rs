use super::*;
use crate::model::Provider;
use crate::spotify::tests::{SpotifyApi, catalog_track};
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn profile(id: &str) -> Value {
    json!({"id": id, "display_name": "Shared name", "external_urls": {}, "images": [], "href": "http://example/me"})
}

fn page(offset: u32, total: u32, prefix: &str) -> Value {
    let count = total.saturating_sub(offset).min(crate::playlist::PAGE_SIZE);
    let items: Vec<_> = (offset..offset + count).map(|index| json!({
        "is_local": false, "added_at": "2026-01-01T00:00:00Z", "item": catalog_track(&format!("{prefix}{index}"), None)
    })).collect();
    json!({"href":"http://example/items", "limit":50, "offset":offset, "previous":null,
        "next": if offset + count < total { Some("http://example/next") } else { None }, "total":total, "items":items})
}

fn responses(total: u32) -> Vec<(&'static str, u16, Value)> {
    vec![
        ("/me/", 200, profile("account-one")),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example/items?offset=0",
            200,
            page(0, total, "a"),
        ),
        (
            "/playlists/example/items?offset=50",
            200,
            page(50, total, "a"),
        ),
        (
            "/playlists/example/items?offset=100",
            200,
            page(100, total, "a"),
        ),
    ]
}

struct Stream {
    worker: Worker,
    requests: Sender<PlaylistRequest>,
    updates: UnboundedReceiver<Result<PlaylistSnapshot>>,
}

impl Stream {
    async fn open(api: &SpotifyApi, store: BlockingStore) -> Self {
        let (events, _) = mpsc::unbounded_channel();
        let (unavailable, _) = mpsc::unbounded_channel();
        let (connected, _) = mpsc::unbounded_channel();
        let mut worker = Worker::new(
            Startup {
                store,
                spotify: api.spotify.clone(),
                configuration: None,
                playback_snapshot: None,
                playback_credentials_invalidated: false,
            },
            events,
            unavailable,
            connected,
        );
        let (requests, incoming) = mpsc::channel(CONTROL_CAPACITY);
        let (updates, received) = mpsc::unbounded_channel();
        let (_, mut shutdown) = tokio::sync::watch::channel(false);
        worker
            .handle_command(
                BackendCommand::StreamPlaylist {
                    playlist: Playlist {
                        provider: Provider::Spotify,
                        source_id: "example".into(),
                        name: "Example".into(),
                        owner: "Owner".into(),
                        track_count: 0,
                        artwork_url: None,
                    },
                    requests: incoming,
                    updates,
                },
                &mut shutdown,
            )
            .await;
        Self {
            worker,
            requests,
            updates: received,
        }
    }

    async fn next(&mut self) -> PlaylistSnapshot {
        tokio::time::timeout(Duration::from_secs(2), self.updates.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    async fn complete(&self) -> Result<PlaylistSnapshot> {
        let (respond, reply) = tokio::sync::oneshot::channel();
        self.requests
            .send(PlaylistRequest::Complete { respond })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), reply)
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.worker.catalog.abort_all();
    }
}

#[tokio::test]
async fn cold_open_and_next_only_fetch_one_raw_page_then_complete_finishes_the_queue() {
    let mut api = SpotifyApi::new(responses(123)).await;
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    let mut stream = Stream::open(&api, store.clone()).await;
    let first = stream.next().await;
    assert_eq!(
        (first.next_offset, first.tracks.len(), first.total),
        (50, 50, 123)
    );
    let requests: Vec<_> = std::iter::from_fn(|| api.requests.try_recv().ok()).collect();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("/playlists/example/items"))
            .count(),
        1
    );
    stream
        .requests
        .send(PlaylistRequest::NextPage)
        .await
        .unwrap();
    assert_eq!(stream.next().await.next_offset, 100);
    let full = stream.complete().await.unwrap();
    assert!(full.complete());
    assert_eq!(full.tracks.len(), 123);
    assert_eq!(full.tracks.last().unwrap().track.source_id, "a122");
    let published: Vec<_> = std::iter::from_fn(|| stream.updates.try_recv().ok()).collect();
    assert_eq!(
        published.last().unwrap().as_ref().unwrap(),
        &full,
        "full update precedes completion reply"
    );
    assert_eq!(
        store
            .call(|store| store.cached_playlist("account-one", "example"))
            .await
            .unwrap(),
        Some(full)
    );
}

#[tokio::test]
async fn retry_after_identity_failure_verifies_the_account_and_fetches_only_the_first_page() {
    let mut configured = responses(123);
    configured.insert(
        0,
        (
            "/me/",
            500,
            json!({"error":{"status":500,"message":"temporary"}}),
        ),
    );
    let api = SpotifyApi::new(configured).await;
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    let mut stream = Stream::open(&api, store.clone()).await;
    assert!(stream.updates.recv().await.unwrap().is_err());
    stream
        .requests
        .send(PlaylistRequest::NextPage)
        .await
        .unwrap();
    let first = stream.next().await;
    assert_eq!(first.next_offset, 50);
    assert_eq!(
        store
            .call(|store| store.cached_playlist("account-one", "example"))
            .await
            .unwrap(),
        Some(first)
    );
    assert_eq!(
        store
            .call(|store| store.cached_playlist("", "example"))
            .await
            .unwrap(),
        None
    );
}

async fn wait_for(api: &mut SpotifyApi, path: &str) {
    loop {
        let request = tokio::time::timeout(Duration::from_secs(2), api.requests.recv())
            .await
            .unwrap()
            .unwrap();
        if request.split('?').next().unwrap() == path {
            return;
        }
    }
}

#[tokio::test]
async fn cached_rows_precede_validation_and_unchanged_versions_skip_items() {
    let mut api = SpotifyApi::new(responses(2)).await;
    let cached_page = api
        .spotify
        .playlist_page("example", "version-a", 0)
        .await
        .unwrap();
    let cached = PlaylistSnapshot {
        revision: cached_page.revision,
        tracks: cached_page.tracks,
        next_offset: cached_page.next_offset,
        total: cached_page.total,
    };
    let saved = cached.clone();
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    store
        .call(move |store| store.replace_playlist_cache("account-one", "example", &saved))
        .await
        .unwrap();
    while api.requests.try_recv().is_ok() {}
    api.hold("/playlists/example");
    let mut stream = Stream::open(&api, store).await;
    assert_eq!(stream.next().await, cached);
    wait_for(&mut api, "/playlists/example").await;
    assert!(
        stream.updates.try_recv().is_err(),
        "cache appears while validation is still in flight"
    );
    api.release("/playlists/example");
    assert_eq!(stream.next().await, cached);
    assert_eq!(stream.complete().await.unwrap(), cached);
    assert_eq!(stream.updates.try_recv().unwrap().unwrap(), cached);
    assert!(
        std::iter::from_fn(|| api.requests.try_recv().ok())
            .all(|request| !request.starts_with("/playlists/example/items"))
    );
}

#[tokio::test]
async fn reopening_a_saved_prefix_fetches_only_the_requested_raw_tail() {
    let mut api = SpotifyApi::new(responses(70)).await;
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    let mut first_open = Stream::open(&api, store.clone()).await;
    let cached = first_open.next().await;
    drop(first_open);
    while api.requests.try_recv().is_ok() {}
    let mut reopened = Stream::open(&api, store).await;
    assert_eq!(reopened.next().await, cached);
    assert_eq!(reopened.next().await, cached);
    assert!(
        std::iter::from_fn(|| api.requests.try_recv().ok())
            .all(|request| !request.starts_with("/playlists/example/items"))
    );
    reopened
        .requests
        .send(PlaylistRequest::NextPage)
        .await
        .unwrap();
    let full = reopened.next().await;
    assert_eq!((full.next_offset, full.tracks.len()), (70, 70));
    let item_requests: Vec<_> = std::iter::from_fn(|| api.requests.try_recv().ok())
        .filter(|request| request.starts_with("/playlists/example/items"))
        .collect();
    assert_eq!(item_requests.len(), 1);
    let url = url::Url::parse(&format!("http://example{}", item_requests[0])).unwrap();
    assert!(
        url.query_pairs()
            .any(|(key, value)| key == "offset" && value == "50")
    );
}

#[tokio::test]
async fn failed_cached_validation_preserves_rows_and_retry_fetches_one_fresh_page() {
    let api = SpotifyApi::new(vec![
        ("/me/", 200, profile("account-one")),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example",
            500,
            json!({"error":{"status":500,"message":"temporary"}}),
        ),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-b"}),
        ),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "a")),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "b")),
    ])
    .await;
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    let mut first_open = Stream::open(&api, store.clone()).await;
    let cached = first_open.next().await;
    drop(first_open);
    let mut reopened = Stream::open(&api, store.clone()).await;
    assert_eq!(reopened.next().await, cached);
    assert!(reopened.updates.recv().await.unwrap().is_err());
    assert_eq!(
        store
            .call(|store| store.cached_playlist("account-one", "example"))
            .await
            .unwrap(),
        Some(cached)
    );
    reopened
        .requests
        .send(PlaylistRequest::NextPage)
        .await
        .unwrap();
    let fresh = reopened.next().await;
    assert_eq!(
        (fresh.revision.as_str(), fresh.next_offset),
        ("version-b", 50)
    );
    assert!(
        fresh
            .tracks
            .iter()
            .all(|listed| listed.track.source_id.starts_with('b'))
    );
}

#[tokio::test]
async fn a_changed_version_discards_the_inflight_page_and_restarts_before_publishing() {
    let api = SpotifyApi::new(vec![
        ("/me/", 200, profile("account-one")),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-b"}),
        ),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "a")),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "b")),
        ("/playlists/example/items?offset=50", 200, page(50, 70, "b")),
    ])
    .await;
    let store = BlockingStore::from_store(Store::in_memory().unwrap());
    let mut stream = Stream::open(&api, store).await;
    let first = stream.next().await;
    assert_eq!(first.revision, "version-b");
    assert!(
        first
            .tracks
            .iter()
            .all(|listed| listed.track.source_id.starts_with('b'))
    );
    stream
        .requests
        .send(PlaylistRequest::NextPage)
        .await
        .unwrap();
    let full = stream.next().await;
    assert_eq!(full.tracks.len(), 70);
    assert!(
        full.tracks
            .iter()
            .all(|listed| listed.track.source_id.starts_with('b'))
    );
    assert_eq!(
        stream
            .worker
            .store
            .call(|store| store.cached_playlist("account-one", "example"))
            .await
            .unwrap(),
        Some(full)
    );
}

#[tokio::test]
async fn complete_revalidates_a_previously_loaded_prefix_before_building_the_queue() {
    let api = SpotifyApi::new(vec![
        ("/me/", 200, profile("account-one")),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-a"}),
        ),
        (
            "/playlists/example",
            200,
            json!({"snapshot_id":"version-b"}),
        ),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "a")),
        ("/playlists/example/items?offset=0", 200, page(0, 70, "b")),
        ("/playlists/example/items?offset=50", 200, page(50, 70, "b")),
    ])
    .await;
    let mut stream =
        Stream::open(&api, BlockingStore::from_store(Store::in_memory().unwrap())).await;
    assert_eq!(stream.next().await.revision, "version-a");
    let full = stream.complete().await.unwrap();
    assert_eq!(
        (full.revision.as_str(), full.tracks.len()),
        ("version-b", 70)
    );
    assert!(
        full.tracks
            .iter()
            .all(|listed| listed.track.source_id.starts_with('b'))
    );
}

#[tokio::test]
async fn a_queued_cache_write_checks_the_account_generation_inside_the_storage_worker() {
    let api = SpotifyApi::new(responses(2)).await;
    let (jobs, incoming) = std::sync::mpsc::channel::<StoreJob>();
    let (queued, write_queued) = tokio::sync::oneshot::channel();
    let (clear, cleared) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut store = Store::in_memory().unwrap();
        incoming.recv().unwrap()(&mut store); // Initial cache read.
        let write = incoming.recv().unwrap();
        queued.send(()).unwrap();
        cleared.recv().unwrap();
        store.clear_library_cache().unwrap();
        write(&mut store);
        while let Ok(job) = incoming.recv() {
            job(&mut store);
        }
    });
    let store = BlockingStore { jobs };
    let mut stream = Stream::open(&api, store.clone()).await;
    tokio::time::timeout(Duration::from_secs(2), write_queued)
        .await
        .unwrap()
        .unwrap();
    stream.worker.catalog_generation.store(1, Ordering::Release);
    clear.send(()).unwrap();
    assert!(stream.updates.recv().await.unwrap().is_err());
    assert_eq!(
        store
            .call(|store| store.cached_playlist("account-one", "example"))
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn closing_either_stream_endpoint_cancels_the_owned_fetch() {
    for close_controls in [false, true] {
        let mut api = SpotifyApi::new(responses(2)).await;
        api.hold("/playlists/example");
        let mut stream =
            Stream::open(&api, BlockingStore::from_store(Store::in_memory().unwrap())).await;
        wait_for(&mut api, "/playlists/example").await;
        if close_controls {
            let (replacement, _) = mpsc::channel(1);
            drop(std::mem::replace(&mut stream.requests, replacement));
        } else {
            let (_, replacement) = mpsc::unbounded_channel();
            drop(std::mem::replace(&mut stream.updates, replacement));
        }
        let mut task = stream.worker.catalog.playlist.take().unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
        if finished.is_err() {
            task.abort();
        }
        finished.unwrap().unwrap();
        assert_eq!(
            stream
                .worker
                .store
                .call(|store| store.cached_playlist("account-one", "example"))
                .await
                .unwrap(),
            None
        );
    }
}

#[tokio::test]
async fn closing_controls_cancels_a_fetch_even_when_pending_controls_are_full() {
    let mut api = SpotifyApi::new(responses(2)).await;
    api.hold("/playlists/example");
    let mut stream =
        Stream::open(&api, BlockingStore::from_store(Store::in_memory().unwrap())).await;
    wait_for(&mut api, "/playlists/example").await;
    for _ in 0..CONTROL_CAPACITY * 2 {
        stream
            .requests
            .send(PlaylistRequest::NextPage)
            .await
            .unwrap();
    }
    tokio::task::yield_now().await;
    let (replacement, _) = mpsc::channel(1);
    drop(std::mem::replace(&mut stream.requests, replacement));
    let mut task = stream.worker.catalog.playlist.take().unwrap();
    let finished = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
    if finished.is_err() {
        task.abort();
    }
    finished
        .expect("dropping controls cancels a saturated actor")
        .unwrap();
}
