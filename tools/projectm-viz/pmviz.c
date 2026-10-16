// pmviz: render MilkDrop presets with projectM off screen (EGL, no display), fed with music,
// and either measure how fast it goes (--bench) or encode a video with the music (-o).
//
// A prototype for RMS's TV visualiser: can the server's GPU draw projectM at 720p30 in real
// time, with room to spare for the encoder?  See README.md.

#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <projectM-4/projectM.h>

#include <errno.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <ftw.h>
#include <getopt.h>
#include <math.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define RATE 44100

static struct {
    const char *input;      // audio file (decoded by ffmpeg), "-" for f32le stereo on stdin, NULL for a test signal
    const char *presets;    // a .milk file or a directory of them
    const char *textures;   // texture directory
    const char *output;     // video file, NULL: no output
    const char *encoder;    // libx264 | h264_vaapi | h264_qsv
    const char *device;     // DRM render node to prefer, e.g. /dev/dri/renderD128
    int width, height, fps;
    double seconds;         // stop after this long (0: when the audio ends)
    double preset_secs;     // switch presets this often
    int mesh_w, mesh_h;
    bool bench;             // no output, as fast as it goes
    bool unpaced;           // with -o: don't pace to real time (animation speed will be wrong)
    bool software;          // force a software (llvmpipe) EGL device
    bool shuffle;
    unsigned seed;
} opt = {
    .encoder = "libx264", .width = 1280, .height = 720, .fps = 30,
    .preset_secs = 20, .mesh_w = 48, .mesh_h = 32, .shuffle = true,
};

static volatile sig_atomic_t stop;
static void on_signal(int s) { (void)s; stop = 1; }

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void die(const char *fmt, ...) __attribute__((format(printf, 1, 2), noreturn));
static void die(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    fprintf(stderr, "pmviz: ");
    vfprintf(stderr, fmt, ap);
    fputc('\n', stderr);
    va_end(ap);
    exit(1);
}

// ------------------------------------------------------------------------------- EGL

static EGLDisplay try_display(bool want_hw) {
    PFNEGLQUERYDEVICESEXTPROC query_devices = (void *)eglGetProcAddress("eglQueryDevicesEXT");
    PFNEGLQUERYDEVICESTRINGEXTPROC device_string = (void *)eglGetProcAddress("eglQueryDeviceStringEXT");
    PFNEGLGETPLATFORMDISPLAYEXTPROC platform_display = (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (!query_devices || !device_string || !platform_display) return EGL_NO_DISPLAY;
    EGLDeviceEXT devs[16];
    EGLint n = 0;
    if (!query_devices(16, devs, &n)) return EGL_NO_DISPLAY;
    for (int pass = 0; pass < 2; pass++) {
        for (int i = 0; i < n; i++) {
            const char *exts = device_string(devs[i], EGL_EXTENSIONS);
            const char *node = exts && strstr(exts, "EGL_EXT_device_drm_render_node")
                ? device_string(devs[i], EGL_DRM_RENDER_NODE_FILE_EXT) : NULL;
            bool sw = exts && strstr(exts, "EGL_MESA_device_software");
            if (want_hw && (sw || !node)) continue;
            if (!want_hw && !sw) continue;
            // first pass: only the requested render node
            if (pass == 0 && opt.device && (!node || strcmp(node, opt.device) != 0)) continue;
            EGLDisplay d = platform_display(EGL_PLATFORM_DEVICE_EXT, devs[i], NULL);
            if (d != EGL_NO_DISPLAY && eglInitialize(d, NULL, NULL)) {
                fprintf(stderr, "pmviz: EGL device %d: %s\n", i, node ? node : "software");
                return d;
            }
        }
        if (!opt.device) break;
    }
    return EGL_NO_DISPLAY;
}

static void egl_setup(void) {
    EGLDisplay d = opt.software ? EGL_NO_DISPLAY : try_display(true);
    if (d == EGL_NO_DISPLAY) {
        if (!opt.software) fprintf(stderr, "pmviz: no GPU render node found through EGL, trying software rendering\n");
        d = try_display(false);
    }
    if (d == EGL_NO_DISPLAY) {
        d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
        if (d == EGL_NO_DISPLAY || !eglInitialize(d, NULL, NULL)) die("cannot open an EGL display");
    }
    const EGLint cfg_attrs[] = {
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE,
    };
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(d, cfg_attrs, &cfg, 1, &n) || n < 1) die("no EGL config with GLES 3 and pbuffers");
    // projectM draws to the default framebuffer: a pbuffer the size of the video
    const EGLint pb_attrs[] = { EGL_WIDTH, opt.width, EGL_HEIGHT, opt.height, EGL_NONE };
    EGLSurface s = eglCreatePbufferSurface(d, cfg, pb_attrs);
    if (s == EGL_NO_SURFACE) die("cannot create a %dx%d pbuffer", opt.width, opt.height);
    if (!eglBindAPI(EGL_OPENGL_ES_API)) die("no OpenGL ES");
    EGLContext c = EGL_NO_CONTEXT;
    for (int minor = 2; minor >= 0 && c == EGL_NO_CONTEXT; minor--) {
        const EGLint ctx_attrs[] = { EGL_CONTEXT_MAJOR_VERSION, 3, EGL_CONTEXT_MINOR_VERSION, minor, EGL_NONE };
        c = eglCreateContext(d, cfg, EGL_NO_CONTEXT, ctx_attrs);
    }
    if (c == EGL_NO_CONTEXT) die("cannot create a GLES 3 context");
    if (!eglMakeCurrent(d, s, s, c)) die("eglMakeCurrent failed");
    fprintf(stderr, "pmviz: GL_RENDERER: %s\npmviz: GL_VERSION:  %s\n",
            glGetString(GL_RENDERER), glGetString(GL_VERSION));
}

// ------------------------------------------------------------------------------- presets

static char **presets;
static size_t n_presets, cap_presets, next_preset;

static int add_preset(const char *path, const struct stat *st, int type, struct FTW *f) {
    (void)st; (void)f;
    size_t l = strlen(path);
    if (type == FTW_F && l > 5 && strcasecmp(path + l - 5, ".milk") == 0) {
        if (n_presets == cap_presets) {
            cap_presets = cap_presets ? cap_presets * 2 : 256;
            presets = realloc(presets, cap_presets * sizeof *presets);
        }
        presets[n_presets++] = strdup(path);
    }
    return 0;
}

static int cmp_str(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

static void find_presets(void) {
    if (!opt.presets) return;
    struct stat st;
    if (stat(opt.presets, &st) != 0) die("%s: %s", opt.presets, strerror(errno));
    if (S_ISDIR(st.st_mode)) nftw(opt.presets, add_preset, 32, FTW_PHYS);
    else add_preset(opt.presets, &st, FTW_F, NULL);
    if (!n_presets) die("no .milk presets in %s", opt.presets);
    qsort(presets, n_presets, sizeof *presets, cmp_str);
    if (opt.shuffle) {
        srand(opt.seed);
        for (size_t i = n_presets - 1; i > 0; i--) {
            size_t j = (size_t)rand() % (i + 1);
            char *t = presets[i]; presets[i] = presets[j]; presets[j] = t;
        }
    }
    fprintf(stderr, "pmviz: %zu presets\n", n_presets);
}

static int preset_failures;

static void load_next(projectm_handle pm, bool smooth) {
    if (!n_presets) return;
    const char *p = presets[next_preset++ % n_presets];
    const char *base = strrchr(p, '/');
    fprintf(stderr, "pmviz: preset %s\n", base ? base + 1 : p);
    projectm_load_preset_file(pm, p, smooth);
}

static void on_switch_failed(const char *file, const char *msg, void *user) {
    fprintf(stderr, "pmviz: preset failed (%s): %s\n", file, msg);
    if (++preset_failures < 20) load_next(user, false);
}

// ------------------------------------------------------------------------------- audio

static FILE *audio;

static void audio_open(void) {
    if (!opt.input) return;
    if (strcmp(opt.input, "-") == 0) { audio = stdin; return; }
    char cmd[4096];
    // the input path is quoted for the shell: ' becomes '\''
    char q[3000], *o = q;
    for (const char *s = opt.input; *s && o < q + sizeof q - 5; s++) {
        if (*s == '\'') { memcpy(o, "'\\''", 4); o += 4; } else *o++ = *s;
    }
    *o = 0;
    snprintf(cmd, sizeof cmd, "ffmpeg -nostdin -hide_banner -loglevel error -i '%s' -f f32le -ac 2 -ar %d -", q, RATE);
    audio = popen(cmd, "r");
    if (!audio) die("cannot run ffmpeg");
}

/// Fill `buf` with `frames` stereo frames; false at the end of the audio.
static bool audio_read(float *buf, int frames, long frame_no) {
    if (!audio) {
        // test signal: a kick on every beat at 120 BPM over a slowly moving chord
        for (int i = 0; i < frames; i++) {
            double t = (double)(frame_no * frames + i) / RATE;
            double beat = fmod(t * 2.0, 1.0);
            double kick = exp(-beat * 25.0) * sin(2 * M_PI * 55.0 * t);
            double chord = sin(2 * M_PI * 220.0 * t) + sin(2 * M_PI * (277.2 + 20 * sin(t / 3)) * t)
                + 0.5 * sin(2 * M_PI * 3520.0 * t) * exp(-fmod(t * 4.0, 1.0) * 40.0);
            buf[2 * i] = buf[2 * i + 1] = (float)(0.6 * kick + 0.12 * chord);
        }
        return true;
    }
    size_t got = fread(buf, sizeof(float) * 2, (size_t)frames, audio);
    if (got < (size_t)frames) memset(buf + 2 * got, 0, (frames - got) * 2 * sizeof(float));
    return got > 0;
}

// ------------------------------------------------------------------------------- video out

static FILE *video;

static void video_open(void) {
    if (!opt.output) return;
    char vf[256], enc[512], ain[3200] = "", amap[64] = "";
    if (opt.input && strcmp(opt.input, "-") != 0) {
        char q[3000], *o = q;
        for (const char *s = opt.input; *s && o < q + sizeof q - 5; s++) {
            if (*s == '\'') { memcpy(o, "'\\''", 4); o += 4; } else *o++ = *s;
        }
        *o = 0;
        snprintf(ain, sizeof ain, "-i '%s'", q);
        snprintf(amap, sizeof amap, "-map 1:a -c:a aac -b:a 192k -shortest");
    }
    const char *dev = opt.device ? opt.device : "/dev/dri/renderD128";
    if (strcmp(opt.encoder, "h264_vaapi") == 0) {
        snprintf(vf, sizeof vf, "vflip,format=nv12,hwupload");
        snprintf(enc, sizeof enc, "-vaapi_device %s", dev);
    } else if (strcmp(opt.encoder, "h264_qsv") == 0) {
        snprintf(vf, sizeof vf, "vflip,format=nv12");
        snprintf(enc, sizeof enc, "-init_hw_device qsv=hw -filter_hw_device hw");
    } else {
        snprintf(vf, sizeof vf, "vflip,format=yuv420p");
        snprintf(enc, sizeof enc, " ");
    }
    char cmd[8192];
    snprintf(cmd, sizeof cmd,
             "ffmpeg -nostdin -hide_banner -loglevel warning -y %s "
             "-f rawvideo -pix_fmt rgba -s %dx%d -r %d -i - %s "
             "-map 0:v -vf '%s' -c:v %s %s -g %d %s '%s'",
             enc, opt.width, opt.height, opt.fps, ain, vf, opt.encoder,
             strcmp(opt.encoder, "libx264") == 0 ? "-preset veryfast -tune animation -crf 20" : "-b:v 6M",
             opt.fps * 2, amap, opt.output);
    fprintf(stderr, "pmviz: %s\n", cmd);
    video = popen(cmd, "w");
    if (!video) die("cannot run ffmpeg");
}

// ------------------------------------------------------------------------------- main

static void usage(void) {
    fprintf(stderr,
        "usage: pmviz [options]\n"
        "  -i FILE      music to visualise (any format ffmpeg reads; - for f32le stereo 44.1 kHz on stdin;\n"
        "               default: a built-in test signal)\n"
        "  -p PATH      a .milk preset, or a directory of them (searched recursively)\n"
        "  -t DIR       texture directory for the presets\n"
        "  -o FILE      encode a video with the music (mp4, mkv, ts, …)\n"
        "  -e ENCODER   libx264 (default), h264_vaapi or h264_qsv\n"
        "  -s WxH       size (default 1280x720)\n"
        "  -r FPS       frames a second (default 30)\n"
        "  -T SECS      stop after this long (default: when the music ends; 30 with the test signal)\n"
        "  -d SECS      switch presets this often (default 20)\n"
        "  -m WxH       per-pixel mesh size (default 48x32; MilkDrop's high quality is 64x48+)\n"
        "  --bench      draw as fast as possible, no output, and report the timings\n"
        "  --unpaced    with -o: don't hold frames to real time (presets animate by the wall clock,\n"
        "               so the video's motion is wrong; only to test the encoder's throughput)\n"
        "  --device N   DRM render node to use (default: the first GPU EGL finds)\n"
        "  --software   draw with Mesa's software renderer\n"
        "  --in-order   presets in name order instead of shuffled; --seed N for the shuffle\n");
    exit(2);
}

static void parse_size(const char *s, int *w, int *h) {
    if (sscanf(s, "%dx%d", w, h) != 2 || *w < 16 || *h < 16) die("bad size %s", s);
}

int main(int argc, char **argv) {
    static const struct option longs[] = {
        { "bench", no_argument, 0, 'B' }, { "unpaced", no_argument, 0, 'U' },
        { "device", required_argument, 0, 'D' }, { "software", no_argument, 0, 'S' },
        { "in-order", no_argument, 0, 'O' }, { "seed", required_argument, 0, 'Z' },
        { "help", no_argument, 0, 'h' }, { 0 },
    };
    opt.seed = (unsigned)time(NULL);
    int c;
    while ((c = getopt_long(argc, argv, "i:p:t:o:e:s:r:T:d:m:h", longs, NULL)) != -1) {
        switch (c) {
        case 'i': opt.input = optarg; break;
        case 'p': opt.presets = optarg; break;
        case 't': opt.textures = optarg; break;
        case 'o': opt.output = optarg; break;
        case 'e': opt.encoder = optarg; break;
        case 's': parse_size(optarg, &opt.width, &opt.height); break;
        case 'r': opt.fps = atoi(optarg); break;
        case 'T': opt.seconds = atof(optarg); break;
        case 'd': opt.preset_secs = atof(optarg); break;
        case 'm': parse_size(optarg, &opt.mesh_w, &opt.mesh_h); break;
        case 'B': opt.bench = true; break;
        case 'U': opt.unpaced = true; break;
        case 'D': opt.device = optarg; break;
        case 'S': opt.software = true; break;
        case 'O': opt.shuffle = false; break;
        case 'Z': opt.seed = (unsigned)strtoul(optarg, NULL, 10); break;
        default: usage();
        }
    }
    if (opt.fps < 1 || opt.fps > 240) die("bad frame rate");
    if (opt.bench && opt.output) die("--bench writes no output; leave out -o");
    if (!opt.input && opt.seconds <= 0) opt.seconds = 30;
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    signal(SIGPIPE, SIG_IGN);

    egl_setup();
    find_presets();

    projectm_handle pm = projectm_create();
    if (!pm) die("projectm_create failed (is the GL context usable?)");
    char *ver = projectm_get_version_string();
    fprintf(stderr, "pmviz: projectM %s, %dx%d at %d fps, mesh %dx%d\n", ver, opt.width, opt.height,
            opt.fps, opt.mesh_w, opt.mesh_h);
    projectm_free_string(ver);
    projectm_set_window_size(pm, opt.width, opt.height);
    projectm_set_fps(pm, opt.fps);
    projectm_set_mesh_size(pm, opt.mesh_w, opt.mesh_h);
    projectm_set_aspect_correction(pm, true);
    // we switch presets ourselves, on a timer
    projectm_set_preset_duration(pm, 1e9);
    projectm_set_hard_cut_enabled(pm, false);
    projectm_set_soft_cut_duration(pm, 3);
    projectm_set_preset_locked(pm, true);
    projectm_set_preset_switch_failed_event_callback(pm, on_switch_failed, pm);
    if (opt.textures) {
        const char *paths[] = { opt.textures };
        projectm_set_texture_search_paths(pm, paths, 1);
    }
    load_next(pm, false);

    audio_open();
    video_open();

    const int spf = RATE / opt.fps; // audio frames per video frame
    float *pcm = malloc(sizeof(float) * 2 * (size_t)spf);
    const size_t frame_bytes = (size_t)opt.width * opt.height * 4;

    // two pixel buffers: read frame N into one while the other (frame N-1) is written out
    GLuint pbo[2];
    glGenBuffers(2, pbo);
    for (int i = 0; i < 2; i++) {
        glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo[i]);
        glBufferData(GL_PIXEL_PACK_BUFFER, (GLsizeiptr)frame_bytes, NULL, GL_STREAM_READ);
    }
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    const bool readback = opt.output || opt.bench;

    double t_render = 0, t_read = 0, t_write = 0, worst = 0, worst_switch = 0, sum_switch = 0;
    long frames = 0, late = 0, switches = 0, repeated = 0;
    long out = 0;     // video frames due so far (drawn ones plus repeats)
    int reps[2] = { 1, 1 }; // how many times the frame in each pixel buffer goes out
    const long max_frames = opt.seconds > 0 ? (long)(opt.seconds * opt.fps) : -1;
    const long preset_frames = (long)(opt.preset_secs * opt.fps);
    const bool paced = opt.output && !opt.unpaced;
    const double start = now();
    double report = start + 5, next_switch = (double)preset_frames;

    // write the frame in pixel buffer `slot` out `n` times
    #define WRITE_OUT(slot, n) do {                                                              \
        glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo[slot]);                                          \
        void *px_ = glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, (GLsizeiptr)frame_bytes, GL_MAP_READ_BIT); \
        for (int k_ = 0; px_ && video && k_ < (n) && !stop; k_++)                               \
            if (fwrite(px_, frame_bytes, 1, video) != 1) {                                      \
                fprintf(stderr, "pmviz: the encoder stopped\n");                                \
                stop = 1;                                                                       \
            }                                                                                   \
        glUnmapBuffer(GL_PIXEL_PACK_BUFFER);                                                    \
    } while (0)

    while (!stop && (max_frames < 0 || out < max_frames)) {
        // the music up to this frame's time; with repeats, the audio of the skipped frames too
        long due_audio = out + 1;
        bool ended = false;
        static long fed = 0;
        while (fed < due_audio) {
            if (!audio_read(pcm, spf, fed)) { ended = true; break; }
            projectm_pcm_add_float(pm, pcm, (unsigned)spf, PROJECTM_STEREO);
            fed++;
        }
        if (ended) break;

        double t0 = now();
        bool switching = preset_frames > 0 && out >= next_switch;
        if (switching) {
            next_switch += preset_frames;
            load_next(pm, true);
        }
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        glViewport(0, 0, opt.width, opt.height);
        projectm_opengl_render_frame(pm);
        if (opt.bench) glFinish(); // so the render time is the GPU's, not just the queueing
        double t1 = now();
        double took = t1 - t0;
        t_render += took;

        // paced: this frame covers every frame slot until the next one can be drawn, so the
        // video keeps time with the music even when a frame was late
        int n = 1;
        if (paced) {
            long due = (long)((t1 - start) * opt.fps) + 1;
            if (due > out + 1) n = (int)(due - out);
            if (max_frames >= 0 && out + n > max_frames) n = (int)(max_frames - out);
            if (n < 1) n = 1;
        }
        repeated += n - 1;
        out += n;

        if (readback) {
            int slot = (int)(frames & 1);
            reps[slot] = n;
            glBindFramebuffer(GL_READ_FRAMEBUFFER, 0);
            glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo[slot]);
            glReadPixels(0, 0, opt.width, opt.height, GL_RGBA, GL_UNSIGNED_BYTE, 0);
            double t2 = now();
            t_read += t2 - t1;
            if (frames > 0) WRITE_OUT(slot ^ 1, reps[slot ^ 1]);
            glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
            t_write += now() - t2;
        }
        double t_end = now();
        if (switching) {
            switches++;
            sum_switch += t_end - t0;
            if (t_end - t0 > worst_switch) worst_switch = t_end - t0;
        } else {
            if (t_end - t0 > worst) worst = t_end - t0;
            if (t_end - t0 > 1.0 / opt.fps) late++;
        }
        frames++;

        if (paced) {
            double wait = start + (double)out / opt.fps - now();
            if (wait > 0) {
                struct timespec ts = { (time_t)wait, (long)((wait - (time_t)wait) * 1e9) };
                nanosleep(&ts, NULL);
            }
        }
        if (now() >= report) {
            report += 5;
            double el = now() - start;
            fprintf(stderr, "pmviz: %ld frames drawn in %.1f s: %.1f fps (render %.1f ms, readback %.1f ms, out %.1f ms a frame)%s\n",
                    frames, el, frames / el, 1e3 * t_render / frames, 1e3 * t_read / frames,
                    1e3 * t_write / frames, paced && repeated ? ", some repeated to keep time" : "");
        }
    }
    // the last frame still in its pixel buffer
    if (video && frames > 0) WRITE_OUT((frames - 1) & 1, reps[(frames - 1) & 1]);
    double el = now() - start;
    if (frames == 0) die("no frames drawn");
    long normal = frames - switches;
    double per = (t_render + t_read + t_write) / frames;
    printf("\n%ld frames drawn, %dx%d, mesh %dx%d, %s\n", frames, opt.width, opt.height, opt.mesh_w,
           opt.mesh_h, glGetString(GL_RENDERER));
    printf("  render    %6.2f ms a frame\n", 1e3 * t_render / frames);
    printf("  readback  %6.2f ms a frame\n", 1e3 * t_read / frames);
    if (video) printf("  to ffmpeg %6.2f ms a frame (includes waiting for the encoder)\n", 1e3 * t_write / frames);
    if (normal > 0)
        printf("  slowest   %6.2f ms; %ld of %ld frames over the %.1f ms budget (%.1f%%)\n", 1e3 * worst, late,
               normal, 1e3 / opt.fps, 100.0 * late / normal);
    if (switches)
        printf("  preset switches: %ld, %.0f ms on average, slowest %.0f ms (compiling the shaders)\n", switches,
               1e3 * sum_switch / switches, 1e3 * worst_switch);
    if (paced)
        printf("  video     %ld frames, %ld of them repeats of a late one (%.1f%%)\n", out, repeated,
               100.0 * repeated / out);
    printf("  overall   %.1f fps drawn over %.1f s", frames / el, el);
    if (!paced) printf(" = %.1fx real time at %d fps", (1.0 / opt.fps) / per, opt.fps);
    printf("\n");

    projectm_destroy(pm);
    if (audio && audio != stdin) pclose(audio);
    int rc = 0;
    if (video) {
        rc = pclose(video);
        if (rc != 0) fprintf(stderr, "pmviz: ffmpeg failed (%d)\n", rc);
        else fprintf(stderr, "pmviz: wrote %s\n", opt.output);
    }
    return rc != 0;
}
