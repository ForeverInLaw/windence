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
Spotify's AI-curated personal station: the server picks tracks per account and speaks between them in its own client. The lineup has no fetchable form — it exists only inside a live playback session that Spotify's own clients start.
_Avoid_: DJX, AI DJ, mix

**Date Added**:
When a track entered the context it is listed in: a playlist's own add history, or the moment it was liked. It belongs to the listing, not to the track — the same track carries different dates in different contexts. Shown as a relative age during the first month, as an absolute date after that.
_Avoid_: release date (the album's), liked at

**Default Order**:
The track order Spotify reports for a context: playlists keep their curated sequence, liked songs their like sequence. Every sort view cycles back to it, and playback never edits it.
_Avoid_: original position, custom order

**Rootlist**:
The ordered set of playlists and folders Spotify holds for an account, read over the internal protocol. It carries the order, the folder tree and each entry's Date Added, but no names, artwork or owners — those keep coming from the Web API and are joined by uri.
_Avoid_: library, playlist list (that is what the rootlist feeds)

**Library Index**:
Cadence's own table of where each playlist and folder sits and when each was last played, fed from the rootlist and the recently-played endpoint. Every client keeps one of these; Spotify has no endpoint that returns an ordered library.
_Avoid_: library cache (the Web API copy of names and artwork is a different thing), rootlist

**Folder**:
A group of playlists inside the rootlist, marked by a start-group and an end-group entry around its children. It opens and closes where it stands instead of navigating anywhere, and it has no play history of its own: in the time-ordered sorts it takes the newest of its children.
_Avoid_: group, directory

**Pin**:
An item the listener has fixed to the top of their library in Spotify: a playlist or a folder. Pins belong to the account rather than to Cadence, so every client shows the same ones, and their order is hand-made and never sorted. Liked Songs is pinned on most accounts and is passed over, because Cadence already gives it a sidebar row of its own.
_Avoid_: favourite (that is Liked Songs), bookmark, starred

**Recents**:
The default playlist order, and the name the sort menu gives it: newest first by the later of last played and Date Added. The other three modes are Date Added, Alphabetical and Creator, worded as the official client words them.
_Avoid_: recently played (that is the page of played tracks), last played

**Home**:
Spotify's own page for the account: the curated shelves it offers right now, read from the internal partner endpoint because the Web API has no feed of them. Fetched when opened and refreshed on return; never stored.
_Avoid_: dashboard, recommendations (that is the track-level Web API feature behind Radio and Smart Shuffle), browse (a different destination)

**Shelf**:
One titled row of the Home feed, such as "Made For You" or "Soundtrack your Wednesday afternoon". It arrives with its first ten cards and a place to continue from; "Show more" appends the next page in place.
_Avoid_: section (the wire name), carousel, row (too generic)

**Card**:
One item on a Shelf: a playlist, album or artist with its artwork. Opening it hands the item to the page Cadence already has for that kind; podcast and episode cards are carried but not drawn.
_Avoid_: tile, item, entry
