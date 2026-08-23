use super::*;

/// The library's fallback refresh cadence. Window activation is the primary
/// trigger; this timer covers long stays in the app without a switch away.
const LIBRARY_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Services that outlive any single window.
///
/// Closing the Cadence window must not interrupt playback, so the backend is
/// owned here instead of by the view that happens to be on screen.
pub(super) struct AppServices {
    /// Taken during shutdown so the worker thread stops before the process exits.
    backend: Option<Backend>,
    player: Entity<player::Player>,
    session: Entity<session::Session>,
    library: Entity<library::Library>,
    image_cache: Entity<image_cache::BoundedImageCache>,
    brand_mark: Arc<gpui::Image>,
    /// The system media controls. Where they bind to a window (Windows),
    /// `media_controls_window` names it: when that window closes the binding
    /// dies with it, so both drop together and a reopened window re-attaches.
    media_controls: Option<media_controls::SystemMediaControls>,
    media_controls_window: Option<gpui::AnyWindowHandle>,
    /// The window currently showing these services, if one is open.
    root: Option<gpui::WeakEntity<Workspace>>,
    /// The open main and sign-in windows. Slots are cleared when gpui reports
    /// a window closed, so they are the authority on what is on screen.
    main_window: Option<gpui::AnyWindowHandle>,
    onboarding_window: Option<gpui::AnyWindowHandle>,
    /// The last session state the window sync acted on.
    last_connection_state: ConnectionState,
    /// Once the session has been ready, the main window is the listener's home
    /// and losing the session no longer tears it down.
    has_been_ready: bool,
    /// Drains backend events for the whole process, not just for a window.
    event_pump: Option<gpui::Task<()>>,
    lifecycle: Arc<InstanceLifecycle>,
    store: Option<Store>,
    /// The live preference values, so a window opened later starts from what
    /// the listener last chose rather than from what was on disk at launch.
    preferences: AppPreferences,
}

impl gpui::Global for AppServices {}

impl AppServices {
    pub(super) fn init(
        cx: &mut App,
        lifecycle: Arc<InstanceLifecycle>,
        store: Option<Store>,
        preferences: AppPreferences,
    ) -> BackendHandle {
        let (backend, events) = Backend::start();
        let handle = backend.handle();
        let player = cx.new(|_| player::Player::new(handle.clone()));
        let session = cx.new(|_| session::Session::new(handle.clone()));
        let library = cx.new(|_| library::Library::new(handle.clone()));
        let image_cache = image_cache::BoundedImageCache::new(cx);
        let brand_mark = Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            include_bytes!("../../assets/cadence-mark.png").to_vec(),
        ));
        let player_for_media = player.clone();
        // Keep the system's now-playing panel in step with the player.
        cx.observe(&player, |player, cx| {
            let mut services = cx.remove_global::<Self>();
            if let Some(controls) = services.media_controls.as_mut() {
                controls.sync(player.read(cx));
            }
            cx.set_global(services);
        })
        .detach();
        cx.on_app_quit(|cx| {
            Self::shutdown(cx);
            async {}
        })
        .detach();
        cx.on_window_closed(|cx, _| {
            Self::prune_closed_windows(cx);
            if cx.windows().is_empty() {
                // The window owned the live position; persist it while the
                // services keep playing without one.
                Self::player(cx).read(cx).save_position();
            }
        })
        .detach();
        cx.observe(&session, |_, cx| windows::sync_windows(cx))
            .detach();
        let media_controls = media_controls::SystemMediaControls::attach(player_for_media, cx);
        cx.set_global(Self {
            backend: Some(backend),
            player,
            session,
            library,
            image_cache,
            brand_mark,
            media_controls,
            media_controls_window: None,
            root: None,
            main_window: None,
            onboarding_window: None,
            last_connection_state: ConnectionState::Starting,
            has_been_ready: false,
            event_pump: None,
            lifecycle,
            store,
            preferences,
        });
        Self::pump(events, cx);
        Self::poll_library(cx);
        handle
    }

    pub(super) fn backend(cx: &App) -> BackendHandle {
        cx.global::<Self>()
            .backend
            .as_ref()
            .expect("services are shut down")
            .handle()
    }

    /// Playback outlives windows, so the player is owned here and shared by handle.
    pub(super) fn player(cx: &App) -> Entity<player::Player> {
        cx.global::<Self>().player.clone()
    }

    /// The signed-in account, which outlives any window showing it.
    pub(super) fn session(cx: &App) -> Entity<session::Session> {
        cx.global::<Self>().session.clone()
    }

    /// The listener's music, which outlives any window showing it.
    pub(super) fn library(cx: &App) -> Entity<library::Library> {
        cx.global::<Self>().library.clone()
    }

    /// The Cadence mark, drawn by both the sidebar and the setup screen.
    pub(super) fn brand_mark(cx: &App) -> Arc<gpui::Image> {
        cx.global::<Self>().brand_mark.clone()
    }

    /// Artwork is shared by every view, so the cache is not tied to one of them.
    pub(super) fn image_cache(cx: &App) -> Entity<image_cache::BoundedImageCache> {
        cx.global::<Self>().image_cache.clone()
    }

    /// Notifications that another launch of Cadence asked this instance to come
    /// to the front.
    pub(super) fn activations(cx: &App) -> async_channel::Receiver<()> {
        cx.global::<Self>().lifecycle.activation_receiver()
    }

    pub(super) fn preferences(cx: &App) -> AppPreferences {
        cx.global::<Self>().preferences
    }

    /// A list's persisted sort, when the store is available and the row
    /// reads back. Storage trouble degrades to the default order.
    pub(super) fn list_sort(list_key: &str, cx: &App) -> Option<model::ListSort> {
        let services = cx.global::<Self>();
        services
            .store
            .as_ref()
            .and_then(|store| store.list_sort(list_key).ok())
            .flatten()
    }

    /// Persists a list's sort, `None` for the default order. Failures log
    /// and keep the on-screen sort: the view works without the memory.
    pub(super) fn set_list_sort(list_key: &str, sort: Option<model::ListSort>, cx: &mut App) {
        let services = cx.global_mut::<Self>();
        if let Some(Err(error)) = services
            .store
            .as_mut()
            .map(|store| store.set_list_sort(list_key, sort))
        {
            log::error!("could not save the list sort: {error}");
        }
    }

    pub(super) fn set_theme_preference(
        preference: ThemePreference,
        cx: &mut App,
    ) -> Option<anyhow::Result<()>> {
        let services = cx.global_mut::<Self>();
        services.preferences.theme = preference;
        services
            .store
            .as_mut()
            .map(|store| store.set_theme_preference(preference))
    }

    pub(super) fn set_sidebar_collapsed(
        collapsed: bool,
        cx: &mut App,
    ) -> Option<anyhow::Result<()>> {
        let services = cx.global_mut::<Self>();
        services.preferences.sidebar_collapsed = collapsed;
        services
            .store
            .as_mut()
            .map(|store| store.set_sidebar_collapsed(collapsed))
    }

    pub(super) fn set_autoplay(autoplay: bool, cx: &mut App) -> Option<anyhow::Result<()>> {
        let services = cx.global_mut::<Self>();
        services.preferences.autoplay = autoplay;
        services
            .store
            .as_mut()
            .map(|store| store.set_autoplay(autoplay))
    }

    /// Revalidates the library on a timer, backstopping the activation
    /// trigger in the workspace. A revalidation is two head requests unless
    /// something actually changed, so the cadence is not the cost concern it
    /// was when every refresh walked the whole collection.
    fn poll_library(cx: &mut App) {
        let library = Self::library(cx);
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(LIBRARY_REFRESH_INTERVAL)
                    .await;
                library.update(cx, |library, cx| library.revalidate(cx));
            }
        })
        .detach();
    }

    /// Notes which window should receive the events the services do not consume.
    pub(super) fn set_root(root: gpui::WeakEntity<Workspace>, cx: &mut App) {
        cx.global_mut::<Self>().root = Some(root);
    }

    pub(super) fn main_window(cx: &App) -> Option<gpui::AnyWindowHandle> {
        cx.global::<Self>().main_window
    }

    pub(super) fn onboarding_window(cx: &App) -> Option<gpui::AnyWindowHandle> {
        cx.global::<Self>().onboarding_window
    }

    pub(super) fn set_main_window(handle: Option<gpui::AnyWindowHandle>, cx: &mut App) {
        cx.global_mut::<Self>().main_window = handle;
    }

    pub(super) fn set_onboarding_window(handle: Option<gpui::AnyWindowHandle>, cx: &mut App) {
        cx.global_mut::<Self>().onboarding_window = handle;
    }

    pub(super) fn take_main_window(cx: &mut App) -> Option<gpui::AnyWindowHandle> {
        cx.global_mut::<Self>().main_window.take()
    }

    pub(super) fn take_onboarding_window(cx: &mut App) -> Option<gpui::AnyWindowHandle> {
        cx.global_mut::<Self>().onboarding_window.take()
    }

    pub(super) fn has_been_ready(cx: &App) -> bool {
        cx.global::<Self>().has_been_ready
    }

    /// Records the session state, reporting whether it moved since the last
    /// call so the window sync acts on transitions only.
    pub(super) fn note_connection_state(state: ConnectionState, cx: &mut App) -> bool {
        let services = cx.global_mut::<Self>();
        if services.last_connection_state == state {
            return false;
        }
        services.last_connection_state = state;
        if state == ConnectionState::Ready {
            services.has_been_ready = true;
        }
        true
    }

    /// Binds the system media controls to a freshly opened main window on
    /// platforms that hang them off a window handle (Windows SMTC). A no-op
    /// once attached; where the controls attached without a window at startup
    /// (macOS) there is nothing to bind, so this is never called.
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    pub(super) fn attach_media_controls(handle: gpui::AnyWindowHandle, cx: &mut App) {
        if cx.global::<Self>().media_controls.is_some() {
            return;
        }
        #[cfg(target_os = "windows")]
        {
            let player = Self::player(cx);
            match handle.update(cx, |_, window, cx| {
                media_controls::SystemMediaControls::attach_to_window(player, window, cx)
            }) {
                Ok(Some(mut controls)) => {
                    // Playback runs on without a window, so state may have
                    // piled up while none existed; push it now rather than
                    // waiting for the next player tick.
                    controls.sync(Self::player(cx).read(cx));
                    let services = cx.global_mut::<Self>();
                    services.media_controls = Some(controls);
                    services.media_controls_window = Some(handle);
                }
                Ok(None) => log::warn!("system media controls unavailable"),
                Err(error) => log::warn!("system media controls unavailable: {error}"),
            }
        }
    }

    /// Drops window slots whose window gpui no longer lists as open.
    fn prune_closed_windows(cx: &mut App) {
        let open = cx.windows();
        let services = cx.global_mut::<Self>();
        if services
            .main_window
            .is_some_and(|handle| !open.contains(&handle))
        {
            services.main_window = None;
        }
        if services
            .onboarding_window
            .is_some_and(|handle| !open.contains(&handle))
        {
            services.onboarding_window = None;
        }
        // Windows SMTC lives inside its host window; once that is gone the
        // binding is dead, so drop it and let the next window open re-attach.
        if services
            .media_controls_window
            .is_some_and(|handle| !open.contains(&handle))
        {
            services.media_controls = None;
            services.media_controls_window = None;
        }
    }

    /// Drains backend events for the process, so playback keeps advancing even
    /// when no window is open to watch it.
    fn pump(mut events: BackendEvents, cx: &mut App) {
        let task = cx.spawn(async move |cx| {
            while let Some(batch) = receive_backend_event_batch(&mut events).await {
                cx.update(|cx| Self::dispatch(batch, cx));
            }
        });
        cx.global_mut::<Self>().event_pump = Some(task);
    }

    fn dispatch(events: Vec<BackendEvent>, cx: &mut App) {
        let (player, session, library, root) = {
            let services = cx.global::<Self>();
            (
                services.player.clone(),
                services.session.clone(),
                services.library.clone(),
                services.root.clone(),
            )
        };
        let mut unhandled = Vec::new();
        for event in events {
            let Some(event) =
                player.update(cx, |player, cx| player.handle_backend_event(event, cx))
            else {
                continue;
            };
            let Some(event) =
                session.update(cx, |session, cx| session.handle_backend_event(event, cx))
            else {
                continue;
            };
            let generation = session.read(cx).generation();
            let Some(event) = library.update(cx, |library, cx| {
                library.handle_backend_event(event, generation, cx)
            }) else {
                continue;
            };
            unhandled.push(event);
        }
        if unhandled.is_empty() {
            return;
        }
        if let Some(root) = root.and_then(|root| root.upgrade()) {
            root.update(cx, |root, cx| root.handle_backend_events(unhandled, cx));
        } else {
            log::warn!(
                "dropping {} backend events with no main window open",
                unhandled.len()
            );
        }
    }

    /// Restarts the worker after a fatal failure. Handles already handed out
    /// keep working: `Backend::restart` redirects them at the new worker.
    pub(super) fn restart(cx: &mut App) {
        let handle = Self::backend(cx);
        let (backend, events) = Backend::restart(&handle);
        let session = {
            let services = cx.global_mut::<Self>();
            services.backend = Some(backend);
            services.session.clone()
        };
        session.update(cx, |session, cx| session.restarted(cx));
        Self::pump(events, cx);
    }

    /// Saves the live position and stops the worker thread. The process exits
    /// straight after `applicationWillTerminate:`, so nothing else will.
    fn shutdown(cx: &mut App) {
        let position_ms = Self::player(cx).read(cx).position_snapshot();
        // Stop the worker before writing, both because its shutdown drops any
        // command still queued behind it, and so it cannot write a later
        // position over this one on its way out.
        let backend = cx.global_mut::<Self>().backend.take();
        drop(backend);
        let Some(position_ms) = position_ms else {
            return;
        };
        let saved = cx
            .global_mut::<Self>()
            .store
            .as_mut()
            .map(|store| store.update_playback_position(position_ms));
        if let Some(Err(error)) = saved {
            log::error!("could not save the playback position: {error}");
        }
    }
}
