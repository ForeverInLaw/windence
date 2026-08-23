# Cadence

A minimal Spotify player, forked from [infomiho/cadence](https://github.com/infomiho/cadence) and ported to Windows as its only target platform.

## Language

**Port**:
This repository — Cadence rebuilt for Windows as the single supported platform. The macOS original is called the upstream.
_Avoid_: fork (for the repo itself), windence (informal)

**Upstream**:
The original macOS-only project at infomiho/cadence, kept as a merge source.
_Avoid_: origin (reserved for this port's GitHub remote)

**Milestone 1**:
The first end-to-end state: window opens, Spotify OAuth completes, library/search/browse work, system media controls respond — with audio playback deliberately still absent (that is Milestone 2).
_Avoid_: MVP, tracer

**SMTC**:
Windows System Media Transport Controls — the OS media overlay that displays track metadata and issues play/pause/skip commands to the player.
_Avoid_: media keys, MPRIS (Linux term), Now Playing (macOS term)

**Milestone 2**:
The second end-to-end state: tracks play audibly through the upstream librespot + SDL2 audio stack, with playback behavior matching upstream.
_Avoid_: audio work, sound milestone
