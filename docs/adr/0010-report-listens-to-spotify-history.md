# Report listens to the Spotify history

Status: accepted, 2026-10-31.

A song played in Windence does not reach the account's Spotify history. Recently
Played on other devices and the Web API's `recently-played` do not show it. On
one account, none of the 25 history entries from 26 to 29 October matched a play
in Windence's own history.

ADR 0006 and `connect.rs` said that the connect-state report adds a play to the
history. That was wrong. Spotify accepts the report, but the history does not
change.

## Decision

Windence can send a `RawCoreStream` event to
`/gabo-receiver-service/v3/events/` when a track ends. This is the event the
official client sends. librespot does not send it.

The setting "Report plays to Spotify" controls this. **It is off by default.** A
listener has to turn it on, and each report reads the setting first, so turning
it off stops reports at once.

- The time in a report is only time the player moved through the track. Paused
  time and skipped parts do not count, and there is no way to report a play
  that did not happen.
- Only Spotify tracks are reported. Local files and episodes are not.
- A retry sends the same bytes, so one listen cannot become two plays.
- A failed report is logged and dropped. Reports are not saved for later.

`src/listening.rs` holds the measuring and the event. `backend.rs` runs it for
each playback session.

## Why it is off by default

- Spotify does not document this event or support it for other apps. We do not
  know how Spotify treats these reports, so the risk to the account belongs to
  the person who accepts it.
- The event names a version of the desktop client. Spotify accepts other values
  without an error, but it may then leave the play out of the history.
- Spotify may use this event to count streams. The librespot maintainers chose
  not to add it, because such code can be abused to fake plays. See
  [librespot discussion #626](https://github.com/librespot-org/librespot/discussions/626).

## Known limits

- The schema was seen in one version of the desktop client. If Spotify changes
  it, plays may stop reaching the history with no error. A success response
  does not prove the play was added.
- The file in a report is the one the player would pick. The player's event
  does not name the file it opened, so `PLAYER_FORMATS` must follow its order.
- A listen in progress is lost if the session drops. On closing the app, it is
  sent with a wait of up to 3 seconds.

## Considered options

- **Change the connect-state report.** It does not create history entries.
- **Send the track to a personal service from Windence.** It would change the app
  for everyone to serve one person's profile page.
- **Scrobble to Last.fm or ListenBrainz.** It is safe, but Spotify's history would
  stay empty.
- **Patch librespot.** The event does not need any change to the player, and a
  fork is more work to keep up to date. `RawCoreStream.proto` is taken from
  [librespot PR #1759](https://github.com/librespot-org/librespot/pull/1759),
  because the copy in librespot 0.8 lacks the newer fields.
  `audio_files_extension.proto` is trimmed to `audio_id`, so that it does not
  generate a second copy of librespot's metadata messages.
