# App-side shuffle over the local queue; librespot stays out of ordering

Shuffle and Smart Shuffle operate entirely on the backend's local queue, not through librespot's shuffle machinery.

Why: librespot's `Player` loads exactly one track at a time (`player.load(...)`); it holds no queue that could be shuffled. The play sequence lives in our `PlayQueue` (backend), our SQLite, and the UI. The shuffle code librespot does ship (`connect/src/shuffle_vec.rs`) belongs to the spirc/Connect path: it permutes a server-synced connect state when remote devices command this session — a path Cadence never runs, because we create Session+Player directly and drive them ourselves. Our contexts are materialized locally (`Vec<Track>` from rspotify fetches, the liked cache, apollo-station responses); Spotify's server does not know our queue. From librespot we take only the recommendation source (`spclient` apollo station endpoint via `Playback::radio_track_uris`). Third-party clients (ncspot and friends) all shuffle client-side too.

## Considered Options

- **Connect-state shuffle (adopt spirc, use shuffle_vec)** — rejected: gaining it means handing queue control to server-synced state that cannot represent our locally built contexts and cannot express the semantics we need (exact order restoration, anchored Play-next inserts, removable injected tracks).
- **App-side Fisher–Yates over `PlayQueue` with a stored permutation** — chosen: full control of toggle semantics, persistence in our own schema, testable without network.

## Consequences

- Shuffle state is per-device and local. It does not appear in Spotify connect state; other clients and devices are unaffected and unaware.
- Schema v6 persists the base context order, the active permutation, and the mode, so a restart keeps both the shuffled queue and a working toggle-off. The port's v7 snapshot adds the context kind (collection vs album) and per-track injected marks, so the Smart Shuffle gate and its woven-in recommendations survive a restart too.
- All ordering rules sit behind the backend queue boundary; if librespot ever exposes a usable non-spirc shuffle primitive, only that layer moves.
