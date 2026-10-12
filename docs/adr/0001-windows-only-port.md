# Windows-only port; macOS code retained under cfg

Cadence upstream is macOS-only. We are porting the player to run natively on Windows and treating Windows as the only supported target — but instead of deleting macOS-specific code, we keep it compiled out behind `#[cfg(target_os = "macos")]`. Platform-specific dependencies (objc, gpui_platform features) move into `[target.'cfg(...)'.dependencies]` sections.

Why: the port rides on an actively developed upstream, and most valuable changes arrive as source diffs, so cheap upstream merges beat a clean tree. Deleted macOS code would turn every `git merge upstream/main` into conflict triage; cfg-gated dead code costs almost nothing.

## Considered Options

- **Hard delete of macOS code** — cleaner tree, rejected: makes upstream merges expensive, contradicting the reason we forked with an upstream remote rather than starting fresh.
- **Dual-platform build** — rejected: maintaining macOS support without macOS hardware/users is unpaid work on someone else's product.

## Consequences

- `git merge upstream/main` stays routine; conflicts should be rare and small.
- Dead macOS paths exist in `src/app/windows.rs` and friends; do not "fix" or remove them while porting — that is deliberate.
- SF Symbols do not exist on Windows, and `gpui-symbols` (the crate rendering them) does not compile off macOS. Its usage is replaced by gpui-component's icon set; the dependency is dropped.
- The UI stack now uses `gpui-kit 0.7.3` and its exact `gpui-pre 0.3.9` snapshot, based on Zed `93d7456`. `Cargo.lock` records the matching Base, Component, assets and platform packages. Keep the default backend; the optional GPUI Fast backend is not enabled. The [0.7.3 release](https://github.com/longbridge/gpui-kit/releases/tag/v0.7.3) replaces yanked 0.7.2. Future snapshot upgrades require locked Windows build, clippy, portable tests, packaging and the [native acceptance checklist](../windows-validation.md).
- Production windows use `gpui_kit::open_window`. The builder returns application content; the kit creates the single Base Root and installs component overlays. Windows options, chrome hit areas, HWND attachment for SMTC and cfg-gated AppKit modality remain app responsibilities. Bootstrap registers bundled Inter and selects it with `Theme::update` before opening any window. `Theme::change` still selects light/dark mode.
- Audio first stayed on SDL2 (bundled, static-linked), with the rule that only the output layer in `src/audio.rs` may ever be replaced; librespot and the rest of the playback path stay. That replacement has happened (issue #22): audio now plays through CPAL (WASAPI on Windows), following upstream's own swap in infomiho/cadence@bcbacf5. Why: SDL2 pinned the stream to the device it opened on, so a Bluetooth headset connecting or the default device changing needed a restart, and its bundled build made CMake a build requirement for no other reason. The output layer resamples to the device's native rate (rubato), hands samples to the device callback through a ring buffer, and reopens the default device when the stream fails or the default moves, within a time limit so a device that never returns surfaces an error. The DJ narration queue rides the same sink, as before.
