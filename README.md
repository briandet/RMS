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
| `--music <dir>[,<dir>…]` | `RMS_MUSIC_DIR` | saved pref | music folders to scan: set on the first start and whenever this changes, so folders changed in the web UI stay through restarts |
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
* The TV visualiser: the script also builds `pmviz` and installs it with the MilkDrop
  presets when its build dependencies are there (`sudo apt install cmake ninja-build
  libegl-dev libgles-dev libegl-mesa0 libgl1-mesa-dri mesa-va-drivers`), and lets the
  service use the GPU (the `render` and `video` groups, `DeviceAllow=char-drm`).

## What is implemented

* **slimproto** (crate `slimproto`): byte-exact codec for every client and server frame
  (`HELO`, `STAT`, `RESP`, `META`, `DSCO`, `SETD`, `IR`, … / `strm`, `audg`, `aude`,
  `setd`, `vers`, `serv`, `cont`, display frames), UDP discovery (`d`/`D` and TLV `e`/`E`).
* **Playback controller**: a port of `Slim::Player::StreamingController` including its
  full event × state transition table: gapless track hand-off on `STMd`, end-of-stream
  handling, rebuffering on output underrun, retry of failed radio streams, crossfade /
  fade-in transitions, replay gain (track/album/smart), sleep timer, alarms.  Unlike Lyrion,
  transitions, replay gain, Don't Stop The Music, Adventurous and who to scrobble to are the
  same for every player (Settings → Playback; a client changing them for one player changes
  them for all).
* **Synchronisation**: sync groups share one controller and playlist; synchronized start
  with `strm u` at a common player-clock time and continuous drift correction with
  `skipAhead` / `pauseFor` from accurate play points.  A transcoded, proxied or Spotify
  stream is produced once for the whole group and paced by its slowest player.
* **Streaming**: local files are sent natively when the player can decode them, otherwise
  transcoded with ffmpeg (LMS rules: prefer native, honour `maxBitrate` and the player's
  `MaxSampleRate`).  Internet radio plays through the server (proxied, or transcoded where
  the player can't take it: HLS, HTTPS without `CanHTTPS`): one connection to the station
  for a sync group, and spectrum bars.  With `radioDirect` (*Players fetch radio from the
  station*, Settings) players connect to stations themselves as in Lyrion (`RESP` → `cont`,
  ICY metadata via `META`).  PLS / M3U / ASX / XSPF station playlists are expanded.
* **Control API**: the complete Lyrion dispatch table; `status`, `serverstatus`,
  `players`, `playlist …`, `playlistcontrol`, `mixer`, `sync`, `power`, `pref`, `rescan`,
  `favorites`, `alarm`, … with LMS result shapes; subscriptions (`subscribe:N`) re-executed
  on relevant notifications with LMS's filters and coalescing delays; CLI `listen` /
  `subscribe`; Cometd streaming and long-polling transports.
* **Native API** (`/api/v1`, [docs/api.md](docs/api.md)): players, the queue, the
  library, favorites, Spotify and settings as plain JSON, commands as typed requests, and a
  Server-Sent Events stream that pushes each change as it happens (no polling).  The web UI
  runs on it alone; TypeScript declarations of its types are generated from the server's
  (`crates/rms/web/api-types.d.ts`).  For clients that choose songs themselves (an assistant
  building a playlist), it also finds tracks by tempo, key, genre, year, rating and
  listening history, lists songs similar to any seeds without playing them, and queues or
  saves a list of tracks in one go.  Announcements interrupt a player for a clip (a doorbell,
  a spoken message) and then carry on where it was.  With file changes on, music files can be
  uploaded into the library.
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
  pairing) are not supported.  **Chromecasts play in RMS's own Cast receiver app**
  (below), which shows the playing track's cover and title, or the visualiser.
  **Chromecasts keep time** like the browser, so a group of Chromecasts, Squeezeboxes and
  browsers plays in step.  Where the TV can decode the audio itself (WebCodecs), the
  receiver app plays it on **the same Web Audio player as the browser's**
  (`web/web-audio-player.js`): the server's messages go to it as they are, and it starts
  at the exact moment, skips or waits to the sample, and plays the next track without a
  gap.  The driver lines the app's clock up with its own NTP-style: eight pings in a row when
  the app starts (a synced start waits for them), then one a second, each answer taken as
  true halfway through its round trip, and the quickest of the recent answers trusted.  Moments it sends are put on the app's clock, and the app's play points
  on ours.  Otherwise (and for the server's visualiser video) the app plays through Cast's
  own media handling: the driver asks where playback is a second, starts it at the moment
  the server names (sending `PLAY` as much earlier as the device was seen to take to
  start), and makes up a difference by playing 5% or 10% faster or slower for a few
  seconds (`SET_PLAYBACK_RATE`).  Chromecasts get FLAC whatever the file, or raw samples when a player synced with
  them can't take FLAC (a browser on a page that isn't https, AirPlay): as they are on
  Web Audio, in a WAV header for Cast's media handling.  Each player's *Audio delay* (Settings → Player,
  the `playDelay` pref) is how much later its sound comes out than it says, for TV
  speakers and soundbars.  Sync with UPnP and AirPlay players stays loose (they buffer by
  themselves).
  **A Chromecast doesn't take the TV from other apps:** when another app takes over
  (Netflix, YouTube) or it's stopped on the TV or from Google Home, the player turns off,
  so the queue doesn't go on and take the TV back (in a sync group the others play on).
  One that has another app on it when it connects isn't pulled into its sync group's
  playback.  Playing to it turns it on and takes the TV again.
  A device found several ways (a WiiM is a Squeezebox player, a UPnP renderer and a
  Chromecast) is one player, by its IP address: a Squeezebox player wins, then Chromecast,
  then UPnP, then AirPlay.  The others come back when it disconnects.
* **TV visualiser** (Chromecasts, not in Lyrion): with `pmviz` installed (see below), a
  Chromecast plays MilkDrop visuals drawn to the music by projectM, as video with the music
  in it, instead of audio alone.  A sync group's Chromecasts share one stream, drawn and
  encoded once on the server's GPU (1080p30 by default; an Intel Iris Xe keeps up with 60), and the
  tracks follow one another in it without gaps.  On, while `pmviz` is installed, for
  Chromecasts with a screen (as their discovery says; Cast speakers keep playing audio); a
  player's pref `visualizer` turns it off (0) or on (1, e.g. for a device given by
  address), in Settings → Player → TV visualiser.  Settings → TV visualiser sets the video
  size, frame rate, drawing size, compression and preset time.  Server prefs:
  `visualizerSize` (`1920x1080`), `visualizerFps` (30; some TVs can't decode 1080p60), `visualizerRenderSize` (draw smaller
  and scale up, e.g. `1280x720`), `visualizerPresetSeconds` (30), `visualizerEncoder`
  (`h264_vaapi` where there is a GPU, else `libx264`), `visualizerQuality` (the encoder's
  QP / CRF, 30 by default: each +6 halves the stream, for Wi-Fi), and `visualizerCommand`,
  `visualizerPresets`, `visualizerTextures` for where things are (by default
  `/usr/local/lib/rms/pmviz` and `/usr/local/share/rms/projectm`).  The visuals start a
  second or two after the music would; the receiver app is told each track as the stream
  reaches it, for its track info.
* **RMS's Cast receiver** (Chromecasts, not in Lyrion): every Chromecast plays in RMS's own
  receiver app ([`receiver/`](receiver/README.md)), a page on GitHub Pages.  It shows the
  playing track's cover, title, artist and album, large when a track starts or pauses and
  then small in a corner, over one of: the cover, blurred (*Off* under Settings → Player →
  TV visualiser), the server's visualiser video (above), or **MilkDrop drawn on the TV**
  (*Drawn on the TV*) by projectM in the browser, which needs only the audio and not the
  server's GPU.  A Chromecast's GPU is a phone's: with Settings → TV visualiser → Draw at
  on *Automatic* it draws as large as it keeps up with at the frame rate setting (from
  720p, up to its screen and 1080p, smaller when the frame rate falls short and larger
  again when there's room), and *Automatic* under Settings → Player draws on the TV
  unless even the smallest is too much (or it has no WebGL 2); then it shows the video
  from the server where pmviz is installed, else the cover, from the next track.  The
  preset time applies too.  On a speaker (no screen) it only plays.  The app is RMS's own (app ID `8745C385`,
  serving `https://rustydc.github.io/RMS/receiver/`) unless Settings →
  TV visualiser → Receiver app ID (`visualizerReceiverApp`) names another; for your own
  copy:
  1. Publish the receiver: GitHub Pages builds and serves it from the repository
     (Settings → Pages → Source: GitHub Actions; the *Receiver site* workflow runs on
     pushes to `main`), at `https://<owner>.github.io/<repo>/receiver/`.  One receiver
     serves any number of RMS servers.
  2. Register it in the [Google Cast developer console](https://cast.google.com/publish)
     (a one-time US$5): Add new application → Custom Receiver, with that URL, and
     Google Cast for Audio on (for speakers).  Guest Mode isn't needed.
  3. While it's unpublished, add each Chromecast's serial number there (Devices), and
     restart the Chromecast.  Publishing it removes that step.
  4. Enter its application ID in Settings → TV visualiser → Receiver app ID.

  A device that won't start the app (one not added in step 3) stops with an error that
  says why.  The log gets the TV's frame rate every 15 seconds while it draws (`the TV
  draws at … fps`), to see whether its GPU keeps up.
* **Visualiser in the browser** (not in Lyrion): with "Play in this browser" on and in sync
  (Settings → This browser), Now Playing has a Visualiser button (✦) while this browser is
  the selected player or in its sync group.  It draws the same MilkDrop presets with
  projectM in the browser, from the audio the browser plays, over the whole window, with the
  track in the corner as on the TV.  Keys: N next preset, F full screen, Esc close; Half
  detail draws at half the resolution for slower GPUs.  projectM and the presets come from
  the receiver's GitHub Pages site (see above), so the browser needs the internet for it; a
  URL ending in `/` in the browser's local storage as `rms.vizAssets` points it at another
  copy (e.g. a local `receiver/build.sh` build served with CORS).  The drawing and the track
  info are one module, `web/viz.js`, shared with the TV receiver.
* **The Table** (Library → Table, not in Lyrion): the library as a dense, sortable table,
  one row per track, or per album, artist, genre, year, format, folder, … with how many
  tracks each has and what its tracks share (or that they differ: *‹3 values›*).  A
  `field:value` query narrows it down (`genre:jazz year:1955..1965 -format:mp3`,
  `added:<30d`, `missing:year`, `varies:album_artist`, `happy:>70`, `differs:title`); besides
  the tags, columns show what MusicBrainz says (the matched release's titles, label, catalogue
  number, country, medium, original year, …) and EffNet's style and mood scores; they are chosen, ordered, sorted
  and resized; double-click opens a row (artist → albums → tracks) or plays a track; views
  can be saved.  Rows load as they scroll into view, so a big library scrolls freely.  The
  same queries are in the API ([docs/api.md](docs/api.md#the-table)) and can be queued.
* **Spectrum bars** (not in Lyrion): a playing player shows four little bars, bass to
  treble, moving with the music, in the web UI's player menu and now playing.  They are
  played back from the player's position, so no audio goes to the browser: a library
  track's levels are worked out by the background analysis (about 10 KB a track, or when it
  first plays), and a stream the server relays (radio, Spotify) is decoded and analysed as
  it goes, with ffmpeg.  Radio the players fetch themselves (`radioDirect`), tracks over 20
  minutes and anything else without levels get bars that just bounce.
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
  (Boom's automatic brightness included).  A display test lights every pixel at full
  brightness until a button is pressed, for checking a VFD for dead or dim spots: Settings →
  Advanced → Player Display → Display Test on the player, or `<player> button displaytest`
  over the CLI.
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
(for every player) under Settings → Playback in the web UI, or in Squeezer, the Touch/Radio
or a player's own menus:

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

**What kind of music** (Settings → Library, *Analyse what kind of music each song is*, off by
default): the sonic analysis also runs [Essentia](https://essentia.upf.edu/models.html)'s
Discogs-EffNet model on each song (32 short excerpts across it, about a second and a half of
CPU per song, in pure Rust).  It gives each song a 1280-number "style fingerprint" and
names: Discogs styles (*Deep House*, *Hard Bop*, …), moods (happy, sad, relaxed, aggressive,
party, danceable, acoustic or electronic, voice or instrumental), themes and instruments.  The
mixes then weigh how close a song's style is to the seeds' alongside how it sounds (bliss),
and the track's details show its styles and moods.  The models are the Music Technology
Group's, under [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/) (no
commercial use), so they are not part of RMS: turning it on downloads them (about 30 MB)
from essentia.upf.edu into `<data-dir>/models/effnet`.

**MusicBrainz** (Settings → Library, *Identify recordings with MusicBrainz*, off by default):
in the background, one request a second, RMS finds each recording on MusicBrainz — by the
MusicBrainz id or ISRC in its tags, by its sound on AcoustID (with a free
[AcoustID key](https://acoustid.org/new-application)), or by artist, title and length, taking
the studio recording over live ones — and fills in what the tags leave out: genres for tracks
without one (the recording's, or its artist's most voted), MusicBrainz ids and ISRCs.  Learned
Spotify tracks get their genres the same way, so mixes weigh them like your files.  Your files
are never written, and tags always win.  A large library takes some hours the first time;
`--musicbrainz-url` points RMS at a local MusicBrainz mirror instead, without the rate limit.

Then each album is matched to the release it is (the edition, not just the songs): the one
its tags name, else the best fit among the releases holding its recordings, by how its tracks
pair up with the release's (recording, title, length, position), how much of the release it
holds, and its title and artist.  The album's title must agree for a confident match, so a
lone file tagged with its album matches the album rather than a single of the song.  A
confident match fills in the album's year and MusicBrainz id and the tracks' numbers and discs
where tags have none; uncertain ones can be checked and another release chosen through the API
(`/api/v1/albums/{id}/release`, [docs/api.md](docs/api.md#album-release)).

**Changing tags** (Settings → Library, *Change tags in music files*, off by default): an
album's *Tags* page shows its files' tags next to what its MusicBrainz release says — title,
artists, album artist, track and disc numbers, dates, label, catalog number, barcode and the
MusicBrainz ids, by the names Picard uses — and writes what you tick (or type) into the
files, with undo.  Another release can be picked from the ones considered, or given by its
link.  It needs a login (`username`, `password`, `authorize`, see the firewall notes above),
since anyone who can reach the web UI could otherwise rewrite your music, and write access
for RMS to the music (`packaging/install.sh --edit-files` sets up the service for it).  Only
the tags asked for change: the audio, pictures and other tags stay as they are, the file is
rewritten in place (keeping its owner and permissions), each write is read back and the file
put back if anything is off.  FLAC, Ogg, Opus, MP3, MP4/M4A, AIFF, WAV, APE and WavPack;
not cue-sheet tracks.  Play counts, analysis and the release match stay with the files.

The same page **moves and renames** the album's files by a template (Settings, or edited on
the page and saved as the default), relative to the music folder they are in:
`{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}` gives
`The Band/First Album (1991)/01 Opening.flac`.  A part in `[…]` is left out when its tags are
empty; `{disc}` only appears for albums of several discs.  The page shows every file's
current and new path first.  Names are made safe for any file system (no `/ \ : * ? " < > |`,
no leading dots).  Covers, booklets and logs in the album's folder go along when all of its
audio moves to one place, and folders left empty are removed.  Ratings and play history,
saved playlists (and their `.m3u` files), favorites and the players' queues follow the files;
undo moves everything back.  Files never leave their music folder, and a file already at the
new path is never overwritten.

**Cover art** on the same page comes from the [Cover Art Archive](https://coverartarchive.org)
for the album's MusicBrainz release (or its release group): pick an image (front cover first)
and save it as `cover.jpg` in the album's folder, embed it as the front cover of each file, or
both.  A picture embedded in a file is shown before one in the folder, so embedding is
suggested when the files already have one.  A `cover`/`folder`/`album` picture already in the
folder is set aside, and the files' old embedded covers are kept, so undo puts back exactly
what was there (`<data-dir>/cover-backup`).  The album shows the new cover at once.

Tracks **without any art** of their own get it anyway, with MusicBrainz identification on.
A track on an album confidently matched to its release shows that release's front cover from
the Cover Art Archive; a track on its own (no album tag, or an album not surely identified)
shows the cover of its recording's release: the album its tags name, else its original album
(never a compilation's or a live album's).  Covers are fetched at 1200 pixels (about 100–300
KB each, kept in `<data-dir>/cache/release-art`) and every client shows them: the web UI,
Squeezer, the players' screens, Subsonic apps.  Your files are not changed, and your own art
always wins: a picture embedded in the files, or in an album's own folder.  A folder is a hint,
not what identifies a track: a picture in a folder of several albums' files or loose tracks (a
downloads folder) isn't each track's own, so identified tracks there show their own releases'
covers, and the Tags page offers to embed a cover rather than put it in such a folder.  The
tracks without an album tag aren't an album there either: each is changed on its own, and they
aren't moved together.

**How good are the mixes?** `rms-mix-eval` (built with `rms`) judges the similar-songs
mixes against an outside opinion.  It picks seed songs from your library, makes mixes several
ways — today's scoring, without sound (bliss), without style (EffNet), sound only, style only,
tags only, and at random — and counts a
pick as a *hit* when ListenBrainz's listeners play its artist in the same sessions as the
seed's.  Each way's hit rate is set against chance (how often a random song from your library
would be a hit), so `lift` 1× is no better than random:

```sh
rms-mix-eval --data-dir /var/lib/rms --seeds 50          # a summary table
rms-mix-eval --data-dir /var/lib/rms --details --json report.json   # and every mix
rms-mix-eval --data-dir /var/lib/rms --try more-style:style=8,sound_with_style=1  # other weights
```

`--try NAME:CHANGES` (repeatable) adds a way of mixing with some of the weights changed
(`genres`, `same_artist`, `related_artist`, `sound`, `sound_with_style`, `style`, `era`,
`taste`); each way is also set against the first, by seeds won and lost.

It reads the data folder (RMS can keep running) and caches ListenBrainz's answers
(`--offline` reuses them).  Seeds need artists with MusicBrainz ids (tags or identification)
and a few songs here by artists ListenBrainz finds similar; the same `--rng-seed` gives the same
seeds.  Hits measure "listened to together", not taste; use it to compare ways of mixing, not
as a grade.

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
affects one of the measures slightly.  Library → Spotify → *learned for mixes* (or the count
in Settings) lists the learned tracks in a table to sort and filter: tempo, key, loudness,
style, genres and whether the library has them too.

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

Spotify limits the shared client ID RMS signs in with, so browsing may stall; your own app
(a client ID from developer.spotify.com, in the settings card) has its own limit.  It is for
browsing only: playback and Spotify Connect only work with Spotify's own app (librespot gets
their tokens from Spotify's login5, which refuses other apps).  With your own client ID set
there are two sign-ins in the settings card, **Sign in for playback** (Spotify's app) and
**Sign in with your app** (browsing); without one, a single sign-in does both.

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
playing something else on the player pauses the Connect device, and next and previous on
the player (a remote, the web UI, Squeezer) move through the Spotify queue.  Cast and
UPnP players (a WiiM, a Chromecast) show each track's title, artist and cover: for them the
stream starts afresh for every track, so tracks change with a short gap instead of
gaplessly.  The player's queue holds
one "Spotify Connect" stream; the web UI's queue shows what the Spotify app plays next below
it (changed in the app).  The devices are
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

The end-to-end tests are one test binary per crate (`tests/it/`, a module per file):
`cargo test -p rms --test it conformance` runs one file's tests.
`crates/rms/tests/it/fake_player.rs` runs the server binary and drives it through
slimproto, HTTP streaming, the CLI and JSON-RPC.  `crates/rms/tests/it/conformance.rs` makes
the requests real clients make (Home Assistant, SqueezePlay, Squeezer, a Squeezebox 3's
screens, the web UI) against a generated library and compares the answers with
`crates/rms/tests/golden/`; after an intended change, `UPDATE_GOLDEN=1 cargo test -p rms
--test it conformance` rewrites them (review the diff).  For manual testing, squeezelite with
`-o -` (audio to stdout) works without a sound card.

Build times: release builds are incremental and skip cross-crate LTO, so rebuilding after a
`git pull` takes seconds to a minute rather than minutes (`cargo build --profile dist` keeps
the old thin-LTO settings).  The dev profile keeps line tables only (backtraces have file and
line; set `debug = true` locally for a debugger), dependencies have no debug info, and
`cargo build` and `cargo test` share every artifact: keep dev-dependencies from adding
features to normal dependencies, or each switch rebuilds tokio and everything above it.  In
`crates/rms`, spawn tasks with `crate::util::spawn` and `crate::util::spawn_blocking`, not
tokio's (clippy enforces it): each future handed to tokio's directly gets its own copy of
the task machinery.  A faster linker helps a little more:
with [mold](https://github.com/rui314/mold) installed, add to `~/.cargo/config.toml`

```toml
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

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
