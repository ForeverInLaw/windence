# DJ X rides the playback session's internal protocol; the catalog stays on the Web API

Cadence speaks two Spotify data channels, and each context is fetched through exactly one. The catalog (search, liked songs, the listener's playlists, their contents, artists, albums) stays on the Web API via rspotify. The DJ X lineup is the one context fetched through the playback session's internal protocol (`spclient` protobuf endpoints over the live librespot session).

Why: since November 2024, development-mode Web API apps get 404 on every Spotify-owned playlist. A live probe in August 2026 confirmed this for a valid premium token: the DJ playlist and a control editorial playlist both 404'd across metadata, tracks, and items endpoints while `/me` succeeded. The Web API channel therefore cannot serve DJ X at all, while the internal protocol — the same channel librespot already uses for track metadata and the apollo station radio endpoint — knows nothing of development-mode restrictions.

## Considered Options

- **Web API playlist endpoints** — rejected: hard-blocked for development-mode apps (the probe above); no quota tier short of production access changes this.
- **Internal `spclient` playlist endpoint (adopted)** — chosen: `get_playlist` returns the playlist4 protobuf (attributes, item list with added-at dates); per-track metadata comes from the existing `get_track_metadata` path. The official client fetches the lineup through the same family of endpoints.
- **Scraping or private gateways** — rejected: out of keeping with the librespot-based stack.

## Consequences

- The DJ identity is a hardcoded well-known playlist ID; no discovery, following, or configuration.
- Conversion from librespot metadata to the shared domain model happens once, in a converter beside the Web API converters; downstream (queue, shuffle, media controls) sees an ordinary `Vec<ListedTrack>`.
- Internal-protocol refusals (not found, forbidden, empty lineup) map to a dedicated "not offered" outcome rendered as its own empty state; transport failures stay on the generic retryable error path.
- Reads only: nothing is ever written back to the remote playlist object.
- The `get_playlist` call has no continuation parameter; it returns the full item window for AI-curation lists, and truncation is logged rather than paged. Revisit only if real lineups ever truncate.
