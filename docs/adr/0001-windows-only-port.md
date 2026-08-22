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
- The pinned Zed/gpui revs are kept for now (see Cargo.toml pin comments); bumping the pin is deferred until the port builds and runs, since pin moves are delicate by design.
- Audio stays on SDL2 (bundled, static-linked) rather than switching to WASAPI/cpal — revisit only if SDL2 proves problematic on Windows.
