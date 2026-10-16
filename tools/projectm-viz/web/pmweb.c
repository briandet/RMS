// projectM in the browser: a thin WebAssembly wrapper the demo page drives from JavaScript.
// The page feeds it the playing audio (from Web Audio) and calls pm_render() once a frame;
// projectM draws into the page's <canvas id="canvas"> through WebGL 2.

#include <emscripten.h>
#include <emscripten/html5.h>
#include <projectM-4/projectM.h>

static projectm_handle pm;

EM_JS(void, js_switch_requested, (int hard), {
    if (Module.onSwitchRequested) Module.onSwitchRequested(!!hard);
});
EM_JS(void, js_switch_failed, (const char *msg), {
    if (Module.onSwitchFailed) Module.onSwitchFailed(UTF8ToString(msg));
});

static void switch_requested(bool hard, void *user) {
    (void)user;
    js_switch_requested(hard);
}

static void switch_failed(const char *file, const char *msg, void *user) {
    (void)file;
    (void)user;
    js_switch_failed(msg ? msg : "");
}

// 0 on success, < 0 when WebGL 2 or projectM couldn't start.
EMSCRIPTEN_KEEPALIVE int pm_init(int width, int height, int mesh_w, int mesh_h) {
    EmscriptenWebGLContextAttributes a;
    emscripten_webgl_init_context_attributes(&a);
    a.majorVersion = 2;
    a.minorVersion = 0;
    a.alpha = false;
    a.depth = false;
    a.stencil = false;
    a.antialias = false;
    a.powerPreference = EM_WEBGL_POWER_PREFERENCE_HIGH_PERFORMANCE;
    EMSCRIPTEN_WEBGL_CONTEXT_HANDLE ctx = emscripten_webgl_create_context("#canvas", &a);
    if (ctx <= 0)
        return -1;
    emscripten_webgl_make_context_current(ctx);
    // float textures for the motion vector grid (see projectM's EMSCRIPTEN.md)
    emscripten_webgl_enable_extension(ctx, "OES_texture_float");
    emscripten_webgl_enable_extension(ctx, "OES_texture_float_linear");
    emscripten_webgl_enable_extension(ctx, "EXT_color_buffer_float");

    pm = projectm_create();
    if (!pm)
        return -2;
    projectm_set_window_size(pm, width, height);
    projectm_set_mesh_size(pm, mesh_w, mesh_h);
    // as the server's pmviz: presets change on the timer, blending for 3 seconds
    projectm_set_hard_cut_enabled(pm, false);
    projectm_set_soft_cut_duration(pm, 3);
    projectm_set_preset_duration(pm, 30);
    const char *paths[] = {"/textures"};
    projectm_set_texture_search_paths(pm, paths, 1);
    projectm_set_preset_switch_requested_event_callback(pm, switch_requested, NULL);
    projectm_set_preset_switch_failed_event_callback(pm, switch_failed, NULL);
    return 0;
}

EMSCRIPTEN_KEEPALIVE void pm_resize(int width, int height) {
    projectm_set_window_size(pm, width, height);
}

// `frames` stereo frames, interleaved left and right.
EMSCRIPTEN_KEEPALIVE void pm_pcm(const float *samples, int frames) {
    projectm_pcm_add_float(pm, samples, frames, PROJECTM_STEREO);
}

EMSCRIPTEN_KEEPALIVE void pm_render(void) {
    projectm_opengl_render_frame(pm);
}

EMSCRIPTEN_KEEPALIVE void pm_load(const char *preset, int smooth) {
    projectm_load_preset_data(pm, preset, smooth);
}

EMSCRIPTEN_KEEPALIVE void pm_preset_seconds(double seconds) {
    projectm_set_preset_duration(pm, seconds);
}

EMSCRIPTEN_KEEPALIVE void pm_fps(int fps) {
    projectm_set_fps(pm, fps);
}
