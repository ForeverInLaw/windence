use super::*;
use crate::playlist::{PlaylistPage, PlaylistSnapshot};

pub(super) struct Context {
    pub spotify: Spotify,
    pub playback: Option<Playback>,
    pub playlist: Playlist,
    pub store: BlockingStore,
    pub generation: u64,
    pub current_generation: Arc<AtomicU64>,
}

pub(super) fn start(
    context: Context,
    mut requests: Receiver<PlaylistRequest>,
    updates: UnboundedSender<Result<PlaylistSnapshot>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut loader = Loader {
            context,
            account: String::new(),
            snapshot: None,
            revision: None,
            curated: None,
            initialized: false,
            validated: false,
        };
        let mut pending = VecDeque::new();
        if let Some(Err(error)) =
            run_while_open(loader.open(&updates), &mut requests, &updates, &mut pending).await
        {
            let _ = updates.send(Err(error));
        }
        loop {
            let request = if let Some(request) = pending.pop_front() {
                Some(request)
            } else {
                tokio::select! {
                    _ = updates.closed() => return,
                    request = requests.recv() => request,
                }
            };
            let Some(request) = request else { return };
            match request {
                PlaylistRequest::NextPage => {
                    let result = run_while_open(
                        loader.more(&updates),
                        &mut requests,
                        &updates,
                        &mut pending,
                    )
                    .await;
                    if let Some(Err(error)) = result {
                        let _ = updates.send(Err(error));
                    }
                    if requests.is_closed() || updates.is_closed() {
                        return;
                    }
                }
                PlaylistRequest::Complete { respond } => {
                    let result = run_while_open(
                        loader.complete(&updates),
                        &mut requests,
                        &updates,
                        &mut pending,
                    )
                    .await;
                    if let Some(result) = result {
                        let _ = respond.send(result);
                    } else {
                        return;
                    }
                }
            }
        }
    })
}

/// Keep controls bounded while a page request is in flight. Closing either
/// endpoint cancels that work; no detached fetch can refill a cleared cache.
async fn run_while_open<T>(
    work: impl std::future::Future<Output = Result<T>>,
    requests: &mut Receiver<PlaylistRequest>,
    updates: &UnboundedSender<Result<PlaylistSnapshot>>,
    pending: &mut VecDeque<PlaylistRequest>,
) -> Option<Result<T>> {
    tokio::pin!(work);
    loop {
        if requests.is_closed() || updates.is_closed() {
            return None;
        }
        tokio::select! {
            _ = updates.closed() => return None,
            result = &mut work => return Some(result),
            request = requests.recv() => match request {
                Some(request) if pending.len() < CONTROL_CAPACITY => pending.push_back(request),
                Some(PlaylistRequest::Complete { respond }) => {
                    let _ = respond.send(Err(anyhow!("Too many playlist requests are waiting")));
                }
                Some(PlaylistRequest::NextPage) => {
                    let _ = updates.send(Err(anyhow!("Too many playlist requests are waiting")));
                }
                None => return None,
            },
        }
    }
}

struct Loader {
    context: Context,
    account: String,
    snapshot: Option<PlaylistSnapshot>,
    revision: Option<String>,
    curated: Option<crate::playback::CuratedPlaylist>,
    initialized: bool,
    validated: bool,
}

impl Loader {
    async fn open(&mut self, updates: &UnboundedSender<Result<PlaylistSnapshot>>) -> Result<()> {
        if dj::matches(&self.context.playlist.source_id) {
            bail!("The live DJ playlist does not support cached pages");
        }
        if self.context.playlist.provider != crate::model::Provider::Spotify {
            bail!("Playlist paging is only available for Spotify");
        }
        self.account = tokio::time::timeout(
            Duration::from_secs(CATALOG_TIMEOUT_SECONDS),
            self.context.spotify.account_id(),
        )
        .await
        .context("Spotify account request timed out")??;
        if self.context.playlist.spotify_curated()
            && let Some(playback) = &self.context.playback
            && playback.account_id() != self.account
        {
            bail!("Sign in to the same Spotify account for the library and playback");
        }
        let account = self.account.clone();
        let source = self.context.playlist.source_id.clone();
        let generation = self.context.generation;
        let current = self.context.current_generation.clone();
        self.snapshot = self
            .context
            .store
            .call(move |store| {
                if current.load(Ordering::Acquire) != generation {
                    bail!("Spotify account changed while loading");
                }
                store.cached_playlist(&account, &source)
            })
            .await?;
        self.initialized = true;
        if let Some(snapshot) = &self.snapshot {
            let _ = updates.send(Ok(snapshot.clone()));
        }
        self.validate(updates).await
    }

    async fn source_revision(&mut self) -> Result<String> {
        tokio::time::timeout(Duration::from_secs(CATALOG_TIMEOUT_SECONDS), async {
            if self.context.playlist.spotify_curated() {
                let listing = self
                    .context
                    .playback
                    .as_ref()
                    .context("Spotify playback is not connected")?
                    .curated_playlist(&self.context.playlist.source_id)
                    .await?;
                let revision = listing.revision.clone();
                self.curated = Some(listing);
                Ok(revision)
            } else {
                self.context
                    .spotify
                    .playlist_revision(&self.context.playlist.source_id)
                    .await
            }
        })
        .await
        .context("Spotify playlist version request timed out")?
    }

    async fn validate(
        &mut self,
        updates: &UnboundedSender<Result<PlaylistSnapshot>>,
    ) -> Result<()> {
        self.validated = false;
        self.revision = Some(self.source_revision().await?);
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| Some(&snapshot.revision) == self.revision.as_ref())
        {
            self.validated = true;
            let _ = updates.send(Ok(self.snapshot.as_ref().unwrap().clone()));
            return Ok(());
        }
        self.advance(updates).await
    }

    async fn more(&mut self, updates: &UnboundedSender<Result<PlaylistSnapshot>>) -> Result<()> {
        if !self.initialized {
            return self.open(updates).await;
        }
        if !self.validated {
            return self.validate(updates).await;
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.complete())
        {
            self.advance(updates).await?;
        } else if let Some(snapshot) = &self.snapshot {
            let _ = updates.send(Ok(snapshot.clone()));
        }
        Ok(())
    }

    async fn complete(
        &mut self,
        updates: &UnboundedSender<Result<PlaylistSnapshot>>,
    ) -> Result<PlaylistSnapshot> {
        if !self.initialized {
            self.open(updates).await?;
        } else {
            self.validate(updates).await?;
        }
        while self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.complete())
        {
            self.advance(updates).await?;
        }
        let snapshot = self
            .snapshot
            .clone()
            .context("Spotify playlist has not loaded")?;
        let _ = updates.send(Ok(snapshot.clone()));
        Ok(snapshot)
    }

    async fn advance(&mut self, updates: &UnboundedSender<Result<PlaylistSnapshot>>) -> Result<()> {
        for _ in 0..3 {
            let revision = self
                .revision
                .clone()
                .context("Spotify playlist version is not known")?;
            let (offset, append) = self
                .snapshot
                .as_ref()
                .filter(|snapshot| snapshot.revision == revision)
                .map_or((0, false), |snapshot| (snapshot.next_offset, true));
            let page = self.page(&revision, offset).await?;
            let after = self.source_revision().await?;
            if after != revision {
                self.revision = Some(after);
                self.validated = false;
                continue;
            }
            let mut snapshot = if let Some(prefix) = self
                .snapshot
                .as_ref()
                .filter(|snapshot| snapshot.revision == revision)
            {
                if prefix.total != page.total {
                    bail!("Spotify playlist size changed without a new version");
                }
                prefix.clone()
            } else {
                PlaylistSnapshot {
                    revision,
                    tracks: Vec::new(),
                    next_offset: 0,
                    total: page.total,
                }
            };
            snapshot.tracks.extend(page.tracks.iter().cloned());
            snapshot.next_offset = page.next_offset;
            self.persist(&snapshot, &page, append).await?;
            self.snapshot = Some(snapshot.clone());
            self.validated = true;
            let _ = updates.send(Ok(snapshot));
            return Ok(());
        }
        bail!("Spotify playlist kept changing while loading; try again")
    }

    async fn page(&self, revision: &str, offset: u32) -> Result<PlaylistPage> {
        let page = tokio::time::timeout(Duration::from_secs(CATALOG_TIMEOUT_SECONDS), async {
            if self.context.playlist.spotify_curated() {
                self.context
                    .playback
                    .as_ref()
                    .context("Spotify playback is not connected")?
                    .curated_playlist_page(
                        self.curated
                            .as_ref()
                            .context("Spotify playlist listing is unavailable")?,
                        offset,
                    )
                    .await
            } else {
                self.context
                    .spotify
                    .playlist_page(&self.context.playlist.source_id, revision, offset)
                    .await
            }
        })
        .await
        .context("Spotify playlist page request timed out")??;
        if page.revision != revision
            || page.offset != offset
            || page.next_offset < offset
            || page.next_offset > page.total
            || page.next_offset - offset > crate::playlist::PAGE_SIZE
            || (page.next_offset == offset && offset < page.total)
        {
            bail!("Spotify playlist page did not advance");
        }
        Ok(page)
    }

    async fn persist(
        &self,
        snapshot: &PlaylistSnapshot,
        page: &PlaylistPage,
        append: bool,
    ) -> Result<()> {
        let account = self.account.clone();
        let source = self.context.playlist.source_id.clone();
        let generation = self.context.generation;
        let current = self.context.current_generation.clone();
        let replacement = (!append).then(|| snapshot.clone());
        let page = page.clone();
        self.context
            .store
            .call(move |store| {
                if current.load(Ordering::Acquire) != generation {
                    bail!("Spotify account changed while loading");
                }
                if let Some(snapshot) = replacement {
                    store.replace_playlist_cache(&account, &source, &snapshot)?;
                } else {
                    if !store.append_playlist_cache(&account, &source, &page)? {
                        bail!("Spotify playlist cache changed while loading");
                    }
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests;
