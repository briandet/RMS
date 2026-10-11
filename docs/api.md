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
| `POST /api/v1/players/{id}/command` | run a [command](#commands) |
| `GET`, `PATCH /api/v1/players/{id}/settings` | [player settings](#player-settings) |
| `GET /api/v1/events` | [event stream](#events) |
| `GET /api/v1/browser-player` | WebSocket: [this browser as a player](#browser-player) |
| `GET /api/v1/artists?q=` | page of [Artist](#artist) |
| `GET /api/v1/artists/{id}` | [Artist](#artist) |
| `GET /api/v1/albums?sort=&artist=&genre=&year=&q=` | page of [Album](#album) |
| `GET /api/v1/albums/{id}` | [Album](#album) with `duration` and `tracks` |
| `GET /api/v1/genres?q=` | page of `{id, name}` |
| `GET /api/v1/years` | page of `{year}` (0: tracks without a year) |
| `GET /api/v1/tracks?q=` | page of [Track](#track) |
| `GET /api/v1/playlists?q=` | page of `{id, name}` (saved playlists) |
| `GET /api/v1/playlists/{id}` | `{id, name}` |
| `GET /api/v1/playlists/{id}/tracks` | page of [Track](#track) |
| `GET /api/v1/folders?id=` | page of [folder entries](#folder-entry) |
| `GET /api/v1/search?q=&limit=` | [search results](#search) |
| `GET /api/v1/favorites?id=` | page of [BrowseItem](#browseitem) |
| `POST /api/v1/favorites` | add a favorite: `{"url": …, "title": …}` |
| `DELETE /api/v1/favorites/{id}` | remove a favorite or a folder of them |
| `GET /api/v1/spotify` | [Spotify status](#spotify) |
| `GET /api/v1/spotify/items?id=` | page of [BrowseItem](#browseitem) |
| `POST /api/v1/spotify/login`, `/logout` | [sign in and out](#spotify) |
| `GET`, `PATCH /api/v1/settings` | [server settings](#server-settings) |
| `POST /api/v1/library/scan` | [scan the library](#server-settings) |

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
  "library": {"albums": 310, "artists": 120, "genres": 25, "tracks": 4000, "duration": 980000.0}
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
* For radio, `station` is the station's name.  `title` and `artist` are the current song,
  split from the stream's "Artist - Title" metadata when needed.

### Queue

```json
{"version": 1791694516.45, "length": 12, "index": 2, "offset": 0, "items": [ "…Tracks" ]}
```

Item `i` is queue entry `offset + i`.  By default the whole queue is returned.

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
{"type": "album", "id": 42}
{"type": "artist", "id": 7}
{"type": "genre", "id": 3}
{"type": "year", "year": 1999}
{"type": "playlist", "id": 9001}
{"type": "folder", "id": 50001}
{"type": "url", "url": "http://radio.example.com/stream"}
{"type": "favorite", "id": "0.1"}
{"type": "spotify", "id": "1.0.3"}
```

```sh
curl -X POST http://localhost:9000/api/v1/players/00:04:20:12:34:56/command \
     -H 'Content-Type: application/json' \
     -d '{"command": "enqueue", "item": {"type": "album", "id": 42}}'
```

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
 "compilation": false, "artwork": "music/ef34ab/cover", "letter": "K"}
```

`GET /api/v1/albums` sorts by `title` (default), `artist` (then year), `year` or `added`
(newest first), and filters by `artist`, `genre` and `year` ids and by search words `q`.
`GET /api/v1/albums/{id}` adds `duration` (seconds) and `tracks`, in disc and track order.

### Folder entry

```json
{"type": "track", "id": 1234, "name": "01 So What.flac", "track": { "…": "a Track" }}
```

`GET /api/v1/folders` lists the music folders; `?id=` the contents of one.  `type` is
`folder` (`id` is the folder's: list it with `?id=` or queue it as `{"type": "folder"}`),
`track` (with the `track`), `playlist` (a playlist file: `{"type": "playlist"}`) or `other`
(a file that can't be played as it is).

### Search

`GET /api/v1/search?q=blue%20miles` matches every word at the start of a word of the name,
title, album or artist, and returns up to `limit` (default 20) of each kind:

```json
{"artists": Page, "albums": Page, "tracks": Page, "genres": Page}
```

## Favorites and Spotify

Both are menus of [BrowseItem](#browseitem)s, the same the player menus show.

### BrowseItem

```json
{"id": "0.1", "name": "Radio Paradise", "type": "audio", "playable": true,
 "url": "http://stream.radioparadise.com/flac", "artwork": "imageproxy/…/image"}
```

* `type` `folder`: has items; fetch them with `?id=<id>`.  `audio`: something to play.
  `search`: a search box; fetch `?id=<id>_<url-encoded words>` for the results.  `text`:
  information only.
* `playable` items are queued with the `enqueue` command as `{"type": "favorite", "id": …}`
  or `{"type": "spotify", "id": …}` (a Spotify playlist or album as a whole).

### Spotify

`GET /api/v1/spotify` is 404 when this server has no Spotify support, otherwise
`{"signed_in": true, "user": "…", "product": "premium", "error": "…"}`.

Signing in: `POST /api/v1/spotify/login` with `{}` returns `{"url": …}`, the Spotify page to
open.  Spotify then sends the browser to `http://127.0.0.1:5588/login?code=…`.  On the
server's own computer RMS receives that by itself (watch `signed_in`); elsewhere, post the
address the browser ended up on as `{"code": "<address>"}`, which returns `{"user": …}`.
`POST /api/v1/spotify/logout` signs out.

## Settings

### Server settings

```json
{"library_name": "Music", "media_dirs": ["/srv/music"], "watch_folders": true, "art_folder": "",
 "online_artist_images": false, "spotify_client_id": "", "spotify_bitrate": 320,
 "spotify_normalisation": false}
```

`PATCH /api/v1/settings` with some of these fields changes them and returns all of them.
Unknown fields are an error.  `watch_folders` scans what changes in the music folders as
it changes (see the README).  Changed music folders take effect with the next scan:

`POST /api/v1/library/scan` with `{"mode": "changes"}` (the default: new, changed and
removed files), `{"mode": "playlists"}` or `{"mode": "full"}` (clear the library and scan
everything again; listening history and analysis are kept).  Progress arrives as `server`
events.

### Player settings

```json
{
  "transition_type": 1, "transition_duration": 5, "replay_gain_mode": 0, "max_bitrate": 0,
  "digital_volume_control": true,
  "dont_stop_the_music": {"value": "0", "options": [{"value": "0", "label": "Disabled"}, "…"]},
  "display": {
    "playing_mode": {"value": "5", "options": [{"value": "0", "label": "Nothing"}, "…"]},
    "text_size": Choice, "text_size_off": Choice,
    "brightness_on": Choice, "brightness_off": Choice, "brightness_idle": Choice,
    "auto_brightness_min": {"value": 2, "min": 1, "max": 5},
    "auto_brightness_sensitivity": {"value": 10, "min": 1, "max": 20}
  }
}
```

* `transition_type`: 0 none, 1 crossfade, 2 fade in, 3 fade out, 4 fade in and out;
  `transition_duration` 0–10 seconds.
* `replay_gain_mode`: 0 off, 1 track gain, 2 album gain, 3 smart (album gain when playing
  a whole album).  `max_bitrate` in kbps, 0 for no limit.
* A choice (`dont_stop_the_music` and the display settings) is set to one of its options'
  `value`s.
* `display` is there for players with a screen (Squeezebox 2/3, Transporter, Boom); the
  automatic brightness ranges only for a Boom.

`PATCH /api/v1/players/{id}/settings` with some of these fields (a choice as its value)
changes them and returns all of them.  Nothing is changed when one of them is invalid.

The player's name is changed with the `rename` command.

## Browser player

`GET /api/v1/browser-player?id=<browser id>&name=<player name>&formats=flc,mp3,ogg,ops,aac`
opens a WebSocket that makes the browser a player for as long as it is open (the web UI's
*Play in this browser*).  `id` is the browser's own id, so it is the same player every time;
`formats` are what its `<audio>` element plays, best first.

A browser that plays through Web Audio adds `clock=1` and `rate=<its audio output's rate>`
and asks for `formats=pcm`: raw samples, described by the `pcm` field of `load` and
`preload` (`bits`, `rate`, `channels`, `little_endian`).  It keeps time like a Squeezebox,
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

## LMS interfaces

JSON-RPC (`/jsonrpc.js`), Cometd (`/cometd`) and the CLI (port 9090) stay for Squeezebox
players, SqueezePlay/Squeezer, Home Assistant and other existing clients.  What these
clients send is recorded in `crates/rms/tests/golden/` and checked by
`cargo test -p rms --test conformance`.
