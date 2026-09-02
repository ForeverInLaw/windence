# The Home feed comes from Spotify's partner GraphQL endpoint

The Home page shows the shelves Spotify curates for the account: Discover
Weekly, the Daily Mixes, daylists, Release Radar, "This Is" sets, the mood and
editorial shelves. None of this is in the Web API. It is read from the same
internal GraphQL endpoint the desktop client draws its Home page from, over
the librespot session Cadence already holds for playback.

Why: the Web API lists what the account saved and what a search finds, but it
has no way to ask "what does Spotify recommend right now". The personal
playlists exist and load fine once their uri is known; the feed is the only
place that names them.

## What Spotify actually does

The desktop client loads Home with one persisted GraphQL query on
`POST https://api-partner.spotify.com/pathfinder/v2/query`. A persisted
query sends only the operation name, a `sha256Hash` of the query document and
the variables; Spotify holds the query text. One document carries both
operations the page needs, and `operationName` picks between them:

| Operation | Variables | Answers |
| --- | --- | --- |
| `home` | `timeZone`, `facet: ""` (all, not `music`/`podcasts`), `sectionItemsLimit: 10` | Every shelf with its first page of cards, at `data.home.sectionContainer.sections` |
| `homeSection` | `uri` of a shelf, `sectionItemsOffset`, `sectionItemsLimit: 20` | One shelf's next page, at `data.homeSections.sections[0].sectionItems` |

Both also carry the constants the desktop sends: `homeEndUserIntegration:
"INTEGRATION_DESKTOP"`, `sp_t: ""` and `includeEpisodeContentRatingsV2: true`.

Every shelf reports `pagingInfo.nextOffset`, an integer while more cards exist
and `null` once the shelf is complete. There is no paging of shelves: the
desktop client calls `home` once and draws what comes back, ignoring the
reported `sections.totalCount`.

The request carries the session's access token as a Bearer, the
`Client-Token` librespot's spclient issues, and two headers the endpoint
checks, `app-platform` and `spotify-app-version`, set to the desktop values.
`accept-language` decides the language of shelf titles and `timeZone` the
greeting and the time-of-day shelves.

Cards are typed wrappers: `PlaylistResponseWrapper`, `AlbumResponseWrapper`,
`ArtistResponseWrapper`, plus podcast, episode and list wrappers, and
`UnknownType` placeholders (the liked-songs tile) with no content behind them.

## Decisions

- **One module knows GraphQL exists.** `feed.rs` sends the persisted query,
  reads the envelope and flattens it into the `Home*` model types. Nothing
  else sees an operation name or a hash, so the Browse tab, when it comes, is
  one more caller of the same transport.
- **The hash is a constant, dated in its comment.** Spotify makes no promise
  about it. When a new Home query ships, the endpoint answers
  `PersistedQueryNotFound`, the page shows the error, and the fix is to read
  the new hash from the current xpui bundle. No retry, no fallback: there is
  no Web API equivalent to fall back to.
- **The Web API stays the source of what a card opens.** A card carries a
  uri, a name and artwork; opening it hands the existing playlist, album or
  artist page that uri, and the tracks load the way they always have. The
  same split ADR 0006 uses for the library.
- **Fetched on arrival, not at start-up.** The feed needs the playback
  session and changes by the hour, so the page loads when the listener opens
  it and refreshes on return behind a 30 second debounce. No SQLite cache: a
  cache would mostly serve stale shelves.
- **Shelves are the unit of browsing.** 18 of 31 recorded shelves hold more
  than their first ten cards, so each shelf with a `nextOffset` ends in a
  "Show more" card that appends the next page in place. Pages for different
  shelves run in parallel and never cancel one another.
- **Placeholders are dropped card by card.** A card without content has
  nowhere to go. Its shelf stays, including the Shorts row, whose playlists
  are as playable as any.
- **Podcast and episode cards are carried but not drawn.** Cadence has no
  page for them yet; the model keeps them as `Other` so adding one is a
  render change.
- **English titles.** Cadence has no language setting, so `accept-language`
  matches its own chrome. The listener's time zone comes from the OS.

## Considered Options

- **Scrape the desktop client's rendered page** over remote debugging —
  rejected for the same reasons as in ADR 0006: needs Spotify installed and
  breaks on every xpui update.
- **Build shelves from Web API recommendations** — rejected: the
  recommendations endpoints return tracks, not the curated playlists, and
  cannot name Discover Weekly or a daylist at all.
- **Ship the query text and use the non-persisted form** — rejected: the
  text would drift from Spotify's just as the hash does, and the recorded
  client never sends it.
