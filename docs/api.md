# RMS native API (v1)

A JSON API for controlling RMS, with state pushed to clients instead of polled.  The web UI
uses nothing else.  It lives on the HTTP port (9000 by default) under `/api/v1`.  When
`authorize` is on it needs the same HTTP Basic login as the web UI.

TypeScript declarations of every type below are in
[`crates/rms/web/api-types.d.ts`](../crates/rms/web/api-types.d.ts), generated from the Rust
types (`crates/rms/src/api/model.rs`); a test fails when they are out of date.  Example
requests and responses for every resource are recorded in
[`crates/rms/tests/golden/native_api.json`](../crates/rms/tests/golden/native_api.json).

The LMS interfaces (JSON-RPC, Cometd, CLI) are still there for Squeezebox hardware,
SqueezePlay/Squeezer and other existing clients.  The native API is a separate surface with its
own conventions:

* Field names are `snake_case`.  Enums are lower-case strings.  Booleans are `true`/`false`.
* An absent value is left out (no `""`, `0` or `-1` stand-ins).
* Errors are `{"error": "<message>"}` with status 400 (bad request), 404 (unknown player,
  item or path) or 422 (the server could not carry it out, e.g. the player is disconnected).
* Lists are [pages](#page): `?offset=` (default 0) and `?limit=` (default 100) select one.

## Resources

| | |
|---|---|
| `GET /api/v1/server` | [Server](#server) |
| `GET /api/v1/players` | list of [Player](#player) |
| `GET /api/v1/players/{id}` | [Player](#player) |
| `GET /api/v1/players/{id}/queue?offset=&limit=` | [Queue](#queue) |
| `GET /api/v1/players/{id}/spectrum?from=` | [Spectrum](#spectrum) of what it plays |
| `POST /api/v1/players/{id}/command` | run a [command](#commands) |
| `POST /api/v1/players/{id}/announce?volume=` | play an [announcement](#announcements), then carry on |
| `GET`, `PATCH /api/v1/players/{id}/settings` | [player settings](#player-settings) |
| `GET /api/v1/events` | [event stream](#events) |
| `GET /api/v1/browser-player` | WebSocket: [this browser as a player](#browser-player) |
| `GET /api/v1/artists?q=&role=` | page of [Artist](#artist); `role=album`: album artists only (the artists albums are filed under, as Lyrion's album artists list) |
| `GET /api/v1/artists/{id}` | [Artist](#artist) |
| `GET /api/v1/artists/{id}/top-tracks` | [Track](#track)s: the artist's most played |
| `GET /api/v1/artists/{id}/similar` | `[{"name", "artist"?: Artist}]`: similar artists ([ListenBrainz](#listenbrainz)) |
| `GET /api/v1/albums?sort=&artist=&genre=&year=&q=` | page of [Album](#album) |
| `GET /api/v1/albums/{id}` | [Album](#album) with `duration` and `tracks` |
| `GET`, `PUT`, `DELETE /api/v1/albums/{id}/release` | the album's [MusicBrainz release](#album-release) |
| `GET /api/v1/albums/{id}/tags?release=` | the album's [tags](#tags), and what the release says |
| `POST /api/v1/tags` | [write tags](#tags) into files |
| `GET /api/v1/albums/{id}/files?template=`, `POST` | where the album's [files](#files) go, and moving them |
| `GET /api/v1/albums/{id}/cover-art?release=`, `POST` | [cover art](#cover-art) from the Cover Art Archive |
| `GET /api/v1/tags/edits`, `POST /api/v1/tags/edits/{batch}/undo` | [tag changes and moves](#tags), and undoing one |
| `GET /api/v1/genres?q=` | page of `{id, name}` |
| `GET /api/v1/years` | page of `{year}` (0: tracks without a year) |
| `GET /api/v1/tracks?q=&sort=&genre=&bpm_min=&…` | page of [Track](#track): [finding tracks](#finding-tracks) |
| `POST /api/v1/tracks?name=&folder=` | [upload](#uploading-music) a music file → its [Track](#track)s |
| `GET /api/v1/tracks/{id}` | [Track](#track) with its `detail` |
| `GET /api/v1/tracks/{id}/similar?limit=&detail=` | [Track](#track)s: [similar songs](#similar-songs) |
| `POST /api/v1/similar` | [Track](#track)s: [similar songs](#similar-songs) to several seeds |
| `GET /api/v1/tracks/{id}/analysis` | what the [analyses](#analysis) made of a library track |
| `POST /api/v1/query` | a page of the [Table](#the-table): tracks, or albums, artists, years, … with their columns |
| `GET /api/v1/query/fields` | the Table's [fields](#the-table) |
| `GET`, `PUT /api/v1/query/views` | its [saved views](#the-table) |
| `GET /api/v1/playlists?q=` | page of `{id, name, editable}` (saved playlists) |
| `GET /api/v1/playlists/{id}` | `{id, name, editable}` |
| `GET /api/v1/playlists/{id}/tracks` | page of [Track](#track) |
| `POST /api/v1/playlists` | a new [playlist](#playlists) → `{id, name, editable}` |
| `PATCH /api/v1/playlists/{id}` | `{"name": …}`: rename |
| `DELETE /api/v1/playlists/{id}` | delete |
| `PUT /api/v1/playlists/{id}/tracks` | `{"order": [indexes]}`: reorder, removing those left out |
| `POST /api/v1/playlists/{id}/tracks` | `{"item": Item, "position"?: n}` → `{"count"}`: add |
| `GET /api/v1/folders?id=` | page of [folder entries](#folder-entry) |
| `GET /api/v1/search?q=&limit=` | [search results](#search) |
| `GET /api/v1/favorites?id=` | page of [BrowseItem](#browseitem) |
| `POST /api/v1/favorites` | add a favorite: `{"url": …, "title": …}` |
| `DELETE /api/v1/favorites/{id}` | remove a favorite or a folder of them |
| `GET /api/v1/radio?id=` | page of [BrowseItem](#browseitem): [internet radio](#internet-radio) |
| `GET /api/v1/listenbrainz?id=` | page of [BrowseItem](#browseitem): [ListenBrainz](#listenbrainz) recommendations and playlists |
| `POST /api/v1/listenbrainz/accounts` | `{"token": …}` → `{"user", "default"}`: add a [ListenBrainz](#listenbrainz) account |
| `PATCH /api/v1/listenbrainz/accounts/{user}` | `{"default": bool}` |
| `DELETE /api/v1/listenbrainz/accounts/{user}` | remove it (and its unsent listens) |
| `GET /api/v1/spotify` | [Spotify status](#spotify) |
| `GET /api/v1/players/{id}/spotify-queue` | [what Spotify plays next](#spotify) on its Connect device |
| `GET /api/v1/spotify/items?id=` | page of [BrowseItem](#browseitem) |
| `POST /api/v1/spotify/login`, `/logout` | [sign in and out](#spotify) |
| `PATCH /api/v1/spotify`, `GET`, `DELETE /api/v1/spotify/learned` | [learning for mixes](#spotify) |
| `GET`, `PATCH /api/v1/settings` | [server settings](#server-settings) |
| `POST /api/v1/library/scan` | [scan the library](#server-settings) |
| `GET /api/v1/library/analysis` | how far the [analyses](#analysis) have got |

`{id}` is the player's MAC address (`00:04:20:12:34:56`) for players, the library id for
artists, albums and playlists, and the item id (`0.1`) for favorites.

## Events

`GET /api/v1/events` is a [Server-Sent Events](https://html.spec.whatwg.org/multipage/server-sent-events.html)
stream.  Browsers read it with `EventSource`.  Each message's SSE event name equals the
`type` field of its JSON data:

| type | data | when |
|---|---|---|
| `snapshot` | `{server, players}` | first message on every connection, and again if the client fell too far behind |
| `server` | `{server}` | library totals or scan progress changed |
| `player` | `{player}` | a player connected or anything about it changed |
| `player_removed` | `{id}` | a disconnected player was forgotten |

Every message carries the complete object, not a diff, so a client only has to replace what
it holds.  After a reconnect the snapshot brings it up to date.

A player's position is **not** pushed while it moves with the clock.  `elapsed` is the
position when the state was produced.  While `mode` is `play` and `buffering` is false, add
the time since the message arrived.  A seek, track change, pause or stall sends a new
`player` event.

```sh
curl -N http://localhost:9000/api/v1/events
```

## Types

### Server

```json
{
  "name": "living-room-pi",
  "version": "0.1.0",
  "uuid": "b8e7d126…",
  "scan": {"step": "directory_new", "done": 120, "total": 4000},
  "last_scan": 1791694802,
  "library": {"albums": 310, "artists": 120, "genres": 25, "tracks": 4000, "duration": 980000.0},
  "https_port": 9443,
  "local_ca": true
}
```

`scan` is present only while a library scan runs.  `last_scan` is a Unix time.

### Player

```json
{
  "id": "00:04:20:12:34:56",
  "name": "Kitchen",
  "model": "squeezelite",
  "model_name": "SqueezeLite",
  "ip": "192.168.1.20",
  "firmware": "v2.0.0",
  "connected": true,
  "power": true,
  "mode": "play",
  "buffering": false,
  "elapsed": 37.25,
  "duration": 241.0,
  "can_seek": true,
  "volume": 40,
  "muted": false,
  "volume_fixed": false,
  "shuffle": "off",
  "repeat": "all",
  "queue": {"version": 1791694516.45, "length": 12, "index": 2, "name": "Morning"},
  "now_playing": { "…": "a Track" },
  "synced_with": ["00:04:20:ab:cd:ef"],
  "sleep": 1200.0
}
```

* `mode`: `play` | `pause` | `stop`.  `buffering` means "playing, but the position is not
  moving yet".
* `volume`: 0–100.  `volume_fixed` means the player outputs a fixed level, so a volume
  control should be disabled.
* `shuffle`: `off` | `songs` | `albums`.  `repeat`: `off` | `one` | `all`.
* `queue.version` changes whenever the queue's contents or order change.  Fetch the queue
  again when it does.  `queue.index` is the current entry and is absent when the queue is
  empty.
* `synced_with` lists the other members of the player's sync group.
* `sleep` is the number of seconds left on the sleep timer, if one is set.

### Track

```json
{
  "id": 1234,
  "url": "file:///music/Artist/Album/01.flac",
  "title": "Song",
  "artist": "Artist",
  "artist_id": 7,
  "album": "Album",
  "album_id": 42,
  "duration": 241.0,
  "track_number": 1,
  "disc": 1,
  "year": 1999,
  "artwork": "music/ab12cd/cover",
  "remote": false,
  "station": "Radio Paradise",
  "format": "flc"
}
```

* `id` is the library id.  Streams and service tracks have none.
* `artwork` is relative to the server root.  When it ends in `/cover` or `/image`, append
  `_<w>x<h>_o` to get a resized copy (`music/ab12cd/cover_300x300_o`).
* For a Spotify track (or what a Spotify Connect stream plays now), `spotify_album` and
  `spotify_artist` are the Spotify menu ids of its album and first artist (`album:<id>`,
  `artist:<id>`), when known.
* For radio, `station` is the station's name.  `title` and `artist` are the current song,
  split from the stream's "Artist - Title" metadata when needed.
* `detail` is there only when asked for (`detail=true`, and always from
  `GET /api/v1/tracks/{id}`), for library tracks:

```json
{"genres": ["Jazz", "Modal"], "bpm": 136.2, "key": "8A", "rating": 80, "play_count": 12,
 "skip_count": 1, "last_played": 1791694802, "added": 1780000000,
 "mbid": "5fb524f1-…", "isrc": "USSM15900113", "loudness": -14.2, "analyzed": true,
 "kind": {"styles": [{"name": "Electronic---Techno", "score": 0.62}],
          "moods": [{"name": "danceable", "score": 0.97}, {"name": "party", "score": 0.94}],
          "themes": [{"name": "energetic", "score": 0.21}],
          "instruments": [{"name": "synthesizer", "score": 0.55}]}}
```

  `bpm` is the measured tempo, else the `BPM` tag; `key` the measured key as a Camelot code
  (`8A` is A minor, `8B` C major).  Both, and `loudness` (LUFS), come from the sonic analysis,
  as it reaches each track.  `rating` is 0–100.  `skip_count` counts plays left within 30
  seconds.  `analyzed`: how the song sounds is known, so [similar songs](#similar-songs) go by
  it.  `kind` is there once the [EffNet analysis](#server-settings) has reached the song:
  its likeliest Discogs styles (`Genre---Style`), moods (`happy`, `sad`, `relaxed`,
  `aggressive`, `party`, `danceable`, `acoustic`, `electronic`, `voice`, and `approachability`
  and `engagement` as 0–1 values), themes and instruments, each with a score of 0–1.

### Queue

```json
{"version": 1791694516.45, "length": 12, "index": 2, "offset": 0, "items": [ "…Tracks" ]}
```

Item `i` is queue entry `offset + i`.  By default the whole queue is returned.

### Spectrum

What a player plays as four bars, bass to treble (the web UI's moving bars), played back
from the player's position: no audio reaches the client.

```json
{"kind": "track", "key": "track:42", "fps": 20, "start": 0, "curve": "q7yrq5qZ…"}
```

`curve` is base64: `fps` readings a second from `start` seconds into the track, two bytes
each, four levels 0–15 (30–150 Hz, 150–600 Hz, 600 Hz–2.5 kHz, 2.5–11 kHz) with the first of
each pair in the high four bits.  Each band is scaled to its own range in the track, quiet
to loud.  `kind`:

* `track`: a library track's whole curve, stored by the background analysis (or worked out
  when first asked for, which takes about a second).  Fetch it once per track (`key`).
* `live`: a stream the server is playing to the player (internet radio, Spotify), analysed
  as it goes, from a second before `from` (seconds into the track) to as far as it has got.
  Ask again for more, about once a second.
* `none`: nothing to show: not playing, a stream the player fetches itself
  (`radio_direct`), or audio that can't be decoded.

## Commands

`POST /api/v1/players/{id}/command` with a JSON body tagged by `command`.  The response is
`{}`, or `{"count": n}` for `enqueue` and `mix`.  The resulting state arrives on the event
stream.

| body | |
|---|---|
| `{"command": "play"}` | play, or resume when paused |
| `{"command": "pause"}` | |
| `{"command": "toggle_pause"}` | |
| `{"command": "stop"}` | |
| `{"command": "next"}` / `{"command": "previous"}` | |
| `{"command": "seek", "position": 61.5}` | seconds into the current track |
| `{"command": "volume", "value": 30}` | 0–100 |
| `{"command": "mute"}` | toggle; or `"muted": true/false` |
| `{"command": "power", "on": true}` | |
| `{"command": "shuffle", "mode": "songs"}` | `off` / `songs` / `albums` |
| `{"command": "repeat", "mode": "all"}` | `off` / `one` / `all` |
| `{"command": "jump", "index": 3}` | play queue entry 3 |
| `{"command": "move", "from": 5, "to": 0}` | |
| `{"command": "remove", "index": 2}` | |
| `{"command": "clear"}` | |
| `{"command": "enqueue", "item": Item, "mode": "play", "start": 4}` | `mode`: `play` (replace the queue; the default), `add` (append), `next` (after the current track).  `start`: the entry of the new queue to begin at |
| `{"command": "mix", "seed": Item}` | replace the queue with about 20 similar songs (Create Mix) and play them; the seed is a track, album, artist, genre or year |
| `{"command": "sync", "with": "00:04:20:ab:cd:ef"}` | join that player's sync group |
| `{"command": "unsync"}` | |
| `{"command": "rename", "name": "Kitchen"}` | |

`play`, `enqueue` with `mode: play` and `mix` turn the player on first.

An `Item` is one of:

```json
{"type": "track", "id": 1234}
{"type": "tracks", "ids": [1234, 77, 5120]}
{"type": "album", "id": 42}
{"type": "artist", "id": 7}
{"type": "genre", "id": 3}
{"type": "year", "year": 1999}
{"type": "playlist", "id": 9001}
{"type": "folder", "id": 50001}
{"type": "url", "url": "http://radio.example.com/stream"}
{"type": "favorite", "id": "0.1"}
{"type": "radio", "id": "radiobrowser.1.0"}
{"type": "spotify", "id": "1.0.3"}
{"type": "query", "q": "genre:jazz year:1950..1959", "sort": [{"field": "year"}]}
```

A `query` is the library tracks a [Table](#the-table) query finds, in its order (at most
`maxPlaylistLength`).

```sh
curl -X POST http://localhost:9000/api/v1/players/00:04:20:12:34:56/command \
     -H 'Content-Type: application/json' \
     -d '{"command": "enqueue", "item": {"type": "album", "id": 42}}'
```

## Announcements

`POST /api/v1/players/{id}/announce` plays a short clip — a doorbell, a spoken message — on
the player and its sync group, then puts everything back: the queue as it was (shuffle order
and repeat included), the track, playing again from where it was, and each member's power
and volume.  The body is the clip itself, at most 20 MB, with an audio `Content-Type`
(`audio/mpeg`, `audio/wav`, `audio/flac`, `audio/ogg`, `audio/opus`, `audio/mp4`,
`audio/aiff`); any player can play it, transcoded when needed.  `volume` (0–100) plays it
at that volume instead of the players' own.

```sh
curl -X POST -H 'Content-Type: audio/mpeg' --data-binary @dinner.mp3 \
     'http://localhost:9000/api/v1/players/00:04:20:12:34:56/announce?volume=50'
```

It answers `202 {}` once the clip is queued; the rest shows on the event stream.  `400` for
something that isn't audio, empty or too big; `422` while another announcement plays in the
group.  The music stops for the clip (it isn't mixed under it).  A paused player comes back
stopped at the track it was on; one that was off is turned on for the clip and off again.
Choosing something else to play while the clip plays cancels the return: nothing is put back
over it.

## Library

The library resources list what the players and SqueezePlay/Squeezer show: the same artists
(the "artists" roles from the settings), the same Various Artists handling, the same
sort order.  Ids are stable until the library is cleared and scanned again; URLs are kept.

### Page

```json
{"total": 120, "offset": 0, "items": [ "…" ]}
```

Item `i` is entry `offset + i` of `total`.

### Artist

```json
{"id": 7, "name": "Miles Davis", "portrait": "contributor/ab12cd/image", "letter": "M"}
```

`portrait` is the artist's picture, if there is one (append `_<w>x<h>_o` like for artwork).
`letter` is the index letter of the list's sort order.

### Album

```json
{"id": 42, "title": "Kind of Blue", "artist": "Miles Davis", "artist_id": 7, "year": 1959,
 "compilation": false, "release_type": "album", "artwork": "music/ef34ab/cover", "letter": "K"}
```

`release_type` is the tagged release type, in lower case (`album`, `ep`, `single`,
`compilation`, `live`, …), when release types aren't ignored in the settings.

`GET /api/v1/albums` sorts by `title` (default), `artist` (then year), `year` or `added`
(newest first), and filters by `artist`, `genre` and `year` ids and by search words `q`.
`GET /api/v1/albums/{id}` adds `duration` (seconds) and `tracks`, in disc and track order.

### Album release

With MusicBrainz identification on (`musicbrainz` in the [settings](#server-settings)), each
album is matched to the MusicBrainz release it is, once its recordings are looked up: the
release its tags name (`MUSICBRAINZ_ALBUMID`), else the best fit among the releases holding
its recordings (or, without recording ids, found by title and artist), judged by how its tracks
pair with the release's (same recording, title, length, position), how much of the release it
holds, and its title and artist; of releases with the same tracks, the earliest.
`GET /api/v1/albums/{id}/release`:

```json
{"status": "matched", "how": "auto", "score": 0.97,
 "release": {"mbid": "1b022e01-…", "title": "Nevermind", "artist": "Nirvana",
             "date": "1991-09-24", "original_date": "1991-09-24", "country": "US",
             "status": "Official", "label": "DGC", "catalog_number": "DGCD-24425",
             "barcode": "720642442524", "format": "CD", "release_type": ["Album"],
             "track_count": 12, "disc_count": 1,
             "url": "https://musicbrainz.org/release/1b022e01-…"},
 "tracks": [{"track_id": 101, "disc": 1, "position": 1, "number": "1",
             "title": "Smells Like Teen Spirit", "artist": "Nirvana", "length": 301.92,
             "recording": "5fb524f1-…"}, "…"],
 "candidates": [{"mbid": "1b022e01-…", "score": 0.97, "release": { "…": "as above" }}, "…"]}
```

`status`: `matched` (a confident match, or one the tags name or the user chose), `uncertain`
(worth checking: `score` under 0.8, or a release whose title isn't the album's), `none` (no release found; tried again after 90 days) or
`pending` (not matched yet).  `how`: `tag`, `auto` or `user`.  `tracks` lists the album's
tracks; one without `position` is none of the release's.  `candidates` are the releases
considered, best first.  A `matched` release fills in what the tags leave out: the album's
MusicBrainz id and year, the tracks' MusicBrainz ids, numbers and discs.  Files are never
written.

`PUT` with `{"mbid": "<release id>"}` matches the album to that release (fetched from
MusicBrainz; `502` when it can't be asked) and returns the result; `DELETE` forgets the match,
so the album is matched again.  A user's choice is kept when the album gains tracks.

### Tags

`GET /api/v1/albums/{id}/tags` lists each of the album's files with every tag RMS can write,
as it is in the file (`current`) and as the album's release says (`proposed`; `?release=`
another release considered for it), `changed` when they differ:

```json
{"release": { "…": "as in Album release" }, "blocked": "…",
 "tracks": [{"track_id": 102, "path": "/srv/music/Nirvana/Nevermind/02.flac",
             "tags": [{"field": "title", "current": "In Blom", "proposed": "In Bloom", "changed": true},
                      {"field": "track_number", "current": "2", "proposed": "2", "changed": false}, "…"]}]}
```

The fields: `title`, `artist`, `album`, `album_artist`, `track_number`, `track_total`,
`disc_number`, `disc_total`, `date`, `original_date`, `label`, `catalog_number`, `barcode`,
`musicbrainz_recording_id`, `musicbrainz_track_id`, `musicbrainz_release_id`,
`musicbrainz_release_group_id`, `musicbrainz_artist_id`, `musicbrainz_album_artist_id`.  A
track's `error` says why its file can't be changed (a cue-sheet track, a format RMS can't
write, no write permission).

`POST /api/v1/tags` writes them:
`{"changes": [{"track_id": 102, "tags": {"title": "In Bloom", "label": null}}]}` (`null`
removes the tag) → `{"batch": 7, "changed": 1, "failed": [{"track_id": …, "error": …}]}`.
Only the tags named change; the files' folders are rescanned.  It answers `403` unless file
changes are on (`edit_files` in the settings) and the server has a login (`authorize`, a user
name and a password): `blocked` and the settings' `edit_files_blocked` say which.
`GET /api/v1/tags/edits` lists the changes made, newest first (`{batch, kind, album, files,
applied_at, undone_at}`, `kind` `tags`, `move` or `cover`), and `POST /api/v1/tags/edits/{batch}/undo` puts back the tags
they replaced, except in files changed again since.

### Files

`GET /api/v1/albums/{id}/files` shows where the album's files are and where the name template
(`file_name_template` in the settings, or `?template=`) puts them, relative to their music
folder:

```json
{"template": "{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}",
 "files": [{"track_id": 101, "from": "incoming/nevermind/a.flac",
            "to": "Nirvana/Nevermind (1991)/01 Smells Like Teen Spirit.flac", "changed": true},
           {"from": "incoming/nevermind/cover.jpg", "to": "Nirvana/Nevermind (1991)/cover.jpg",
            "changed": true}]}
```

A template is a path of literal text and tags: `{title}`, `{artist}`, `{album}`,
`{album_artist}` (else the artist), `{track}`, `{track_total}`, `{disc}` (only when the album
has several discs), `{disc_total}`, `{year}`, `{date}`, `{original_year}`, `{original_date}`,
`{label}`, `{catalog_number}`; `{track:02}` pads a number.  Text in `[…]` is left out when a
tag in it is empty.  The file's extension is kept.  Files without `track_id` are the other
files of a folder whose audio all moves to one place.  `error` says why a file can't move:
another file goes to the same place, a file is there already, it is a cue-sheet track.  A bad
template answers `400`.

`POST /api/v1/albums/{id}/files` (`{}`, or `{"template": …}`) moves them, under the same
conditions as writing tags, and answers once the library has caught up:
`{"batch": 8, "changed": 2, "failed": [], "album_id": 42}` (the album's id may change).
Ratings, play history, saved playlists, favorites and players' queues follow the files.
`POST /api/v1/tags/edits/{batch}/undo` moves them back.

### Artist pages

`GET /api/v1/artists/{id}/top-tracks` lists up to 10 of the artist's tracks that have been
played, most played first (empty until some have).  `GET /api/v1/artists/{id}/similar` lists
up to 12 artists [ListenBrainz](#listenbrainz) finds similar, those in the library first with
their `artist`; it is empty unless discovery is on or a ListenBrainz account is added (it asks
ListenBrainz about the artist), and for artists without a MusicBrainz id.

### Playlists

Saved playlists live as `.m3u` files in the playlist folder (`<data dir>/playlists` unless
set).  `POST /api/v1/playlists` makes one: `{"name": "Road trip"}` (empty),
`{"name": …, "item": Item}` (what the item stands for: a track, an album, another playlist, …)
or `{"name": …, "from_player": "<player id>"}` (that player's queue).  Names are unique (400
for one taken).  Only `editable` playlists can be renamed, deleted or changed: playlist files
among the music and cue sheets are the music's own (400); make a copy with `item`.

`PUT …/tracks` with `{"order": [2, 0, 1]}` keeps the tracks listed at those indexes in that
order and removes the others.  Entries whose files are missing aren't listed; they are kept,
at the end.  `POST …/tracks` adds what an [Item](#commands) stands for before track
`position`, or at the end; Spotify and radio menu items can't be added.

### Cover art

`GET /api/v1/albums/{id}/cover-art` lists the Cover Art Archive's images of the album's
release (`?release=` another release; the release group's when the release has none), front
cover first, and what the album has now:

```json
{"release_mbid": "1b022e01-…", "files": 12, "embedded": 0, "folder_image": "folder.jpg", "archive": false,
 "images": [{"id": "1234", "types": ["Front"], "front": true,
             "thumbnail": "https://coverartarchive.org/release/…/1234-500.jpg"}, "…"]}
```

`embedded` counts the files with an embedded front cover, which players and RMS show before a
picture in the folder (`folder_image`).  `folder_shared` is true when the album's folder holds other music
too (other albums' files, or tracks without an album tag): a picture there would be theirs as
well, so `folder` is refused (400).  `archive` is true when the album, having no art of its
own, shows the archive's front cover of its release (fetched in the background with
MusicBrainz identification on; the files are not changed).  Without a matched release `images` is empty.
`502` when the archive can't be asked.

`POST` with `{"image": "1234", "folder": true, "embed": true}` downloads that image (only one
the archive lists for the release; about 1200 pixels, JPEG or PNG) and saves it as
`cover.jpg` (`.png`) in the album's folder, embeds it as each file's front cover, or both —
under the same conditions as writing tags.  A `cover`, `folder`, `album` or `thumb` picture
in the folder is set aside; the files' other pictures and tags stay.  The answer is like
writing tags' (`{batch, changed, failed}`), and `POST /api/v1/tags/edits/{batch}/undo` puts
the old covers back.

### Folder entry

```json
{"type": "track", "id": 1234, "name": "01 So What.flac", "track": { "…": "a Track" }}
```

`GET /api/v1/folders` lists the music folders; `?id=` the contents of one.  `type` is
`folder` (`id` is the folder's: list it with `?id=` or queue it as `{"type": "folder"}`),
`track` (with the `track`), `playlist` (a playlist file: `{"type": "playlist"}`) or `other`
(a file that can't be played as it is).

### Finding tracks

`GET /api/v1/tracks` with only `q` lists tracks as the players do.  These parameters find
tracks by what the library knows about them; every one given must hold:

| parameter | |
|---|---|
| `q` | search words, as everywhere |
| `artist`, `album` | ids (the artist in any role) |
| `genre` | genre ids, comma-separated: any of them |
| `year_from`, `year_to` | |
| `bpm_min`, `bpm_max` | tempo; tracks with no known tempo are left out |
| `key` | Camelot codes, comma-separated (`8A,9A`) |
| `compatible_with` | a Camelot code: the keys that mix with it (itself, the next and previous number, its relative major or minor) |
| `rating_min` | 0–100 |
| `played` | `true`: played at least once; `false`: never |
| `played_since`, `not_played_since` | Unix times; never played counts as not played since |
| `added_since` | Unix time |
| `duration_min`, `duration_max` | seconds |
| `sort` | `title` (default), `random`, `added` (newest first), `most_played`, `least_recently_played` (never played first), `recently_played`, `rating`, `bpm` (slowest first), `year` (oldest first) |
| `detail` | `true`: each track with its `detail` |

```sh
# upbeat, rated, not heard for a month, in keys that mix with A minor
curl 'http://localhost:9000/api/v1/tracks?bpm_min=118&bpm_max=130&rating_min=60&not_played_since=1789000000&compatible_with=8A&sort=random&limit=30&detail=true'
```

### The Table

The library as a table: one row per track, or per album, artist, genre, year, … (`group`),
with the columns asked for.  `POST /api/v1/query`:

```json
{"q": "genre:jazz year:1950..1969 -format:mp3", "group": "album",
 "columns": ["album", "album_artist", "count", "year", "duration", "format"],
 "sort": [{"field": "count", "desc": true}], "offset": 0, "limit": 200}
```

→

```json
{"total": 37, "offset": 0, "tracks": 412, "duration": 151023.5,
 "rows": [{"key": 812, "label": "Kind of Blue", "drill": "album_id:812", "next": "track",
           "cells": ["Kind of Blue", "Miles Davis", 6, 1959, 2754.1, "flc"]}, "…"]}
```

`total` counts rows; `tracks` and `duration` are the tracks the query finds.  A row of
tracks has the track's `id`; a grouped row has what it stands for (`key`, `label`), and
`drill`, query words that find its tracks (add them to `q`), to show in the grouping `next`.
A grouped row's cell is the value its tracks share, `{"varies": n}` when they differ, a
total (lengths, plays, sizes), an average (rating, loudness), the latest (added, last played)
or `[min, max]` (years, tempo); `count` is its number of tracks.  Pages after the first come
from the first's result, so scrolling is quick; asking from `offset` 0 again works it out
afresh.

`q` is words, searched as everywhere, and `field:value` filters, all of which must hold:

| | |
|---|---|
| `artist:radiohead` | text contains, ignoring case (and accents in titles and names; `artist`: any of the track's artists) |
| `album:="ok computer"` | exactly (quotes around values with spaces) |
| `genre:jazz,soul` | any of them |
| `year:1990..1999`, `year:>=2000`, `bpm:<100`, `rating:80` | numbers: ranges, comparisons, equality |
| `duration:>5:00`, `size:>1gb`, `bitrate:>=320` | in their units (`90s`, `5m`; `700mb`; kbit/s) |
| `added:<30d`, `last_played:>1y`, `added:2024`, `added:2023-06..2024` | times: less or more than an age ago (`h`, `d`, `w`, `mo`, `y`), or dates |
| `lossless:yes`, `analyzed:no` | yes / no |
| `-format:mp3`, `-live` | not |
| `missing:mbid`, `has:genre` | without / with a value |
| `varies:album_artist` | grouped rows whose tracks differ in it |
| `happy:>70`, `danceable:40..80` | EffNet's moods and qualities, in percent |
| `differs:title` | the tag isn't what MusicBrainz says (`title`, `artist`, `album`, `album_artist`, `year`; tracks of a matched album) |

A word with a colon in it is quoted: `"re:birth"`.  A mistake is a `400` whose `error` says
what's wrong.

`GET /api/v1/query/fields` lists the fields: `{"name": "year", "label": "Year", "kind": "int",
"section": "Tags", "agg": "range", "next": "album"}`.  `kind` is `text`, `int`, `real`,
`duration` (seconds), `time` (Unix), `bool`, `size` (bytes), `bitrate` (bit/s) or `percent`
(0–1); `section` groups them for a column chooser; `agg` what a grouped row shows (`same`,
`sum`, `avg`, `max`, `range`); a field with `next` can be grouped by.  The fields:

* tags and audio: `title`, `artist`, `album_artist`, `album`, `track`, `disc`, `genre`, `year`,
  `decade`, `duration`, `format`, `bitrate`, `sample_rate`, `bits`, `channels`, `lossless`,
  `release_type`, `compilation`, `mbid`, `isrc`, `size`, `folder`, `path`;
* listening and analysis: `rating`, `plays`, `skips`, `last_played`, `added`, `bpm`, `key`,
  `loudness`, `replay_gain`, `analyzed`;
* MusicBrainz, from the release the album was [matched](#album-release) to (what the Tags
  page proposes) and the recording identified: `mb_title`, `mb_artist`, `mb_album`,
  `mb_album_artist`, `mb_date`, `mb_original_date`, `mb_year`, `label`, `catalog_number`,
  `barcode`, `country`, `release_status`, `medium` (`CD`, `Digital Media`, …), `mb_match`
  (how the album was matched), `mb_score`, `mb_identified` (how the recording was found:
  `tag`, `isrc`, `acoustid`, `search`, `none`), `mb_genres`, `release_mbid`,
  `release_group_mbid`, `artist_mbid`;
* EffNet (when that analysis is on): `style` (the likeliest Discogs style, `Genre / Style`),
  `styles` (the likeliest few), `theme`, `instrument`, and the moods and qualities, 0–1:
  `happy`, `sad`, `relaxed`, `aggressive`, `party`, `danceable`, `acoustic`, `electronic`,
  `voice`, `approachability`, `engagement`;
* for filters only: `id`, `album_id`, `artist_id`, `album_artist_id`, `genre_id`.

`GET /api/v1/query/views` lists the saved views, and `PUT` replaces them (names must differ):
`[{"name": "Unplayed jazz", "q": "genre:jazz plays:0", "group": "album", "columns": ["album",
"count"], "sort": [{"field": "added", "desc": true}], "widths": {"album": 280}}]`.

### Uploading music

`POST /api/v1/tracks?name=01%20Song.flac` with a music file as the body adds it to the
library: it is written into `Uploads/` of the first music folder (`folder=Artist/Album` puts
it in a folder inside that), scanned, and answered with `201` and the [Track](#track)s it
became, with their `detail` — one, or several for a file with an embedded cue sheet.  Then it
is a library track like any other: MusicBrainz identification and the
[file name template](#files) apply to it.

* `name` is only a file name (any path in it is dropped); its extension must be an audio
  type the scanner reads.  A name already taken gets a number: `01 Song (2).flac`.  Files are
  never overwritten.
* `folder` is a relative path inside `Uploads/`: plain folder names only.
* At most 1 GB.  A file the scanner can't read as audio (damaged, or not what its name says)
  is removed again and answered with `400`.
* Like [changing tags](#tags), it needs file changes on (`edit_files`) and a login, else
  `403` says why.

```sh
curl -u me:secret -X POST --data-binary @song.flac \
     'http://localhost:9000/api/v1/tracks?name=Song.flac&folder=New%20Band/Debut'
```

### Similar songs

`GET /api/v1/tracks/{id}/similar` and `POST /api/v1/similar` list library songs similar to
seeds, scored as [Create Mix](#commands) scores them (how they sound once analysed, shared
genres and artists, listening history; at most one per album and two per artist, none that
sounds like the same recording), but without playing anything.  Only library tracks: no
ListenBrainz discoveries or streamed tracks.

```json
{"seeds": [{"type": "track", "id": 1234}, {"type": "artist", "id": 7}],
 "limit": 20, "exclude": [77, 5120], "detail": true}
```

`seeds` are tracks (`track`, `tracks`), albums, artists, genres or years; a big album, artist,
genre or year counts as a sample of its tracks.  `limit` is 20 by default, at most 200.
`exclude` leaves out tracks already chosen.  Queue the result with `enqueue` and
`{"type": "tracks", "ids": [...]}`.

### Analysis

After a scan, each local song's audio is analysed in the background (server setting
`sonicAnalysis`, on by default): bliss measures how it sounds and its loudness, the rhythm
analysis its tempo, beats, key and energy, Chromaprint fingerprints it, and EffNet (setting
`effnet`) recognises its styles and moods, in one pass over the audio.  MusicBrainz lookups
(setting `musicbrainz`) follow on their own, at one a second.  Analyses are stored by the
audio's fingerprint, so a moved or renamed file keeps them, and copies of one file share them.

`GET /api/v1/tracks/{id}/analysis` (404 for no such library track):

```json
{"track_id": 1234, "title": "Teardrop", "artist": "Massive Attack",
 "album": {"id": 56, "title": "Mezzanine", "artist": "Massive Attack", "artist_id": 12, "…": "…"},
 "file": {"url": "file:///music/…", "path": "/music/…", "fingerprint": "c8a3e450…",
          "format": "flc", "size": 31457280, "bitrate": 911000, "vbr": true,
          "sample_rate": 44100, "sample_size": 16, "channels": 2, "lossless": true,
          "duration": 330.2, "replay_gain": -7.6, "replay_peak": 0.98},
 "bliss": {"status": "done", "version": 2, "current_version": 2, "analyzed_at": 1791948472,
           "result": {"features": [{"name": "tempo", "value": 0.18}, …],
                      "loudness": -10.3, "peak": 0.98, "duplicate_distance": 0.12,
                      "nearest": [{"id": 77, "title": "…", "artist": "…", "score": 0.05}, …]}},
 "rhythm": {"status": "done", …, "result": {"bpm": 77.4, "beat_confidence": 0.82,
            "beats": 425, "bars": 106, "first_downbeat": 0.52, "key": "8A",
            "key_name": "A minor", "key_confidence": 0.41, "energy": [-30.1, …],
            "energy_step": 3.1}},
 "chromaprint": {"status": "done", …, "result": {"items": 968, "duration": 120.0,
                 "duplicates": [{"id": 5120, "title": "…", "same_recording": true}]}},
 "effnet": {"status": "off", "current_version": 1},
 "musicbrainz": {"status": "done", "current_version": 0, "analyzed_at": 1791948500,
                 "result": {"mbid": "…", "isrc": "GBAAA9800001", "genres": ["trip hop"],
                            "how": "isrc"}}}
```

Each analysis has a `status`:

| status | |
|---|---|
| `done` | current results in `result` |
| `failed` | analysed by the current `version`, but nothing came of it (unreadable, too short, not found on MusicBrainz); tried again when the file changes |
| `stale` | results of an older `version`, to be redone |
| `pending` | not analysed yet |
| `skipped` | never analysed: longer than 20 minutes, or not a local file |
| `off` | turned off in the server settings |

- `bliss.features`: bliss's numbers, about -1 to 1, in its order (tempo, zero-crossing rate,
  spectral centroid, rolloff and flatness as mean and `std`, loudness mean and `std`, then
  chroma).  `nearest`: the ten library tracks that sound closest, with their distance; below
  `duplicate_distance` they are taken for the same recording.  `loudness` is LUFS (EBU R128),
  `peak` linear true peak.
- `rhythm.bpm` is 0 when there is no steady beat; below a `beat_confidence` of about 0.3,
  mixes don't beat-match.  `energy` is the loudness per bar from the first downbeat (per 2
  seconds without a beat), in dB, `energy_step` seconds apart.
- `chromaprint.duplicates`: tracks bliss finds that close, and whether their fingerprints
  confirm it is the same recording.
- `effnet.result`: `labels` (styles, moods, themes and instruments with their scores, as in
  a track's `detail.kind`) and `nearest`, the ten tracks closest in kind by cosine similarity.
- `musicbrainz.result.how`: `tag`, `isrc`, `acoustid` or `search`; `none` (with status
  `failed`) when the recording wasn't found.

`GET /api/v1/library/analysis` counts the library's distinct audio for each analysis:

```json
{"running": true,
 "bliss": {"enabled": true, "total": 8120, "done": 6002, "failed": 3, "pending": 2115},
 "rhythm": {…}, "chromaprint": {…}, "effnet": {"enabled": false, …}, "spectrum": {…},
 "musicbrainz": {…}}
```

`running`: the background pass over the audio is under way.  `pending` includes results of
an older version.

### Search

`GET /api/v1/search?q=blue%20miles` matches every word at the start of a word of the name,
title, album or artist, and returns up to `limit` (default 20) of each kind:

```json
{"artists": Page, "albums": Page, "tracks": Page, "genres": Page}
```

## Favorites, internet radio and Spotify

All are menus of [BrowseItem](#browseitem)s, the same the player menus show.

### BrowseItem

```json
{"id": "0.1", "name": "Radio Paradise", "type": "audio", "playable": true,
 "url": "http://stream.radioparadise.com/flac", "artwork": "imageproxy/…/image"}
```

* `type` `folder`: has items; fetch them with `?id=<id>`.  `audio`: something to play.
  `search`: a search box; fetch `?id=<id>_<url-encoded words>` for the results.  `text`:
  information only.
* `playable` items are queued with the `enqueue` command as `{"type": "favorite", "id": …}`
  or `{"type": "spotify", "id": …}` (a Spotify playlist or album as a whole), or a radio
  station as `{"type": "radio", "id": …}`, a ListenBrainz item as
  `{"type": "listenbrainz", "id": …}`.

### ListenBrainz

Accounts are added with a user token from <https://listenbrainz.org/settings/>
(`POST /api/v1/listenbrainz/accounts`, which checks it with ListenBrainz; 422 when refused).
What plays is scrobbled to them: *now playing* when a song starts, a listen once it has played
for half its length or 4 minutes (if longer than 30 seconds), with the MusicBrainz ids and
ISRC the library knows.  Players scrobble to the server settings' `scrobble_to` accounts, which
are the `default` ones unless set; Subsonic apps' `scrobble` calls go to the default
accounts.  Listens wait in a queue while ListenBrainz can't take them (`scrobble_pending`,
`scrobble_error` in the server settings).

`GET /api/v1/listenbrainz` lists, for each account, `recs:<user>` (what ListenBrainz
recommends for the user) and the playlists it made for them (`pl:<id>`: Daily Jams, Weekly
Jams, Weekly Exploration, …) as folders; under them, recordings (`rec:<mbid>`).  Each is
playable: its recordings play from the library when it has them (by MusicBrainz id, else
artist and title), else from Spotify when signed in; what can't be found is left out (422
when nothing can).

### Internet radio

`GET /api/v1/radio` lists the radio providers as folders: `radiofavorites` (your favorites
that are streams, when there are any), `radiodirectory` (a few well-known stations) and
`radiobrowser` ([Radio Browser](https://www.radio-browser.info), the community directory of
tens of thousands of stations: search, popular, top voted, by country, genre and language).
`?id=<provider>` and the ids under it browse each one; a search is
`?id=radiobrowser.0_<url-encoded words>`.

### Spotify

`GET /api/v1/spotify` is 404 when this server has no Spotify support, otherwise
`{"signed_in": true, "user": "…", "product": "premium", "error": "…", "connect_error": "…",
"learn": true, "learned": 120}`.  `user`: the account's display name; `error`: why the
account can't be reached right now.  `connect_error`: why a player isn't available as a Spotify
Connect device (RMS tries again later and later, up to every 30 minutes).  `learn`: what Spotify tracks sound like is learned as they play, for
mixes; `learned`: how many are.  `PATCH /api/v1/spotify` with `{"learn": false}` turns it
off; `DELETE /api/v1/spotify/learned` forgets them.  `GET /api/v1/spotify/learned` lists
them, the latest learned first: `{"items": [LearnedTrack]}`, each `{"url":
"spotify://track:…", "title", "artist", "album", "year", "duration", "artwork", "bpm", "key"
(Camelot), "loudness" (LUFS), "genres" (MusicBrainz), "style" (the likeliest Discogs style by
its sound), "in_library" (the library has a copy), "learned_at" (Unix seconds)}`; what isn't
known is left out.

Signing in: `POST /api/v1/spotify/login` with `{}` returns `{"url": …}`, the Spotify page to
open.  Spotify then sends the browser to `http://127.0.0.1:5588/login?code=…`.  On the
server's own computer RMS receives that by itself (watch `signed_in`); elsewhere, post the
address the browser ended up on as `{"code": "<address>"}`, which returns `{"user": …}`.
`POST /api/v1/spotify/logout` signs out.

While a player plays its Spotify Connect device (its queue then holds one endless
`spotify://connect:…` stream, named "Spotify Connect"), the tracks the Spotify app plays next
are at `GET /api/v1/players/{id}/spotify-queue`: `{"connect": true, "items": [Track…],
"error": "…"}`, the items `spotify://track:…` URLs with title, artist, album, duration and
artwork, as Spotify last told the Connect device.  `connect` is false (no items) when the
player plays something else; `error` says why the queue can't be shown.  The app's queue
changes without an event: ask again on a track change or now and then.

## Settings

### Server settings

```json
{"library_name": "Music", "media_dirs": ["/srv/music"], "watch_folders": true,
 "upnp_players": false, "chromecast_players": false, "airplay_players": false,
 "radio_direct": false, "art_folder": "",
 "online_artist_images": false, "spotify_bitrate": 320,
 "spotify_normalisation": false, "musicbrainz": false, "acoustid_key": "",
 "musicbrainz_identified": 0, "musicbrainz_pending": 0, "albums_matched": 0,
 "albums_uncertain": 0, "albums_pending": 0, "edit_files": false,
 "edit_files_blocked": "…", "file_name_template": "{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}",
 "listenbrainz": false,
 "discovered": 0, "discovered_played": 0, "discovered_rejected": 0,
 "listenbrainz_accounts": [{"user": "alice", "default": true}], "scrobble_spotify": false,
 "scrobble_pending": 0, "scrobble_error": "…", "effnet": false, "effnet_status": "ready",
 "effnet_error": "…", "effnet_analyzed": 0, "effnet_pending": 0,
 "visualizer_available": true, "visualizer_error": "…", "visualizer_encoder": "h264_vaapi",
 "visualizer_size": Choice, "visualizer_render_size": Choice, "visualizer_fps": Choice,
 "visualizer_quality": {"value": 30, "min": 18, "max": 45},
 "visualizer_preset_seconds": {"value": 30, "min": 5, "max": 600},
 "visualizer_receiver_app": "",
 "transition_type": 1, "transition_duration": 5, "replay_gain_mode": 0,
 "dont_stop_the_music": {"value": "0", "options": [{"value": "0", "label": "Disabled"}, "…"]},
 "adventurous": 15, "scrobble_to": ["alice"]}
```

`PATCH /api/v1/settings` with some of these fields changes them and returns all of them.
Unknown fields are an error.  `watch_folders` scans what changes in the music folders as
it changes (see the README).  `radio_direct`: players fetch internet radio from the station
themselves, as in Lyrion, rather than through the server (then radio has no
[spectrum](#spectrum)).  `albums_matched`, `albums_uncertain` and `albums_pending` count
albums by [release match](#album-release).  `effnet` turns on the EffNet analysis
(downloading its models first); `effnet_status` is `off`, `downloading`, `ready` or `failed`
(with `effnet_error`), and `effnet_analyzed` / `effnet_pending` count the songs done and to do.
The `visualizer_*` fields set up the TV visualiser (see the README): `visualizer_available`
says whether `pmviz` was found (else `visualizer_error` says why), `visualizer_encoder` is
read only, the choices (`visualizer_size`, `visualizer_render_size`, `visualizer_fps`) are
set to one of their options' `value`s and the ranges (`visualizer_quality`, the encoder's
QP / CRF; `visualizer_preset_seconds`) to a number within them.
`visualizer_receiver_app` is the Cast receiver app Chromecasts play in (8 hex digits), or
empty for RMS's own (`8745C385`).  The frame rate, drawing size and preset time apply to Chromecasts
that draw the visualiser themselves too.  Changes apply from the next track.

The playback settings are the same for every player (a player's `playerpref` for them, as
Jive and Material set them, changes them for all):
* `transition_type`: 0 none, 1 crossfade, 2 fade in, 3 fade out, 4 fade in and out;
  `transition_duration` 0–10 seconds.
* `replay_gain_mode`: 0 off, 1 track gain, 2 album gain, 3 smart (album gain when playing
  a whole album).
* `dont_stop_the_music`: what plays when the queue runs out, a choice set to one of its
  options' `value`s (`"0"`: nothing).
* `adventurous`: percent (0–40) of Similar Songs and Create Mix discovered with ListenBrainz
  (when `listenbrainz` is on).
* `scrobble_to`: the [ListenBrainz](#listenbrainz) accounts (user names) players scrobble
  to; set it to choose (`[]`: none).

Changed music folders take effect with the next scan:

`POST /api/v1/library/scan` with `{"mode": "changes"}` (the default: new, changed and
removed files), `{"mode": "playlists"}` or `{"mode": "full"}` (clear the library and scan
everything again; listening history and analysis are kept).  Progress arrives as `server`
events.

### Player settings

```json
{
  "max_bitrate": 0, "play_delay": 0, "digital_volume_control": true,
  "display": {
    "playing_mode": {"value": "5", "options": [{"value": "0", "label": "Nothing"}, "…"]},
    "text_size": Choice, "text_size_off": Choice,
    "brightness_on": Choice, "brightness_off": Choice, "brightness_idle": Choice,
    "auto_brightness_min": {"value": 2, "min": 1, "max": 5},
    "auto_brightness_sensitivity": {"value": 10, "min": 1, "max": 20}
  }
}
```

* `max_bitrate` in kbps, 0 for no limit.
* A choice (the display settings) is set to one of its options' `value`s.
* Transitions, ReplayGain, Don't Stop The Music, Adventurous and who to scrobble to are the
  same for every player: they're in the [server settings](#server-settings).
* `display` is there for players with a screen (Squeezebox 2/3, Transporter, Boom); the
  automatic brightness ranges only for a Boom.
* `play_delay`: milliseconds (0–1000) the player's sound comes out later than it says (TV
  speakers, a soundbar, Bluetooth), so it lines up with the players it is synced with.
* `visualizer` (a choice of `auto`, `1` the video from the server, `tv` drawn on the TV,
  and `0` off: the receiver app shows the cover) is there for Chromecasts.  `auto` draws
  on a Chromecast with a screen, unless it was seen not to keep up (or has no WebGL 2):
  then the video from the server where pmviz is installed, else the cover.  Setting it
  (to `auto` too) tries drawing on the TV again.

`PATCH /api/v1/players/{id}/settings` with some of these fields (a choice as its value)
changes them and returns all of them.  Nothing is changed when one of them is invalid.

The player's name is changed with the `rename` command.

## HTTPS

The API is served over HTTPS too (`--https-port`, 9443; `https_port` in `GET /api/v1/server`).  When
the certificate comes from the server's own authority (`local_ca`), its certificate is at
`GET /tls/rms-ca.crt` (DER) and `GET /tls/rms-ca.pem` (outside `/api/v1`).

## Browser player

`GET /api/v1/browser-player?id=<browser id>&name=<player name>&formats=flc,mp3,ogg,ops,aac`
opens a WebSocket that makes the browser a player for as long as it is open (the web UI's
*Play in this browser*).  `id` is the browser's own id, so it is the same player every time;
`formats` are what its `<audio>` element plays, best first.

A browser that plays through Web Audio adds `clock=1` and `rate=<its audio output's rate>`.
It asks for `formats=flc,pcm` when it decodes FLAC (WebCodecs), `ops,flc,pcm` to save data:
listing Opus first gets Opus (160 kbit/s, 48 kHz) for every track, unless a player it is
synced with can't take it.  Otherwise it asks for `pcm`: raw samples, described by the `pcm`
field of `load` and `preload` (`bits`, `rate`, `channels`, `little_endian`).  It keeps time like a Squeezebox,
so the server keeps it in sync with the players it is synced with: its first message is
`{"type": "clock", "jiffies": <ms>}` (its clock, in milliseconds, wrapping at 2³²), every
`progress` carries `jiffies`, the moment of that clock at which `elapsed` is heard, and it
answers `resume` with `at` (start at that moment of its clock), `hold` (`ms` of silence) and
`skip` (`ms` of audio left out).  It reports `fetched` once a track's audio is all in, which
asks for the next track.

The server sends `{"type": "welcome", "player": "<player id>"}`, then `ToRenderer` messages
(`api-types.d.ts`): `load` a URL (relative to the server root) and play it, or with
`autostart: false` wait for `resume`; `preload` the next track, to start when the current
one ends; `pause`, `resume`, `stop`; `volume` (0–100, `muted`).  The browser reports
`FromRenderer` messages: `ready`, `started` (a loaded or preloaded track began), `progress`
(`elapsed` and `duration` seconds, about once a second), `paused`, `resumed`, `ended` (with
nothing preloaded), `error`; and with Web Audio `clock` and `fetched`.

One page of a browser plays at a time (its tabs and windows share its `id`).  While one
plays, another that connects gets `{"type": "busy"}` and is closed (code 4000); with
`takeover=1` (the user's *Play here*) it plays instead, and the page that played gets
`{"type": "replaced"}` and is closed (code 4000).  A page told either shouldn't take the
player back by itself: the web UI looks again every 10 seconds without `takeover`, so it
plays once the other page has gone.

## LMS interfaces

JSON-RPC (`/jsonrpc.js`), Cometd (`/cometd`) and the CLI (port 9090) stay for Squeezebox
players, SqueezePlay/Squeezer, Home Assistant and other existing clients.  What these
clients send is recorded in `crates/rms/tests/golden/` and checked by
`cargo test -p rms --test it conformance`.
