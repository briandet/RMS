# RMS native API (v1)

A small JSON API for controlling RMS, with state pushed to clients instead of polled.  The
web UI uses it for players, playback and the queue.  It lives on the HTTP port (9000 by
default) under `/api/v1`.  When `authorize` is on it needs the same HTTP Basic login as the
web UI.

The LMS interfaces (JSON-RPC, Cometd, CLI) are still there for Squeezebox hardware,
SqueezePlay/Squeezer and other existing clients.  The native API is a separate surface with its
own conventions:

* Field names are `snake_case`.  Enums are lower-case strings.  Booleans are `true`/`false`.
* An absent value is left out (no `""`, `0` or `-1` stand-ins).
* Errors are `{"error": "<message>"}` with status 400 (bad request), 404 (unknown player or
  path) or 422 (the server could not carry it out, e.g. the player is disconnected).

## Resources

| | |
|---|---|
| `GET /api/v1/server` | [Server](#server) |
| `GET /api/v1/players` | list of [Player](#player) |
| `GET /api/v1/players/{id}` | [Player](#player) |
| `GET /api/v1/players/{id}/queue?offset=&limit=` | [Queue](#queue) |
| `POST /api/v1/players/{id}/command` | run a [command](#commands) |
| `GET /api/v1/events` | [event stream](#events) |

`{id}` is the player's MAC address (`00:04:20:12:34:56`).

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
```

```sh
curl -X POST http://localhost:9000/api/v1/players/00:04:20:12:34:56/command \
     -H 'Content-Type: application/json' \
     -d '{"command": "enqueue", "item": {"type": "album", "id": 42}}'
```

## Not covered yet

Library browsing and search, favorites, preferences and Spotify are still only available
over JSON-RPC.  The web UI uses JSON-RPC for those parts.
