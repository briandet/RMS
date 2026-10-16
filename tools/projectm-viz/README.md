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

The first run on an Intel NUC (i5-1135G7, Iris Xe, Mesa 25.2):

| run | result |
| --- | --- |
| 1280x720, drawing only | 15 ms a frame (2.2x real time); 1 frame in 1,791 over budget |
| 1920x1080, drawing only | 15 ms a frame (2.2x real time): the GPU isn't what limits it |
| preset switches | 50–140 ms on average, at most 385 ms |
| 1280x720, drawing and libx264 | 1.0x real time: 22 ms a frame getting it to ffmpeg |
| 1280x720, paced to the music | 19% of frames repeated |

The GPU keeps up easily; the output path didn't.  That version copied each frame to ffmpeg
straight out of the mapped pixel buffer, which the CPU reads uncached (about 20 ms for 3.7
MB), and bench mode never copied it at all.  The readback now copies with streaming loads,
counts that copy in bench mode too, and `--readback direct` is there to compare.  VAAPI on
this GPU only offers constant quality (`CQP`), which pmviz now asks for.

The second run, with those changes:

| run | result |
| --- | --- |
| 1280x720, drawing and readback | 15 ms a frame (2.2x real time) |
| 1920x1080, drawing and readback | 17 ms a frame (1.9x real time) |
| preset switches | about 50 ms on average, at most 86 ms |
| encoders alone, 720p | libx264 2.9x real time; h264_vaapi 5.7x |
| 1280x720, drawing and libx264 | 1.1–1.2x real time (x264 and projectM share the CPU) |
| 1280x720, drawing and h264_vaapi | 2.8x real time; 1 frame in 896 over budget |
| 1280x720, paced to a song, h264_vaapi | 8 of 1,800 frames repeated (0.4%), mostly at the 3 preset switches; about 13 ms of each 33 ms frame used |
| 1920x1080, drawing and h264_vaapi | 1.9x real time; 11 of 896 frames over budget |
| 1920x1080, paced to a song, h264_vaapi | 7 of 1,800 frames repeated (0.4%); about 17 ms of each 33 ms frame used, 13 of them copying the frame out and piping it to ffmpeg |
| 1920x1080 at 60 fps, drawing and h264_vaapi | 1.0x real time (RGBA); 1.4x with `--gpu-yuv` |
| 1920x1080 at 60 fps, paced to a song, `--gpu-yuv` | 63 of 3,600 frames repeated (1.8%), 19% of frames over the 16.7 ms budget, mostly once the shader-heavy presets came on |
| drawing up to 150 ms ahead, drawn at 1920x1080 / 1280x720 | **0.7% / 0.2% repeated** (51 and 16 of 7,200), 12.5% / 3.2% of frames over budget: the margin absorbs switches and spikes; only `beta106S` (17.7 ms a frame) stays over |
| with the readback ring and writer thread, drawn at 1920x1080 / 1280x720 | 3.2% / 2.2% repeated, 9.8% / 4.3% of frames over budget: what is left is mostly preset switches, the first frame and single slow frames |
| the same over 2 minutes and 12 presets, drawn at 1920x1080 / 1600x900 / 1280x720 | 6.2% / 3.2% / 2.3% of frames repeated: drawing fewer pixels barely helps, so most of a frame is a fixed cost (projectM's per-frame and mesh work on the CPU, and pmviz's readback and pipe, then all on one thread) |

So the TV visualiser is viable on this machine at 720p30 and 1080p30 with the GPU's encoder; with
libx264 it would only just keep up, and starve the rest of RMS.  The two readbacks are
close (the streaming copy slightly ahead), so pixel buffers stay the default.

## Options

```
pmviz [-i music] [-p presets] [-t textures] [-o video] [-e libx264|h264_vaapi|h264_qsv]
      [-s 1280x720] [-r 30] [-T secs] [-d preset-secs] [-m 48x32]
      [--bench] [--unpaced] [--gpu-yuv] [--render-size WxH] [--ahead MS] [--readback pbo|direct] [--bars]
      [--device /dev/dri/renderD129] [--software] [--in-order] [--seed N]
```

- `-i -` reads 32-bit float stereo samples at 44.1 kHz from stdin, which is how RMS feeds
  it the playing audio. With no `-i`, it uses a built-in test signal.
- `-o -` writes fragmented MP4 (a fragment a second) to stdout, to stream as it comes: RMS's
  TV visualiser runs `pmviz -i - -o - --gpu-yuv …`. With `-i -` the same samples are muxed
  in as AAC, so the video carries the music. When the reader pauses (a paused TV) or the
  input stalls, the clock waits instead of repeating frames.
- The mesh (`-m`) is MilkDrop's per-pixel grid. Larger meshes look smoother and cost more.
- `--gpu-yuv` converts each frame to NV12 on the GPU (see below): 1.5 bytes a pixel to copy
  and pipe instead of 4, and nothing left for ffmpeg to convert.
- `--render-size` has projectM draw smaller than the video (`-s`), scaled up on the GPU by
  the `--gpu-yuv` pass (or by ffmpeg without it). Heavy presets cost about as much as the
  pixels they draw, and MilkDrop's soft visuals scale up well.
- `--ahead` (default 150 ms): paced drawing may run this far ahead of the clock, so a slow
  frame or a preset switch uses up the margin instead of repeating frames. Each frame gets
  the music of its own time slot, and the video's timestamps are fixed, so the sync with
  the music doesn't change.
- At each preset switch pmviz prints how the preset drew (time a frame, worst, share over
  budget), to find presets too heavy for the GPU.
- `--bars` draws colour bars over a grey ramp instead of projectM, to check the colours and
  orientation of the output.

## How it works, and what it means for RMS

- **Rendering.** An EGL pbuffer with an OpenGL ES 3 context, on the first GPU render node EGL
  finds, with software rendering as a fallback. projectM 4.1 draws to the default framebuffer.
- **Readback.** Frames are read through a ring of three pixel buffers, so frame N is read
  while frame N-2 is copied out: the GPU has had two frames' time to finish it. A writer
  thread feeds ffmpeg from a few buffers, so a busy encoder doesn't hold up drawing ("to
  ffmpeg" is then only the time spent waiting for a free buffer). By default that's raw RGBA, which ffmpeg flips right side up and converts
  to YUV on the CPU. With `--gpu-yuv` a shader pass does both on the GPU: luma at full size
  and chroma at half size each way, packed four bytes to an RGBA texel because OpenGL ES only
  promises RGBA reads, which is also NV12's byte order.
- **Colours.** HD video uses the BT.709 matrix in limited ("TV") range. Both paths convert
  with it and tag the stream so (`-colorspace bt709 …`). Before, ffmpeg converted with its
  default for RGB input, SD's BT.601, and left the stream untagged, so colours were slightly
  off on HD players.
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
