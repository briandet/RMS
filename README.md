# RMS — Rusty Music Server

A Rust implementation of [Lyrion Music Server](https://lyrion.org) (formerly Logitech Media
Server / SqueezeCenter), the server behind Squeezebox players.  It speaks the same protocols
as Lyrion 9.2 so existing players and controllers work unchanged:

| Client | Protocol | Status |
|---|---|---|
| squeezelite, piCorePlayer, **WiiM** (Pro, Pro Plus, Amp, Ultra…) | slimproto (TCP 3483) + HTTP streaming, UDP discovery | ✔ tested with squeezelite 2.0 |
| Squeezebox 2/3/Classic, Boom, Receiver, Transporter, Touch, Radio | slimproto | ✔ playback, Now Playing screens, on-screen menus with the IR remote |
| **Squeezer** (Android), Material skin, iPeng, SqueezePlay/Jive | Cometd (`/cometd`), JSON-RPC (`/jsonrpc.js`), Jive menus | ✔ |
| Scripts, home automation | Telnet CLI (port 9090), JSON-RPC | ✔ |
| Browser | built-in web UI at `http://<server>:9000/` | ✔ new responsive UI |

## Quick start

```sh
cargo build --release
./target/release/rms --music /path/to/music
```

Then open `http://<server>:9000/`, or point a player at the server:

* **squeezelite**: `squeezelite -s <server-ip>` (or no `-s` to use discovery).
* **WiiM**: WiiM Home app → device → Settings → *Squeezelite* → enable, then pick the
  server from the list (or *Connect by IP Address*).  The WiiM, like any squeezelite-based
  player, finds the server by UDP broadcast on port 3483, so it must be on the same LAN
  segment (Docker: use host networking).
* **Squeezer**: add the server by IP, port 9000.

`ffmpeg` should be installed: it is used for transcoding (e.g. formats a player cannot
decode, sample-rate limiting, bitrate limiting), seeking inside FLAC/AAC/Ogg, cue-sheet
tracks and HLS radio.  Without it RMS still streams files the player can decode natively.

### Options

| Flag | Env | Default | |
|---|---|---|---|
| `--music <dir>[,<dir>…]` | `RMS_MUSIC_DIR` | saved pref | music folders to scan |
| `--playlist-dir <dir>` | `RMS_PLAYLIST_DIR` | | where saved playlists are written |
| `--data-dir <dir>` | `RMS_DATA_DIR` | `./data` | prefs, database, artwork cache |
| `--http-port <n>` | `RMS_HTTP_PORT` | 9000 | web UI, JSON-RPC, Cometd, streaming |
| `--cli-port <n>` | `RMS_CLI_PORT` | 9090 | telnet CLI |
| `--name <name>` | `RMS_NAME` | host name | server name shown on players |
| `--rescan` / `--wipe` / `--scan-only` | | | library scanning at startup |
| `--log <filter>` | `RMS_LOG` | `info` | e.g. `info,rms=debug` |

Preferences are stored as YAML in `<data-dir>/prefs/server.prefs` and one
`<mac>.prefs` file per player, using the same pref names as Lyrion (`maxBitrate`,
`transitionType`, `replayGainMode`, `syncVolume`, `powerOnResume`, …); they can be changed
with the `pref` / `playerpref` commands or the web UI.

## What is implemented

* **slimproto** (crate `slimproto`): byte-exact codec for every client and server frame
  (`HELO`, `STAT`, `RESP`, `META`, `DSCO`, `SETD`, `IR`, … / `strm`, `audg`, `aude`,
  `setd`, `vers`, `serv`, `cont`, display frames), UDP discovery (`d`/`D` and TLV `e`/`E`).
* **Playback controller**: a port of `Slim::Player::StreamingController` including its
  full event × state transition table: gapless track hand-off on `STMd`, end-of-stream
  handling, rebuffering on output underrun, retry of failed radio streams, crossfade /
  fade-in transitions, replay gain (track/album/smart), sleep timer, alarms.
* **Synchronisation**: sync groups share one controller and playlist; synchronized start
  with `strm u` at a common player-clock time and continuous drift correction with
  `skipAhead` / `pauseFor` from accurate play points.
* **Streaming**: local files are sent natively when the player can decode them, otherwise
  transcoded with ffmpeg (LMS rules: prefer native, honour `maxBitrate` and the player's
  `MaxSampleRate`).  Internet radio is streamed directly by the player (`RESP` → `cont`,
  ICY metadata via `META`) or proxied/transcoded when the player can't (HTTPS without
  `CanHTTPS`, HLS).  PLS / M3U / ASX / XSPF station playlists are expanded.
* **Control API**: the complete Lyrion dispatch table; `status`, `serverstatus`,
  `players`, `playlist …`, `playlistcontrol`, `mixer`, `sync`, `power`, `pref`, `rescan`,
  `favorites`, `alarm`, … with LMS result shapes; subscriptions (`subscribe:N`) re-executed
  on relevant notifications with LMS's filters and coalescing delays; CLI `listen` /
  `subscribe`; Cometd streaming and long-polling transports.
* **Library** (crate `rms-library`): SQLite database with Lyrion's schema, scanner using
  [lofty](https://crates.io/crates/lofty) (FLAC, MP3, AAC/ALAC, Ogg, Opus, WAV, AIFF, APE,
  WavPack, DSF), cue sheets, compilations, multi-artist/genre splitting, artwork
  extraction and resizing, saved playlists.
* **Artist pictures** (Lyrion's contributor portraits): `artist.jpg` / `<Artist Name>.jpg`
  next to or above an artist's albums, or in the *artist pictures folder* (`artfolder`),
  served at `/contributor/<id>/image` and shown in Squeezer, Jive clients and the web UI.
  Optionally (`onlineArtistImages`, off by default) missing pictures are downloaded from
  Deezer, which sends artist names to Deezer.
* **Jive menus** for Squeezer/SqueezePlay: home menu, My Music browsing, context menus,
  favorites, settings (including player display brightness and text size).
* **Player displays** (Squeezebox2/3/Classic, Transporter, Boom): Lyrion's bitmap fonts in
  all three text sizes, the Now Playing screen with each model's playing display modes
  (progress bar, elapsed/remaining time, buffer fullness, clock, VU meter and spectrum
  visualizers), server-side scrolling of long titles, the volume bar, the date/time
  screensaver while off, Transporter's second screen and brightness while on / off / idle
  (Boom's automatic brightness included).
* **On-screen menus** for those players, driven by the IR remote and front panel buttons:
  Lyrion's IR timing (press / repeat / hold / single) and its own `Default.map` button
  maps, Home → My Music / Radio / Favorites / Search / Settings, browsing with accelerated
  scrolling and number-key letter jumps, play / add / insert from any list, the Now Playing
  playlist browser, track info, text entry for searches and names (multi-tap number keys),
  sliders, checkboxes and radio settings, volume, presets, sleep, shuffle / repeat and the
  screensavers, with the push and bump animations of the real firmware.  Holding FWD/REW
  opens the song scanner (seek within the song); Settings → Alarm Clock edits alarms on the
  player (time entry, days, sound, shuffle, repeat, volume) like Lyrion's player screens.  The menus are the
  same Jive menus Squeezer uses, so everything browsable there is browsable on the player.
* **Plugins**: Favorites (OPML, compatible with Lyrion's `favorites.opml`), alarm clock,
  a small internet radio directory.

### WiiM notes

WiiM devices run a squeezelite-based client (`ModelName=WiiM Player`).  RMS uses the
SqueezePlay volume curve for them (as Lyrion does), always enables digital volume control
for players without a digital output, transcodes 32-bit PCM to 24-bit FLAC (WiiM garbles
32-bit PCM), never sends more than the advertised `MaxSampleRate`, and re-queries the
player name on every connect (the WiiM always reports its own device name).  The WiiM Home
app is only a player, not a library browser — use the web UI or Squeezer to browse.

### Differences from Lyrion

* One transcoder (ffmpeg) replaces Lyrion's `convert.conf` helper chain (flac, sox, lame,
  faad…); per-format enable/disable is not configurable yet.
* Only a few plugins exist (Favorites, alarms, radio directory).  Streaming-service
  plugins (Spotty, Qobuz, TIDAL…) and the TuneIn directory are not ported.
* The on-screen menus follow the Jive menu layout (as on a Squeezebox Touch/Radio) rather
  than every legacy `Slim::Buttons` screen: e.g. track info lists Play / Add first.
  Squeezebox 1 and SliMP3 (text displays) are not supported.
* No firmware updates for hardware players.

## Development

```sh
cargo test --workspace           # unit tests + an end-to-end test with a fake player
```

`crates/rms/tests/fake_player.rs` runs the server binary and drives it through
slimproto, HTTP streaming, the CLI and JSON-RPC.  For manual testing, squeezelite with
`-o -` (audio to stdout) works without a sound card.

Layout:

```
crates/slimproto   wire protocol codec + discovery packets
crates/library     SQLite schema, scanner, artwork, library queries
crates/rms         the server: players, controller, streaming, control API, web UI
crates/rms/web     the web UI (plain HTML/JS/CSS, embedded into the binary)
```

## License

GPL-2.0, like Lyrion Music Server, from whose source this is derived.
