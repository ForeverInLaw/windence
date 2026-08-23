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
The first end-to-end state: window opens, Spotify OAuth completes, library/search/browse work, system media controls respond — with audio playback deliberately deferred to Milestone 2.
_Avoid_: MVP, tracer

**SMTC**:
Windows System Media Transport Controls — the OS media overlay that displays track metadata and issues play/pause/skip commands to the player.
_Avoid_: media keys, MPRIS (Linux term), Now Playing (macOS term)

**Milestone 2**:
The second end-to-end state: tracks play audibly through the upstream librespot + SDL2 audio stack, with playback behavior matching upstream.
_Avoid_: audio work, sound milestone

**Queue**:
The running play sequence: the track playing now plus every track after it. It starts as a copy of the context, then grows through Play next, Add to queue, Autoplay, and Smart Shuffle. It is not the playlist — the playlist is the saved Spotify object and playback never edits it.
_Avoid_: playlist, context

**Context**:
The set of tracks playback was started from: a playlist, an album, the liked-songs view, or a radio seeded on one track. The queue begins as the context's tracks and drifts from it as tracks are inserted or appended.
_Avoid_: queue, playlist

**Injected Track**:
A queued track that is not part of the played context. Only Smart Shuffle and Autoplay place these. They carry a distinct icon in the queue UI and are never written back into any playlist.
_Avoid_: suggestion, bonus track

**Shuffle**:
Randomised play order for the queue. Switching it on keeps the playing track in place and shuffles what follows; switching it off restores the original context order.
_Avoid_: random, mix, Smart Shuffle (adds injections)

**Anchor**:
A queued track pinned to its slot because it arrived after the context did: Play next inserts, Add-to-queue appends, Autoplay extensions. Shuffling and unshuffling reorder context tracks around anchors and never move one.
_Avoid_: Injected Track (says why a track is queued, not where it sits)

**Smart Shuffle**:
Shuffle plus injection: about one injected track between every three context tracks, fetched from Spotify recommendations. Switching it off removes the injected tracks and restores the original order.
_Avoid_: Autoplay (works only at the end of the queue), Radio (replaces the whole queue)

**Radio**:
A context built entirely from Spotify recommendations seeded on one track. Starting it replaces the queue instead of extending it.
_Avoid_: Autoplay, Smart Shuffle

**Autoplay**:
A preference-gated behaviour that appends recommended tracks when the queue reaches its end.
_Avoid_: Smart Shuffle (injects while the queue still runs), Radio

**DJ X**:
Spotify's AI-curated personal lineup, served as one Spotify-owned playlist whose tracks the server keeps rewriting for the listening account. Cadence treats it as an ordinary context: starting it snapshots the current lineup into the queue. Voice commentary and spoken requests are part of Spotify's own client, not of the context.
_Avoid_: DJX, AI DJ, mix

**Date Added**:
When a track entered the context it is listed in: a playlist's own add history, or the moment it was liked. It belongs to the listing, not to the track — the same track carries different dates in different contexts. Shown as a relative age during the first month, as an absolute date after that.
_Avoid_: release date (the album's), liked at

**Default Order**:
The track order Spotify reports for a context: playlists keep their curated sequence, liked songs their like sequence. Every sort view cycles back to it, and playback never edits it.
_Avoid_: original position, custom order
