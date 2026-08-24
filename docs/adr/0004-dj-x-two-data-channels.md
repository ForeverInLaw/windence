# DJ X has no fetchable lineup; only a live Connect session can serve one

## Status

This replaces the earlier revision of the same ADR, which assumed the DJ X
lineup was fetchable through the playback session's playlist endpoint. That
premise is falsified by direct experiment (below).

## Context

DJ X is Spotify's AI-curated personal station. Since November 2024 the Web API
404s on every Spotify-owned playlist for development-mode apps, so Cadence
routed the lineup through the internal `spclient` protocol instead. In August
2026 that path returned an empty list for accounts where DJ X demonstrably
works in the official client, and this ADR records why: the DJ "playlist" is
not data, it is a server-driven session.

## Evidence

Live probes against a valid premium token whose official app plays DJ X fine.
All runs are reproducible with `cargo run --example dj_probe`.

| Channel | Result |
| --- | --- |
| Web API playlist endpoints | 404 (development-mode block) |
| `spclient` playlist4 (`get_playlist`) | HTTP OK, but `length=0`, `items=0` |
| `/context-resolve/v1/<dj-uri>` | one skeleton page: 0 tracks, no `page_url`, no `next_page_url`, url is inert `context://` |
| same resolve after registering a Connect device (dealer up, `NEW_DEVICE` state PUT accepted) | unchanged |
| same resolve after activating the device with player state naming the DJ context | unchanged |
| apollo radio seeded on the DJ uri | 404 |

The community record matches point for point: spotifyd#1393,
librespot#1604, go-librespot#287, go-librespot#296, librespot-java#883.
A TLS capture of the official desktop client (recorded in go-librespot#296)
shows it never fetches the queue over HTTP at switch time: roughly 68 "vibe
sections" arrive when the session starts, topped up every ~30 seconds over the
dealer websocket and Mercury AP events.

## Falsified premises

1. "`get_playlist` returns the full item window for AI-curation lists."
   It returns an empty shell for the DJ id.
2. "A registered Connect device can pull the lineup itself." Registration,
   activation, and advertising `supports_dj` all leave the context-resolve
   skeleton pointerless.

## How third-party clients actually receive DJ X

The merged reference implementation (go-librespot#352) and its predecessor
(#296) agree on the mechanics:

- An entitled client (phone or desktop official app) starts the DJ session and
  casts it to the third-party device over Connect.
- The delivered play command carries a context whose skeleton pages name
  session-bound `hm://` urls; following those pages yields the tracks.
- DJ tracks often carry an empty `uri`; the real one sits in
  `metadata["canonical_track_uri"]`.
- Ongoing rotation arrives as dealer `ClusterUpdate` pushes, not fetches.

Both implementations are handover-first. Nobody has demonstrated starting a DJ
session without an entitled client, and this repo's spike now shows the
self-start paths that seemed plausible are closed.

## Resolution (2026-08-24, live test)

The channel is now proven end-to-end with the instrumented probe. The working
sequence:

1. Register as a `CONNECT_STATE` member with go-librespot's proven device
   record (premium license, brand/model, their capability set). With
   `SPIRC_V3` membership the server never routed anything to us.
2. The user casts DJ X from the official app. The transfer arrives as a dealer
   player command whose `current_session.context.url` is
   `hm://lexicon-session-provider/context-resolve/v2/session?contextUri=<dj>`.
3. Fetching that session url returns the materialized context as JSON: about
   59 KB, 56 `spotify:track:` uris, `lexicon_set_type: your_dj`, plus
   restrictions and per-track metadata.

The lineup is reachable exactly once per live session, at cast time, through
the session url inside the transfer. Self-start remains closed. The endless
"connecting…" the sending client showed during probe runs is the missing half
a real player supplies: load the first track and publish player state to
finish the handshake.

## Decision

There is no fetchable DJ X channel, so the sidebar entry cannot open a lineup
by fetching, and an empty fetch must never render as an account/region
restriction. Until a live-session channel is proven end-to-end (handover from
an official client delivering a resolvable context), Cadence does not offer
DJ X. The spike's registration and resolution plumbing lives in
`examples/dj_probe.rs` so the next attempt starts from evidence instead of
repeating this investigation.

## Consequences

- `Playback::dj_lineup` and its fetch-based outcomes remain only until the
  session-based direction is either built or dropped; any UI built on them
  must not present an empty lineup as "not available on this account".
- Any future implementation must keep Cadence visible as a Connect device,
  parse dealer commands and cluster updates, follow skeleton pages, and fall
  back to `canonical_track_uri` for track identity.
- Narration/TTS (`client-tts/v1/fulfill` in go-librespot#352) is out of scope:
  Cadence wants the lineup, not the voice.
