# RMS's Cast receiver

Every Chromecast plays for RMS in this page (or in the app the settings name instead). It
plays what RMS loads: an audio stream from the server, or the server's visualiser video.
Audio plays on Web Audio where the TV can decode it (WebCodecs), with the same player as
the web UI's browser player, so it keeps time to the sample and is gapless. Otherwise
(and for the video) it plays through Cast's media handling.
It shows the playing track's cover, title, artist and album over one of three things,
as RMS says with each track:
- `draw`: MilkDrop presets drawn to the audio with projectM, built to WebAssembly in
  `tools/projectm-viz/web`;
- `video`: the server's visualiser video;
- `off`: the cover, blurred.

On a speaker (no screen) it only plays.

GitHub Pages serves it at `https://<owner>.github.io/<repo>/receiver/`. The *Receiver site*
workflow builds and publishes it on pushes to `main`, along with the projectM browser demo
at `/demo/`. One registered app (RMS's is `8745C385`, registered for
`https://rustydc.github.io/RMS/receiver/`) serves every RMS server: each server sends its stream URLs
and settings with each track. The main README covers registering it in the Cast developer
console.

## How it talks to RMS

- **Playing.** RMS sends the standard media `LOAD`, with the server's stream URL and
  music-track metadata (`metadataType` 3 with `title`, `artist`, `albumName` and
  `images`), one track at a time.
- **Settings.** The `LOAD`'s `customData.rms` carries the server's visualiser settings:

  ```json
  {"version": 3, "visualizer": "draw", "presetSeconds": 30, "fps": 30,
   "renderSize": "1280x720"}
  ```

  `version` is the protocol, raised when a change needs a newer receiver. A receiver that
  gets a newer version reports it as an error.
- **Its own namespace,** `urn:x-cast:net.rms.visualizer`:
  - RMS first sends `{"type": "HELLO"}`; the page answers `{"type": "HELLO", "version": 3,
    "engine": true, "decodes": ["flc", "ops"]}`. `engine` means it plays audio on Web Audio.
  - Then RMS sends the server's renderer messages as they are, in
    `{"type": "RENDER", "cmd": {...}}`: `load` (with `url`, `format`, `autostart`), `preload`,
    `pause`, `resume` (with `at`, a moment of this page's clock), `hold`, `skip`, `stop`. A
    load also carries `visual`, `rms` (the settings) and `track` (the track info), and a
    preload carries `track`. The player's reports come back in `{"type": "REPORT",
    "msg": {...}}`: `ready`, `started`, `progress` (with `jiffies` of this page's clock),
    `fetched`, `paused`, `resumed`, `ended`, `error`.
  - RMS sends `{"type": "PING", "id": 7}` a second. The page answers at once with
    `{"type": "PONG", "id": 7, "t": 123456.7, "position": 12.345, "rate": 1, "playing": true}`.
    `t` is its clock (`performance.now()`): RMS takes it as true halfway between asking
    and hearing back, and the quickest answers line the clocks up. `position` (for Cast's
    media handling) is the one being heard: the media's, less what the audio still takes
    to leave the browser.
  - In the visualiser video, RMS sends `{"type": "TRACK", "title", "artist", "album",
    "art", "duration", "start"}` as each track begins (`start`: where in the stream), for
    the track info.
  - The page reports `{"type": "STATS", "fps": 28, "size": "1280x720", "preset": "…"}`
    every 15 seconds while it draws, and `{"type": "ERROR", "message": "…"}` when
    something fails: no WebGL 2, Web Audio refused, or the stream unreadable for lack of
    CORS. RMS logs both.

## How it works

The drawing (`Visualiser`) and the track info (`TrackOverlay`) live in
`crates/rms/web/viz.js`, shared with the web UI's visualiser. `build.sh` copies it in
next to `receiver.js`.

- **Web Audio:** `web-audio-player.js` with its modules (`decoders.js`, `demux.js`,
  `pcm-output.js`, `pcm-worklet.js`), copied in from the web UI. It fetches and decodes the
  stream itself and plays it in an AudioWorklet. The visuals tap its output.
- **Otherwise,** CAF (the Cast receiver framework) plays into the page's own
  `<video crossorigin>` element, audio and video alike (it's shown only for the server's
  video).
- Once Web Audio is running, the element is routed through two analyser nodes. Each frame,
  projectM gets the samples played since the last frame. Until Web Audio is running, the
  element plays as usual.
- **Sync.** It takes `SET_PLAYBACK_RATE`: RMS keeps the TV in step with its sync group by
  playing a little faster or slower for a while.
- **CORS.** Reading the audio needs CORS on the server's streams. RMS allows any origin,
  and it answers Chrome's private-network preflights, since the page is public and the
  server is on the LAN.
- **Drawing.** projectM draws at `renderSize`, never larger than the screen, scaled up to
  fill it. With `renderSize` empty it draws as large as the TV keeps up with: from 720p, a
  size smaller when two 5-second spells fall below 85% of `fps`, a size larger after a
  minute at it (not a size it left in the last 10 minutes), between 360p and its screen
  (at most 1080p). When even 360p is below 75% of `fps`, it tells RMS (`{"type": "SLOW",
  "fps", "size"}`), and RMS's automatic setting shows the server's video or the cover from
  the next track. Its `HELLO` also says whether it has a screen and WebGL 2, and the
  screen's size. It draws at `fps`, whatever the screen's refresh rate. Presets change every
  `presetSeconds`, blending for 3 seconds, as on the server.
- **Track info.** It's large for 12 seconds when a track starts, and again while paused.
  The rest of the time it's a small card in the corner. Everything stays inside a 5%
  margin, since TVs crop their edges. The progress bar takes its colour from the cover.

## Building and trying it

```sh
source ~/emsdk/emsdk_env.sh      # the Emscripten SDK
receiver/build.sh                 # receiver/build/site: /receiver/ and /demo/
python3 -m http.server -d receiver/build/site 8000
```

Then open, for example,
`http://localhost:8000/receiver/?dev&debug&src=/some.flac&title=Song&artist=Band&album=Album&art=/cover.jpg`.
`dev` plays `src` without the Cast framework, and `debug` shows the frame rate and preset.
On a Chromecast, `chrome://inspect` can attach to the page once the device is registered
for development in the Cast console.
