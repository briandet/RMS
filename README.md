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
| Browser | built-in web UI at `http://<server>:9000/`, installable as an app | ✔ new responsive UI; the browser can be a player too (*Play in this browser*), in sync with Squeezeboxes |
| New clients | native API (`/api/v1`): JSON resources, typed commands, pushed events | ✔ players, library, favorites, settings ([docs/api.md](docs/api.md)) |
| UPnP/DLNA renderers, Chromecast, AirPlay speakers | bridged as players (Settings → Other players) | ✔ tested against simulated devices |
| **Subsonic apps** (DSub, Symfonium, Ultrasonic, play:Sub, Amperfy, Feishin, …) | Subsonic / OpenSubsonic API (`/rest/*`) | ✔ browsing, streaming to the app, playlists, stars, jukebox ([docs/subsonic.md](docs/subsonic.md)) |

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
| `--https-port <n>` | `RMS_HTTPS_PORT` | 9443 | the same over HTTPS ([below](#https)); 0 turns it off |
| `--tls-cert <pem>`, `--tls-key <pem>` | `RMS_TLS_CERT`, `RMS_TLS_KEY` | | your own certificate for HTTPS instead of the server's |
| `--cli-port <n>` | `RMS_CLI_PORT` | 9090 | telnet CLI |
| `--name <name>` | `RMS_NAME` | host name | server name shown on players |
| `--rescan` / `--wipe` / `--scan-only` | | | library scanning at startup |
| `--log <filter>` | `RMS_LOG` | `info` | e.g. `info,rms=debug` |

Preferences are stored as YAML in `<data-dir>/prefs/server.prefs` and one
`<mac>.prefs` file per player, using the same pref names as Lyrion (`maxBitrate`,
`transitionType`, `replayGainMode`, `syncVolume`, `powerOnResume`, …); they can be changed
with the `pref` / `playerpref` commands or the web UI.

### HTTPS

The web UI and the APIs are also served over HTTPS, on port 9443.  Browsers keep some
features to secure pages: installing the web UI as an app, and the browser player's
compressed audio (WebCodecs) and AudioWorklet.  Squeezeboxes and the LMS clients keep using
HTTP.

Without a certificate of your own, RMS creates a small certificate authority of its own on
first start (`<data-dir>/tls`) and issues the server's certificate from it.  The server's
certificate names the host, `<host>.local` and the host's addresses, and is issued again
when they change and before it expires.  Each device trusts it once the authority's
certificate is installed on it: Settings → *Secure connection* has the download
(`/tls/rms-ca.crt`) and the steps for iOS, Android, macOS, Windows and Firefox.  The
authority can only vouch for local names and addresses: `localhost`, the host's name,
`.local`, `.lan`, `.home`, `.home.arpa`, `.internal`, and private, loopback, link-local and
Tailscale (100.64.0.0/10) addresses.  Its certificate says so (name constraints), and
browsers enforce that, so a stolen key could not be used to pose as other sites.  To use a
name outside these, start RMS with `--http-addr <name>` before the authority is first
created, or give your own certificate.

With a certificate from elsewhere (Let's Encrypt, `tailscale cert`, your own CA), pass
`--tls-cert` and `--tls-key`.  RMS reloads the files when they change, so renewals need no
restart.

### Running as a service (Debian / Ubuntu)

```sh
sudo apt install build-essential pkg-config git ffmpeg
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # distro Rust is usually too old
packaging/install.sh /srv/music          # music folders, comma-separated
```

The script builds a release binary (the web UI is built in), installs it as
`/usr/local/bin/rms`, creates an `rms` system user and a hardened systemd service
(`packaging/rms.service`) with its data in `/var/lib/rms`, and starts it.  Settings are in
`/etc/default/rms`; running the script again upgrades and keeps them.  Logs:
`journalctl -u rms -f`.

* The `rms` user only needs read access to the music, e.g.
  `sudo setfacl -R -m u:rms:rX -m d:u:rms:rX /srv/music` (the script warns if it lacks it).
* Stop Lyrion first if it is installed (`sudo systemctl disable --now lyrionmusicserver`):
  both use the same ports.
* Firewall: allow TCP+UDP 3483 (players), TCP 9000 (web UI, Squeezer) and TCP 9443 (the web
  UI over HTTPS), plus TCP 9090 if you use the telnet CLI.  Keep them on the LAN: like Lyrion, there is no login by default.
  To require one, set `username` and `password` and then `authorize` to 1 (e.g. `pref
  username me`, `pref password <sha1_base64 or plain>`, `pref authorize 1` on the CLI):
  the web UI, JSON-RPC and Cometd then need HTTP Basic auth and the CLI needs `login`.
  Players are unaffected (they get a per-stream password).
* Player discovery is a UDP broadcast, so players find the server on the same network
  segment; elsewhere give them its address.
* Spotify sign-in on a headless server: paste the address Spotify sends you to into the
  settings card, or sign in while running `ssh -L 5588:127.0.0.1:5588 <server>`.

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
  `skipAhead` / `pauseFor` from accurate play points.  A transcoded, proxied or Spotify
  stream is produced once for the whole group and paced by its slowest player.
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
* **Native API** (`/api/v1`, [docs/api.md](docs/api.md)): players, the queue, the
  library, favorites, Spotify and settings as plain JSON, commands as typed requests, and a
  Server-Sent Events stream that pushes each change as it happens (no polling).  The web UI
  runs on it alone; TypeScript declarations of its types are generated from the server's
  (`crates/rms/web/api-types.d.ts`).
* **Subsonic API** (`/rest/*`, [docs/subsonic.md](docs/subsonic.md)): Subsonic 1.16.1 with
  the OpenSubsonic additions, for the apps that browse a library and stream it to the
  phone or computer they run on, transcoding with ffmpeg when asked; stars are RMS
  favorites, and the jukebox is a Squeezebox.
* **Library** (crate `rms-library`): SQLite database in RMS's own schema
  ([docs/library-schema.md](docs/library-schema.md)), scanner using
  [lofty](https://crates.io/crates/lofty) (FLAC, MP3, AAC/ALAC, Ogg, Opus, WAV, AIFF, APE,
  WavPack, DSF), cue sheets, compilations, multi-artist/genre splitting, artwork
  extraction and resizing, saved playlists.  Play counts, ratings and sonic analysis
  survive rescans and wipes, and follow a file that is moved or renamed.  A database from
  an earlier release is converted on first start: it is backed up to
  `library.sqlite.lms-backup` first, history and analysis are kept, and the library is
  rescanned (going back: [docs/library-schema.md](docs/library-schema.md#converting-a-database-from-earlier-releases)).
* **Bridged players** (`crates/rms/src/bridge`): devices that aren't Squeezeboxes join as
  players through an in-process slimproto client per device, so the queue, sync groups,
  alarms and every remote control work on them.  The first is the web browser: *Play in
  this browser* (Settings) plays through Web Audio (an AudioWorklet), gaplessly and in
  sync with the Squeezeboxes it is synced with: it reports exact play
  points on its own clock, starts at the moment the server names and skips or waits when
  told, as squeezelite does.  On the HTTPS address the audio comes as FLAC, or as Opus
  (160 kbit/s) when set to save data, decoded by the browser (WebCodecs); on plain HTTP as
  raw samples.  Turned off, or where the browser lacks Web Audio, it plays
  through an `<audio>` element (only roughly in sync).  Lock-screen and media-key controls
  come from Media Session.  Under *Other players*
  the server can also find **UPnP/DLNA renderers** (SSDP), **Chromecasts** (mDNS) and
  **AirPlay 1 speakers** (mDNS, ALAC over RAOP) on its network and add them as players (off
  by default).  The devices fetch the audio from the server themselves, so they must reach
  it on the network; where multicast discovery can't see them (other subnets, Docker without
  host networking), list them in the prefs `upnpRenderers` (description URLs),
  `chromecastHosts` and `airplayHosts` (`host[:port]`).  AirPlay 2-only devices (needing
  pairing) are not supported; sync with these players is loose (they buffer by themselves).
* **Watching the music folders** (`autorescan`, on by default, unlike Lyrion): music
  copied in, retagged, renamed or removed shows up by itself a few seconds later; only the
  folders that changed are scanned.  It uses the operating system's file notifications
  (inotify on Linux), so changes made by another computer on a network share are not seen:
  rescan then.  A very big library may need a higher `fs.inotify.max_user_watches` (one per
  folder; the log says so).
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
  internet radio (a few well-known stations, and the [Radio Browser](https://www.radio-browser.info)
  directory: search, popular, top voted, by country, genre and language — in the web UI's
  Library → Radio tab and under Radio on players and controllers), Don't Stop The Music and
  Create Mix, Spotify (below).

### Don't Stop The Music and Create Mix

**Don't Stop The Music** keeps a player going when its queue runs out, as in Lyrion: when
fewer than two songs follow the one playing (and repeat is off), it adds more.  Choose what
per player under Settings in the web UI, Squeezer, the Touch/Radio or the player's own
menus:

* **Similar Songs**: songs like the last ones in the queue, picked from your library by
  how they sound (see below), shared genres and artists, era, and play history (artists you played around the same
  time, play counts and ratings; songs played in the last day are avoided), at most one per
  album and two per artist in each batch.
* Lyrion's Random Mix choices: **Song Mix** (optionally limited to the queue's genres),
  **Random Album** (optionally limited to the queue's genres), **Random Artist**,
  **Random Year**.
* Any of your favorites, e.g. a radio station.

It does nothing after a radio station, and keeps the queue under `maxPlaylistLength` by
dropping songs from its start.  **Create Mix** plays about 20 similar songs for a track
(starting with it), album, artist, genre or year: the *Mix* button in the web UI, or
*Create Mix* in the track / album / artist / genre / year menus on Squeezer and the players.

**Sonic analysis** (`sonicAnalysis`, on by default): after each scan, RMS analyses your local
tracks in the background with [bliss](https://lelele.io/bliss.html) — tempo, timbre,
loudness and harmony as a small feature vector per song — on half the CPU cores.  Most
formats are decoded in-process; ffmpeg handles the rest (Opus, WMA, APE, …) and cue-sheet
tracks.  Once a seed song is analysed, the mixes add songs from anywhere in the library that
sound like it, and how close they sound counts for more than tags do.  Tracks that sound like
the same recording as one in the queue or the mix (another encoding or edit of it) are left
out; live versions sound too different to be caught this way.  Expect a second or two of
CPU time per song; tracks over 20 minutes are skipped.  Results survive a wipe
and follow a moved file; they are redone when the audio changes.  Lyrion itself has no audio analysis: it relied on the
long-gone MusicIP server, and now on third-party plugins.

The same pass also finds each track's **tempo, beats and key** (BPM to a hundredth, a beat
grid, the bar's downbeat, major/minor key in Camelot notation, and an energy curve per bar),
the groundwork for DJ-style transitions, and a **Chromaprint audio fingerprint**, so the same
recording is recognised under other tags (or none).

**MusicBrainz** (Settings → Library, *Identify recordings with MusicBrainz*, off by default):
in the background, one request a second, RMS finds each recording on MusicBrainz — by the
MusicBrainz id or ISRC in its tags, by its sound on AcoustID (with a free
[AcoustID key](https://acoustid.org/new-application)), or by artist, title and length, taking
the studio recording over live ones — and fills in what the tags leave out: genres for tracks
without one (the recording's, or its artist's most voted), MusicBrainz ids and ISRCs.  Learned
Spotify tracks get their genres the same way, so mixes weigh them like your files.  Your files
are never written, and tags always win.  A large library takes some hours the first time;
`--musicbrainz-url` points RMS at a local MusicBrainz mirror instead, without the rate limit.

**Discovery with ListenBrainz** (Settings → Library, *Discover music with ListenBrainz*, off
by default): a share of each Similar Songs and Create Mix batch — each player's *Adventurous*
setting, 15% unless changed — is music you haven't played: recordings ListenBrainz listeners
play in the same sessions as the seeds, and songs by artists it finds similar.  They play from
your library when you have them (so music you own but never play turns up), else from Spotify
when signed in.  Played to the end, a Spotify track is learned like any other; skipped within
30 seconds, a discovery is rejected and not proposed again, nor is an artist rejected twice.
The seeds need MusicBrainz ids (identification above, or tags).  ListenBrainz's public
similarity data needs no account; answers are cached for a month.

**ListenBrainz accounts** (Settings → ListenBrainz, with the user token from
[your ListenBrainz settings](https://listenbrainz.org/settings/)):

* **Scrobbling**: what plays is sent as you listen — *now playing* when a song starts, a
  listen once it has played for half its length or 4 minutes — with the MusicBrainz ids and
  ISRC RMS knows, so ListenBrainz links it to the right recording.  Each player scrobbles to
  the accounts you pick for it (several people, one each), or to the default ones; Subsonic
  apps too.  Spotify plays only if you tick *Scrobble Spotify plays too* (ListenBrainz can
  import them from Spotify itself).  Listens wait while ListenBrainz or the network is down.
* **Recommendations**: Library → ListenBrainz has what ListenBrainz recommends for you and
  the playlists it makes for you (Daily Jams, Weekly Jams, Weekly Exploration), playing from
  your library or Spotify; with discovery on, recommendations also find their way into
  mixes.

**Spotify in mixes** (Settings → Spotify, *Use what I play in mixes*, on by default): when a
Spotify track plays to its end, RMS analyses the samples it was streaming anyway (sound,
rhythm and key, fingerprint) and keeps that with the track's metadata; no audio is kept, and
nothing is streamed just to be analysed.  Similar Songs and Create Mix then pick learned
Spotify tracks alongside your files while you're signed in.  A Spotify track the library has
too (the same ISRC or audio fingerprint, or the same artist and title at the same length)
plays from your file instead.  With *Normalise volume* on, the loudness Spotify applies
affects one of the measures slightly.

**Loudness**: the same pass measures each track's loudness (EBU R128, as ReplayGain 2.0
does, at any length) and fills in the ReplayGain that your tags leave out — track gain and
peak, and album gain once every track of the album is measured — so `replayGainMode` evens
out untagged music too.  Tags always win and files are never written; a mono file counts as
playing on both speakers.  The album gain is a length-weighted average of its tracks', within
a few tenths of a dB of a full album measurement.


### Spotify

Spotify Premium is required (Spotify streams only to Premium accounts).  Sign in under web UI
→ Settings → Spotify: Spotify's login page opens, and after you agree it sends your browser
to `http://127.0.0.1:5588/login`, which RMS answers when the browser runs on the server's
machine.  Otherwise that page won't load; paste its address from the address bar into the
form in the settings card.  The login is saved under `<data-dir>/spotify/`.

Liked Songs, your playlists, saved albums and followed artists, recently played, top tracks
and artists, new releases and search are then in the web UI's Library → Spotify tab, in the
Home menu on Squeezer, Touch/Radio and on the player's own screens.  Tracks, albums and
playlists can be played, added and saved as favorites.  RMS plays the audio itself with
[librespot](https://github.com/librespot-org/librespot): it decodes each track (Ogg Vorbis,
96/160/320 kbit/s, optional volume normalisation, both set in the settings card) and streams
it to the player as 16-bit 44.1 kHz FLAC (lossless, about half the size of PCM; PCM for
players without FLAC), so any player works (including WiiM), seeking included.  Synced
players share one decode.

Every player is also a **Spotify Connect** device of the signed-in account, named after the
player: pick it in a Spotify app to play there.  Play, pause, seek and track changes in the
app drive the player, and the player's volume follows the app's; stopping, pausing or
playing something else on the player pauses the Connect device.  The devices are
registered with the account rather than announced on the network, so they are visible only
to the account RMS is signed in with.

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
* Only a few plugins exist (Favorites, alarms, internet radio, Don't Stop The Music,
  Spotify).  Other streaming services (Qobuz, TIDAL…) and the Random Mix plugin are not
  ported, and Radio Browser stands in for the TuneIn directory; Spotify, including Spotify Connect, is built in rather than
  through Spotty.
* The on-screen menus follow the Jive menu layout (as on a Squeezebox Touch/Radio) rather
  than every legacy `Slim::Buttons` screen: e.g. track info lists Play / Add first.
  Squeezebox 1 and SliMP3 (text displays) are not supported.
* No firmware updates for hardware players.

## Development

```sh
cargo test --workspace           # unit tests, end-to-end tests and client conformance
```

`crates/rms/tests/fake_player.rs` runs the server binary and drives it through
slimproto, HTTP streaming, the CLI and JSON-RPC.  `crates/rms/tests/conformance.rs` makes
the requests real clients make (Home Assistant, SqueezePlay, Squeezer, a Squeezebox 3's
screens, the web UI) against a generated library and compares the answers with
`crates/rms/tests/golden/`; after an intended change, `UPDATE_GOLDEN=1 cargo test -p rms
--test conformance` rewrites them (review the diff).  For manual testing, squeezelite with
`-o -` (audio to stdout) works without a sound card.

Layout:

```
crates/slimproto   wire protocol codec + discovery packets
crates/library     SQLite schema, scanner, artwork, library queries
crates/rms         the server: players, controller, streaming, control API, web UI
crates/rms/src/player/command.rs  typed player commands: the one implementation behind
                   the LMS commands, the native API and the Subsonic jukebox
crates/rms/src/api the native API (/api/v1) and its event stream
crates/rms/src/subsonic  the Subsonic API (/rest)
crates/rms/web     the web UI (plain HTML/JS/CSS, embedded into the binary)
```

## License

GPL-2.0, like Lyrion Music Server, from whose source this is derived.
