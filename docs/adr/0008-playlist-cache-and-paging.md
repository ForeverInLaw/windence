# Cache playlist prefixes and load pages on demand

## Context

Opening a playlist fetched all its tracks before showing any rows. Reopening
it repeated the same work. Large playlists delayed browsing even when the
listener only wanted the first few rows.

Spotify's Web API supports offset pages of up to 50 items and gives each
playlist a `snapshot_id`. These are the paging and version boundaries.
The existing internal protocol remains the source for Spotify-curated
playlists. DJ X keeps its live session path.

## Decision

Store a playlist prefix in the existing SQLite database. Key it by the
verified Spotify account ID and playlist ID. Store its revision, raw next
offset, raw total and ordered track occurrences. Do not deduplicate tracks.
Keep Date Added on each occurrence.

Publish a saved prefix before checking its revision. A matching revision
needs no track-page request. A changed revision starts again at offset zero.
Check the revision after fetching each page and discard a page whose version
changed. Replace or append rows in one transaction. An append must match the
stored revision, offset and total.

Fetch the first 50 raw items on a cold open. Fetch another page when the
listener scrolls to the end or selects Load more. Hidden items still advance
the raw offset. Save partial progress so the next open can continue from it.

The playlist page owns one stream and its control channel. Closing the route,
opening another playlist or changing accounts cancels that stream. Storage
jobs check the account generation while holding the store lock. Logout clears
the playlist cache along with the existing account data.
For curated playlists, the authenticated playback account must match the
Web API account before any saved rows are published or native data is cached.

Search and sorting request missing pages. Play, Shuffle and row playback
request a complete, revalidated context before starting the existing local
queue. A row intent belongs to a particular revision and occurrence: changing
the revision cancels that intent. Search only changes visible rows.

## Consequences

- Browsing no longer downloads every track on each open. SQLite schema 13
  adds the cache without changing saved playback or preferences.
- Page errors leave the saved prefix visible and offer Retry.
- Full-context actions can still take time for a large uncached playlist.
  The page displays their loading state.
- The account profile is shared in memory. A fresh process must verify the
  account before reading its saved rows; this is not an offline account cache.
- The internal playlist endpoint exposes a URI listing rather than an offset
  API. Curated playlists resolve track details in groups of 50. Incomplete
  listings are rejected instead of being marked as complete.
- The queue remains fully materialized as required by ADR 0003. This cache
  does not replace queue persistence.
- Builds before schema 13 cannot open an upgraded profile. Native appearance
  comparisons need a fresh profile for each tested build.

## Sources

- [Get Playlist Items](https://developer.spotify.com/documentation/web-api/reference/get-playlists-items)
  defines the page limit and raw offset.
- [Get Playlist](https://developer.spotify.com/documentation/web-api/reference/get-playlist)
  exposes `snapshot_id` and field selection.
- [Spotify playlist versions](https://developer.spotify.com/documentation/web-api/concepts/playlists)
  describe snapshots.
- [IVO-78](https://linear.app/ivoxygen/issue/IVO-78/keshirovat-plejlisty-i-zagruzhat-treki-po-stranicam)
  records the agreed behavior.
