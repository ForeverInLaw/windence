# DJ X starts itself, through the lexicon session provider

## Status

Accepted, 2026-08-25. Supersedes ADR 0004, which concluded that only a live
Connect handover could serve the lineup. That conclusion was drawn from
probes of a different service and is wrong about the feature; 0004 is kept
for the record of what it did falsify.

## Context

ADR 0004 established that `spclient` playlist reads return an empty shell for
the DJ id, and that `/context-resolve/v1/<dj-uri>` returns an inert skeleton —
before and after registering a Connect device. From that it concluded self-start
was closed and built a cast-handover service: Cadence registered as a Connect
device, waited for an official client to cast a DJ session, and played the
context the transfer carried.

That worked, and it cost about a thousand lines and a second source of truth
about what was playing. It also required a phone.

A recorded session of the official desktop client shows it starting DJ with one
ordinary request, to a service neither 0004 nor the community threads had
probed: `lexicon-session-provider`, not `context-resolve/v1`.

## Evidence

Two throwaway probes against a live account, before any design work. Both are
deleted; this table is their record.

| Probe | Result |
| --- | --- |
| `.../context-resolve/v2/session?contextUri=<dj>&reason=interactive` | 6884 bytes, 5 songs, one segment, cursor present |
| The same call minutes later | Identical segment and songs; only the request id differed |
| `reason=state_restore` | 164065 bytes, 155 songs — the whole accumulated session |
| Following the cursor | 5 fresh songs and a fresh cursor, three segments deep |
| Narration metadata | Introduction and "moving on" SSML on each segment's first song, with voice, provider, sample rate 44100, loudness -16.0, true peak -3.0 |
| `POST /client-tts/v1/fulfill` | `303 See Other` to a pre-signed CDN url |
| That url | `200`, `audio/mpeg`, 233 KB for one spoken line |

No Connect registration, no dealer, no player state. The session client's own
`hm://` request helper already authenticates all of it.

## How the session's cursor moves

The session keeps a cursor on the server, and **it moves when a page is
requested, not when a song is played.** The recorded client shows it: a restore
reporting segment 85000, the cursor followed one second later, and a restore
nineteen seconds after that reporting segment 1400000 — with no player state
published in between, only heartbeats. Our probes reproduce it from the other
side.

Three consequences follow, and together they decide the design:

- `interactive` does not read the cursor. It returns the same opening stretch
  however far the session has moved.
- `state_restore` does read it, but pays 155 songs and 164 KB to say so, and
  carries a history a station page has no use for.
- Fetching a page is what advances the cursor, so a client that simply follows
  cursors as it plays keeps the session correct without reporting anything.

## Decision

Cadence starts DJ X itself.

- The station is resolved with `reason=interactive` and continues by following
  the cursor each stretch hands back. `state_restore` is never used: everything
  it adds is history.
- The cursor is persisted with the rest of the playback state, so a restart
  resumes the station rather than replaying its opening stretch. A cursor that
  answers with nothing is an expired session, and opening a fresh one is the
  recovery — no clock comparison, no expiry bookkeeping.
- The DJ page lists what is coming up and nothing else, and its rows do not
  start playback.
- Narration is in scope. Both prepared lines are synthesized while the previous
  song plays, and the one that fits how the song was reached is spoken.
- The cast-handover service is deleted in full. Because the session advances on
  fetches alone, publishing player state to Spotify buys the station nothing.

## Consequences

- Cadence no longer appears in the phone's device list, and DJ can no longer be
  cast onto it. Connect presence is a feature in its own right, for all music
  rather than DJ X alone, and belongs in its own decision.
- `playback.rs` loses about two thirds of its size, and DJ playback shares the
  ordinary queue: one transport, one position-saving path, one player bar.
- Narration audio cannot go through the player library, whose local-file path
  indexes a directory once at startup. It is decoded separately and queued on
  the output device ahead of the song. SDL refuses to initialize from a second
  thread and its device handles are neither `Send` nor `Sync`, so this is not a
  preference — it is the only arrangement available.
- Out of scope, deliberately: crossfading the voice into the song, applying the
  loudness and true-peak targets the metadata carries, the closing line each
  stretch ends with, the "Let DJ pick" affordance, resampling a line that comes
  back at an unexpected rate, and caching synthesized speech across restarts.
