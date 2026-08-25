<p align="center">
  <img src="assets/cadence-mark.svg" width="96" height="96" alt="Cadence logo">
</p>

<h1 align="center">Cadence</h1>

<p align="center">
  A minimal Spotify player for Windows.<br>
  Native and responsive, built with Rust and GPUI.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-black.svg" alt="MIT license"></a>
</p>

![Cadence showing the Liked Songs library](assets/cadence.webp)

This repository is the Windows-only Port of
[infomiho/cadence](https://github.com/infomiho/cadence), a minimal Spotify
player for macOS. There are no installers yet; you build and run from source.

## Status

The Port plays sound through the bundled librespot and SDL2 stack.
Everything around it works too:

- Sign in with Spotify through your default browser
- Browse your saved tracks in a scrollable library view
- Search tracks and playlists; native artist and album pages
- Queue, history, and Liked Songs visible in the UI
- Control playback from Windows system media controls (SMTC)
- Light, dark, and follow-system appearances
- Playback position and library state survive restarts

## Built With

**Rust** + **GPUI** + **rspotify** + **SQLite**

GPUI renders the GPU-accelerated interface, rspotify connects to the Spotify
Web API, and SQLite keeps local state. Audio streams through librespot into a
bundled, statically linked SDL2 output.

## Requirements

Building needs a 64-bit Windows 10 or 11 machine with:

- [Rust](https://rustup.rs) — `rustup` reads `rust-toolchain.toml` and
  installs the pinned toolchain by itself.
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/)
  with the **Desktop development with C++** workload — provides the MSVC
  compiler and the Windows SDK.
- [CMake](https://cmake.org/download/) — builds the bundled SDL2 from source.

NASM is **not** needed. Assembly code arrives preassembled inside the
`ring` crate, and SDL2 has no assembly of its own.

You also need a Spotify Premium account.

## Set Up Spotify

On first launch, Cadence guides you through these steps inside the app.
Client IDs are public; Cadence never asks for your client secret.

1. Create an app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard).
2. Add `http://127.0.0.1:8888/callback` as a redirect URI.
3. Copy the Client ID from **Basic Information** and enter it in Cadence.

Cadence keeps the Client ID and app preferences in its local SQLite database,
and login and playback tokens in Windows Credential Manager. No secret is
ever written to a plaintext file.

## Run From Source

```sh
cargo run
```

During development you can skip the setup screen by setting the
`SPOTIFY_CLIENT_ID` environment variable at launch:

```powershell
$env:SPOTIFY_CLIENT_ID = "your-client-id"; cargo run
```

## About Upstream

The macOS original lives at
[infomiho/cadence](https://github.com/infomiho/cadence); its README covers
macOS builds and releases. The macOS sources stay in this repository, but
compile-time checks exclude them from Windows builds. That keeps merges from
upstream routine. The packaging scripts under `scripts/` are macOS-only and
do not run on Windows. The logo attribution is in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## License

[MIT](LICENSE)
