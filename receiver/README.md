# RMS's Cast receiver

Every Chromecast plays for RMS in this page (or in the app the settings name instead). It
plays what RMS loads: an audio stream from the server, or the server's visualiser video.
It shows the playing track's cover, title, artist and album over one of three things,
as RMS says with each track:
- `draw`: MilkDrop presets drawn to the audio with projectM, built to WebAssembly in
  `tools/projectm-viz/web`;
- `video`: the server's visualiser video;
- `off`: the cover, blurred.

On a speaker (no screen) it only plays.

GitHub Pages serves it at `https://<owner>.github.io/<repo>/receiver/`. The *Receiver site*
workflow builds and publishes it on pushes to `main`, along with the projectM browser demo
at `/demo/`. One registered app serves every RMS server: each server sends its stream URLs
and settings with each track. The main README covers registering it in the Cast developer
console.

## How it talks to RMS

- **Playing.** RMS sends the standard media `LOAD`, with the server's stream URL and
  music-track metadata (`metadataType` 3 with `title`, `artist`, `albumName` and
  `images`), one track at a time.
- **Settings.** The `LOAD`'s `customData.rms` carries the server's visualiser settings:

  ```json
  {"version": 2, "visualizer": "draw", "presetSeconds": 30, "fps": 30,
   "renderSize": "1280x720"}
  ```

  `version` is the protocol, raised when a change needs a newer receiver. A receiver that
  gets a newer version reports it as an error.
- **Its own namespace,** `urn:x-cast:net.rms.visualizer`:
  - RMS sends `{"type": "PING", "id": 7}` a second; the page answers at once with
    `{"type": "PONG", "id": 7, "position": 12.345, "rate": 1, "playing": true}`. The
    position is the one being heard: the media's, less what the audio still takes to
    leave the browser (Web Audio's output latency, once the audio goes through it). RMS
    takes it as true halfway between asking and hearing back, to keep the TV in sync.
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

- CAF (the Cast receiver framework) plays into the page's own `<audio crossorigin>`
  element.
- Once Web Audio is running, the element is routed through two analyser nodes. Each frame,
  projectM gets the samples played since the last frame. Until Web Audio is running, the
  element plays as usual.
- **Sync.** It takes `SET_PLAYBACK_RATE`: RMS keeps the TV in step with its sync group by
  playing a little faster or slower for a while.
- **CORS.** Reading the audio needs CORS on the server's streams. RMS allows any origin,
  and it answers Chrome's private-network preflights, since the page is public and the
  server is on the LAN.
- **Drawing.** projectM draws at `renderSize`, never larger than the screen, scaled up to
  fill it. It draws at `fps`, whatever the screen's refresh rate. Presets change every
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
