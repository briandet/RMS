# projectM visualiser prototype

Can the server draw MilkDrop presets for a Chromecast TV in real time? `pmviz` answers that
before RMS builds anything on it. It renders [projectM](https://github.com/projectM-visualizer/projectm)
4.1, the open-source MilkDrop engine, off screen through EGL, so it needs no display or X
server. It feeds projectM the music and either:

- measures how fast it draws (`--bench`), or
- encodes a video with the music through ffmpeg (`-o`), paced the way a live stream to the TV
  would be.

It's a standalone C program outside the Cargo workspace, so it doesn't add to RMS's build.

## Run it on the server

```sh
sudo apt install build-essential cmake ninja-build git pkg-config ffmpeg \
                 libegl-dev libgles-dev libegl-mesa0 libgl1-mesa-dri mesa-va-drivers vainfo
tools/projectm-viz/build.sh                 # about a minute: projectM, pmviz, presets, textures
tools/projectm-viz/bench.sh ~/Music/some-song.flac
```

Your user needs read access to `/dev/dri/renderD128`. Usually that means joining the `render`
group with `sudo usermod -aG render $USER`, then logging in again. Without that access, EGL
falls back to software rendering, and the output says so (`GL_RENDERER: llvmpipe`).

`bench.sh` takes a few minutes. It writes `build/bench-<host>.txt` to paste back, and
`build/sample.mp4`, a minute of the visualiser with the music, to watch on the TV.

## Reading the results

Every run prints a summary like this:

```
1200 frames drawn, 1280x720, mesh 48x32, Mesa Intel(R) Graphics
  render      4.10 ms a frame
  readback    0.90 ms a frame
  slowest    12.30 ms; 0 of 1190 frames over the 33.3 ms budget (0.0%)
  preset switches: 10, 210 ms on average, slowest 480 ms (compiling the shaders)
  overall   190.0 fps drawn over 6.3 s = 6.1x real time at 30 fps
```

(The numbers above are illustrative, not a measurement.)

- **Real time.** The bench runs give drawing alone. The `--unpaced -o` runs give drawing plus
  encoding. Comfortable is 2x or more for drawing plus encoding at 720p, because RMS is
  decoding, analysing and streaming at the same time.
- **Frames over budget.** These are frames that took longer than 1/30 s, not counting preset
  switches. A few percent is fine; heavy presets are uneven.
- **Preset switches.** These are the stalls while a new preset's shaders compile on the CPU.
  On a live stream, pmviz repeats the last frame for the time it missed, so the picture
  freezes briefly but stays in sync with the music. The paced run's `video` line counts the
  repeats.
- **h264_vaapi vs libx264.** This compares the GPU's video encoder with the CPU's. If
  `h264_vaapi` works, the encoding mostly leaves the CPU free.

As a rough guide: if the paced run repeats under 1% of frames, apart from preset switches,
projectM is viable on this machine. If it can't keep 720p30 even with VAAPI, the ffmpeg-only
visual styles are the fallback.

What was measured in the development container, which has no GPU and uses Mesa's llvmpipe on
4 CPU cores:

| run | result |
| --- | --- |
| 640x360, drawing only | 56 fps (1.9x real time) |
| 1280x720, drawing only, Cream of the Crop presets | 14–19 fps (0.5–0.6x real time), depending on the presets |
| 1280x720, paced to the music, with libx264 | 57% of frames repeated |
| 640x360, paced to the music, with libx264 | 20% of frames repeated; preset switches 0.5–0.9 s |

So software rendering isn't an option, and the decision rests on the server's GPU.

## Options

```
pmviz [-i music] [-p presets] [-t textures] [-o video] [-e libx264|h264_vaapi|h264_qsv]
      [-s 1280x720] [-r 30] [-T secs] [-d preset-secs] [-m 48x32]
      [--bench] [--unpaced] [--device /dev/dri/renderD129] [--software] [--in-order] [--seed N]
```

- `-i -` reads 32-bit float stereo samples at 44.1 kHz from stdin, which is how RMS would feed
  it the playing audio. With no `-i`, it uses a built-in test signal.
- The mesh (`-m`) is MilkDrop's per-pixel grid. Larger meshes look smoother and cost more.

## How it works, and what it means for RMS

- **Rendering.** An EGL pbuffer with an OpenGL ES 3 context, on the first GPU render node EGL
  finds, with software rendering as a fallback. projectM 4.1 draws to the default framebuffer.
- **Readback.** Frames are read through two pixel buffers, so frame N is read while frame N-1
  goes to ffmpeg as raw RGBA. ffmpeg flips it right side up and encodes it.
- **Timing.** projectM animates by the wall clock: version 4.1 has no way to set the frame
  time. So output must be drawn in real time, which a live stream needs anyway. Offline
  rendering faster than real time would get the motion wrong, which is why `--unpaced` is only
  for throughput tests.
- **Integration.**
  - pmviz would run as a helper process, one per TV session. RMS would write the playing
    audio's samples to its stdin and serve ffmpeg's output, for example HLS or fragmented MP4,
    to the Chromecast's default media receiver.
  - Keeping projectM out of process also contains crashes in third-party shaders.
  - projectM is LGPL-2.1, which is compatible with RMS's GPL-2.0.
- **The service.** `packaging/rms.service` has `PrivateDevices=yes`, which hides `/dev/dri`. A
  visualiser would need these lines in the unit:

  ```
  PrivateDevices=no
  DeviceAllow=/dev/dri/renderD128 rw
  SupplementaryGroups=render video
  ```

- **Presets.** The [Cream of the Crop](https://github.com/projectM-visualizer/presets-cream-of-the-crop)
  pack has about 9,800 presets, which are generally treated as public domain, and the
  [texture pack](https://github.com/projectM-visualizer/presets-milkdrop-texture-pack) holds
  the images they use. Some presets build up from black over a few seconds, and a few may
  fail to compile on a given driver. pmviz skips those and logs `preset failed`.
- **Switch stalls.** A smoother version would compile the next preset ahead of time on a
  second, shared GL context. projectM 4.1's API doesn't offer that, so the prototype measures
  the stall instead.
