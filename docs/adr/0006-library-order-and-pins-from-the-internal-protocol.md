# Playlist order and pins come from Spotify's internal protocol

The sidebar and the Playlists page order playlists the way the Spotify desktop
client does, and they show the same pinned items. Neither can come from the Web
API: it has no endpoint for either. Both are built from Spotify's internal
protocol over the librespot session we already hold for playback, and cached in
SQLite.

Why: Cadence showed whatever order `/me/playlists` happened to return, and its
pins were a private local list that had nothing to do with the pins in Spotify.
Two clients on one account disagreed about the same library.

## What Spotify actually does

There is no server-side "ordered library" endpoint, and no `your_library` HTTP
or gRPC service — the desktop client keeps a local index and feeds it from
several sources. Each device computes the same order independently from the
same signals, which is why playing a playlist on a phone reorders it on a PC.

Cadence keeps its own index of the same shape, fed the same way:

| Fact | Source |
| --- | --- |
| Playlist set, folder tree, `add_time` | `GET playlist/v2/user/<user>/rootlist`, decoded as `playlist4_external.SelectedListContent` |
| `last_played` per context | `GET recently-played/v3/recently-played?limit=1000` |
| Pins, initial state | `POST collection/v2/paging`, `PageRequest{set: "ylpin"}` |
| Pins, increments | `POST collection/v2/delta` with the stored sync token |
| Pins, live changes | Dealer subscription to `hm://collection/ylpin/<user>` |
| Pin writes | `POST collection/v2/write`, `WriteRequest{set: "ylpin"}` |

Folders appear in the rootlist as `spotify:start-group:<id>:<url-encoded-name>`
… `spotify:end-group:<id>` markers around their children. The same folder is
named three different ways across these sources — bare id in the rootlist,
`spotify:folder:<id>` in pins, `spotify:user:<user>:folder:<id>` in the desktop
index — so uris are normalised on the way into the index.

"Recents", the default sort, orders by `max(last_played, add_time)` descending.
A folder has no `last_played` of its own and takes the newest of its children.

A pin write sends the whole set. Each item carries the library's own Date
Added, in seconds, rather than the moment it was pinned. Spotify answers with
`200` and an empty body, then pushes a dealer message carrying back the
`client_update_id` the write sent. A client can use that to tell its own
change from another device's. Cadence does not: it answers every one of those
messages the same way, with a `delta` call that costs one request and settles
nothing when the change turns out to be its own.

## Decisions

- **The Web API stays the source of playlist metadata.** The rootlist carries
  order, tree, and timestamps; names, artwork, owners, and track counts keep
  coming from `/me/playlists` and are joined by uri. The existing, tested load
  path survives, which is what makes the fallback below possible.
- **Pins are two-way.** Pinning re-reads `paging` and merges before writing,
  because a write sends the whole pin list rather than a delta and would
  otherwise silently clobber a pin made on another device. Unpinning sends a
  single `CollectionItem{is_removed: true}`, as the desktop does, where no race
  exists. The local `pinned_playlists` table is dropped: one truth, not two.
- **A refused write is rolled back.** The button moves on the click, so the
  answer has to be able to move it back. What goes to the window after a
  failure is the set Spotify holds — freshly read, in the pinning case — and
  not the set the click hoped for.
- **A dealer message is a nudge, not the change.** The subscription says
  that the pinned set moved, never how, and never carries the set itself.
  Each message is answered with a `delta` call against the stored sync
  token, which is also why the full read has to land before the
  subscription is opened.
- **A delta places a new pin at the end.** It reports what changed without
  saying where it sits, and the section is hand-ordered, so a pin made on
  another device sits last until the next full read puts it where Spotify
  holds it.
- **`spotify:collection` is skipped when rendering pins.** Liked Songs is a
  permanent sidebar row and stays where it is. It stays in the stored pin
  list all the same: a write sends the whole set, so dropping it on the way
  in would unpin it on the way out.
- **Sorting never moves pins.** Pin order is hand-made — that is what
  `after_uri`/`before_uri`/`first` exist for.
- **Change detection by revision.** The rootlist's first page carries its
  `revision`; when it matches the stored one, the remaining pages are not
  fetched. This mirrors the head-probe already used for the Web API library.
- **Sources apply independently.** A half-finished sync leaves fresh order
  beside yesterday's pins rather than discarding what did arrive.
- **Cadence will publish its playback state.** Today Spotify does not know
  Cadence exists: `put_connect_state` is never called, so a play here reaches
  neither the account's history nor any other device's ordering. A later change
  registers the device through `librespot_connect::ConnectState` and maps
  incoming connect commands onto Cadence's existing actions. Full `Spirc` is
  rejected: it carries its own queue, context, and shuffle state machine, which
  would fight the one Cadence already has under DJ X and Smart Shuffle.

## Considered Options

- **Approximate the order locally** from Cadence's own play history — rejected:
  a listener's phone plays would be invisible, so the two clients would still
  disagree, which is the whole complaint.
- **Import the desktop client's index** over CEF remote debugging — rejected:
  needs Spotify installed with remote debugging on, and breaks on any xpui
  update. `recently-played/v3` gives every account its full history in one
  request, so there is nothing left to import.
- **Web API `/me/player/recently-played`** — rejected once the internal
  endpoint was found: it is capped at 50 plays and reports tracks, not
  contexts, so playlist recency had to be inferred.
- **Full `Spirc` for playback reporting** — rejected, see above.

## Consequences

- Order and pins need a live librespot session. Without one — no Premium,
  offline, connection not up — the cached index is shown, and failing that the
  Web API order, exactly as before this change. In that state only the
  alphabetical and creator sorts can work, so the other two are hidden rather
  than shown broken.
- `collection2v2.proto` and `recently_played_backend.proto` ship in
  librespot-protocol's sources but are not among the messages it compiles.
  Both are vendored and generated in our own `build.rs`.
- These endpoints are reverse-engineered and carry no compatibility promise.
  Every one of them is behind a fallback to the Web API path; a format change
  degrades the order, it does not break the library.
- The pin limit (20 on the account observed) is only reported over the
  Esperanto IPC the desktop uses internally, never over the network. The UI
  warns at 20 but still attempts the write, so a raised limit is not something
  Cadence would have to be taught about.
