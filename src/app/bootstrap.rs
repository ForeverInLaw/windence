use std::borrow::Cow;

use super::*;

pub(super) fn run() {
    let _ = env_logger::try_init();
    let lifecycle = match InstanceLifecycle::acquire().expect("could not initialize app lifecycle")
    {
        Instance::Primary(lifecycle) => lifecycle,
        Instance::Secondary => return,
    };
    let preferences_store = Store::open_default().ok();
    let preferences = preferences_store
        .as_ref()
        .and_then(|store| store.preferences().ok())
        .unwrap_or_default();
    let credentials_expected = preferences_store
        .as_ref()
        .is_some_and(stored_credentials_expected);
    let app = gpui_kit::application().with_assets(assets::AppAssets);
    // Clicking the Dock icon with no window open puts one back over the
    // services that kept playing in the meantime.
    app.on_reopen(|cx| {
        // AppKit can deliver this during launch, before the services exist.
        if cx.has_global::<services::AppServices>() {
            windows::show_app_window(cx);
        }
    });
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        cx.text_system()
            .add_fonts(
                assets::FONT_FILES
                    .iter()
                    .map(|bytes| Cow::Borrowed(*bytes))
                    .collect(),
            )
            .expect("could not register the bundled fonts");
        cx.set_http_client(Arc::new(
            http::ImageHttpClient::new().expect("could not configure image HTTP client"),
        ));
        cx.on_action(|_: &Quit, cx| cx.quit());
        services::AppServices::init(cx, lifecycle, preferences_store, preferences);
        bind_keys(cx);
        // Without a menu bar, Cmd+Q is only deliverable through a window, so
        // closing the last one would leave no way to quit.
        cx.set_menus(vec![
            gpui_kit::Menu {
                name: "Cadence".into(),
                items: vec![gpui_kit::MenuItem::action("Quit Cadence", Quit)],
                disabled: false,
            },
            gpui_kit::Menu {
                name: "Edit".into(),
                items: vec![
                    gpui_kit::MenuItem::os_action("Cut", NoOp, gpui_kit::OsAction::Cut),
                    gpui_kit::MenuItem::os_action("Copy", NoOp, gpui_kit::OsAction::Copy),
                    gpui_kit::MenuItem::os_action("Paste", NoOp, gpui_kit::OsAction::Paste),
                    gpui_kit::MenuItem::os_action(
                        "Select All",
                        NoOp,
                        gpui_kit::OsAction::SelectAll,
                    ),
                ],
                disabled: false,
            },
            gpui_kit::Menu {
                name: "Window".into(),
                items: vec![gpui_kit::MenuItem::action("Close Window", CloseWindow)],
                disabled: false,
            },
        ]);
        watch_for_activations(cx);
        windows::open_initial_window(credentials_expected, cx);
        cx.activate(true);
    });
}

/// Whether the store says a signed-in session should come straight up: a
/// client id is configured and the OAuth credentials were not invalidated.
/// The backend has the final word; a wrong guess swaps the windows.
fn stored_credentials_expected(store: &Store) -> bool {
    let configured = std::env::var("SPOTIFY_CLIENT_ID").is_ok()
        || matches!(store.spotify_client_id(), Ok(Some(_)));
    configured
        && !store
            .spotify_oauth_credentials_invalidated()
            .unwrap_or(true)
}

/// Brings Cadence forward when another launch asks this instance to show
/// itself, opening a window again if the last one was closed.
fn watch_for_activations(cx: &mut App) {
    let activations = services::AppServices::activations(cx);
    cx.spawn(async move |cx| {
        while activations.recv().await.is_ok() {
            cx.update(|cx| {
                cx.activate(true);
                windows::show_app_window(cx);
            });
        }
    })
    .detach();
}

/// Binds the app's global keys, shared by the real launch and the UI tests.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys(app_key_bindings());
}

/// The app's global key bindings. Modifier shortcuts use the "secondary"
/// prefix: cmd on macOS, ctrl elsewhere. Writing "cmd" outright would demand
/// the Windows key on Windows, where gpui maps the platform modifier to Win.
fn app_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", Tab, None),
        KeyBinding::new("shift-tab", TabPrev, None),
        KeyBinding::new("secondary-k", OpenSearch, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("escape", DismissOverlay, Some("Cadence")),
        playback_key_binding(),
        seek_back_key_binding(),
        seek_forward_key_binding(),
        seek_start_key_binding(),
        seek_end_key_binding(),
        volume_up_key_binding(),
        volume_down_key_binding(),
        volume_mute_key_binding(),
    ]
}

fn playback_key_binding() -> KeyBinding {
    KeyBinding::new("space", TogglePlayback, Some("Cadence && !Input"))
}

// The slider keys sit on the focused slider element, one context below the
// app's "Cadence" root. The `>` matches that shape, so the keys stay dead
// while focus is anywhere else — an input, a menu, or the root itself.
fn seek_back_key_binding() -> KeyBinding {
    KeyBinding::new("left", SeekBack, Some("Cadence > ProgressSlider"))
}

fn seek_forward_key_binding() -> KeyBinding {
    KeyBinding::new("right", SeekForward, Some("Cadence > ProgressSlider"))
}

fn seek_start_key_binding() -> KeyBinding {
    KeyBinding::new("home", SeekStart, Some("Cadence > ProgressSlider"))
}

fn seek_end_key_binding() -> KeyBinding {
    KeyBinding::new("end", SeekEnd, Some("Cadence > ProgressSlider"))
}

fn volume_up_key_binding() -> KeyBinding {
    KeyBinding::new("up", VolumeUp, Some("Cadence > VolumeSlider"))
}

fn volume_down_key_binding() -> KeyBinding {
    KeyBinding::new("down", VolumeDown, Some("Cadence > VolumeSlider"))
}

fn volume_mute_key_binding() -> KeyBinding {
    KeyBinding::new("m", VolumeMute, Some("Cadence > VolumeSlider"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_toggles_playback_except_in_text_inputs() {
        let keymap = gpui_kit::Keymap::new(vec![playback_key_binding()]);
        let space = gpui_kit::Keystroke::parse("space").unwrap();
        let cadence = gpui_kit::KeyContext::try_from("Cadence").unwrap();
        let input = gpui_kit::KeyContext::try_from("Input").unwrap();

        let (bindings, _) =
            keymap.bindings_for_input(std::slice::from_ref(&space), std::slice::from_ref(&cadence));
        assert_eq!(bindings.len(), 1);

        let (bindings, _) =
            keymap.bindings_for_input(std::slice::from_ref(&space), &[cadence, input]);
        assert!(bindings.is_empty());
    }

    #[test]
    fn modifier_shortcuts_match_the_platform_secondary_key() {
        let keymap = gpui_kit::Keymap::new(app_key_bindings());
        let cadence = gpui_kit::KeyContext::try_from("Cadence").unwrap();

        #[cfg(target_os = "macos")]
        let (matching, not_matching) = (["secondary-k", "cmd-k"], ["ctrl-k", "alt-k"]);
        #[cfg(not(target_os = "macos"))]
        let (matching, not_matching) = (["secondary-k", "ctrl-k"], ["cmd-k", "alt-k"]);

        for source in matching {
            let keystroke = gpui_kit::Keystroke::parse(source).unwrap();
            let (bindings, _) = keymap.bindings_for_input(
                std::slice::from_ref(&keystroke),
                std::slice::from_ref(&cadence),
            );
            assert_eq!(bindings.len(), 1, "{source} must open search");
        }
        for source in not_matching {
            let keystroke = gpui_kit::Keystroke::parse(source).unwrap();
            let (bindings, _) = keymap.bindings_for_input(
                std::slice::from_ref(&keystroke),
                std::slice::from_ref(&cadence),
            );
            assert!(bindings.is_empty(), "{source} must not open search");
        }
    }

    #[test]
    fn progress_keys_match_only_under_the_focused_progress_slider() {
        let keymap = gpui_kit::Keymap::new(app_key_bindings());
        let cadence = gpui_kit::KeyContext::try_from("Cadence").unwrap();
        let progress = gpui_kit::KeyContext::try_from("ProgressSlider").unwrap();
        let volume = gpui_kit::KeyContext::try_from("VolumeSlider").unwrap();
        let input = gpui_kit::KeyContext::try_from("Input").unwrap();

        // The slider's context sits one element below the app root.
        let stack = vec![cadence.clone(), progress.clone()];
        for key in ["left", "right", "home", "end"] {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
            assert_eq!(bindings.len(), 1, "{key} must seek on the progress slider");
        }
        // Up and Down belong to the volume slider, not the progress slider.
        for key in ["up", "down"] {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
            assert!(
                bindings.is_empty(),
                "{key} must not act on the progress slider"
            );
        }

        // Focus anywhere else — the volume slider, a text input, or the bare
        // root — leaves every progress key dead.
        for stack in [
            vec![cadence.clone(), volume.clone()],
            vec![cadence.clone(), input.clone()],
            vec![cadence.clone()],
        ] {
            for key in ["left", "right", "home", "end"] {
                let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
                let (bindings, _) =
                    keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
                assert!(bindings.is_empty(), "{key} must stay dead off the slider");
            }
        }
    }

    #[test]
    fn volume_keys_match_only_under_the_focused_volume_slider() {
        let keymap = gpui_kit::Keymap::new(app_key_bindings());
        let cadence = gpui_kit::KeyContext::try_from("Cadence").unwrap();
        let progress = gpui_kit::KeyContext::try_from("ProgressSlider").unwrap();
        let volume = gpui_kit::KeyContext::try_from("VolumeSlider").unwrap();
        let input = gpui_kit::KeyContext::try_from("Input").unwrap();

        let stack = vec![cadence.clone(), volume.clone()];
        for key in ["up", "down", "m"] {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
            assert_eq!(bindings.len(), 1, "{key} must step or mute the volume");
        }
        // Left and Right belong to the progress slider, not the volume slider.
        for key in ["left", "right"] {
            let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
            let (bindings, _) = keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
            assert!(
                bindings.is_empty(),
                "{key} must not act on the volume slider"
            );
        }

        for stack in [
            vec![cadence.clone(), progress.clone()],
            vec![cadence.clone(), input.clone()],
            vec![cadence.clone()],
        ] {
            for key in ["up", "down", "m"] {
                let keystroke = gpui_kit::Keystroke::parse(key).unwrap();
                let (bindings, _) =
                    keymap.bindings_for_input(std::slice::from_ref(&keystroke), &stack);
                assert!(bindings.is_empty(), "{key} must stay dead off the slider");
            }
        }
    }
}
