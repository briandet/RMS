#!/bin/sh
# Build the projectM web demo: projectM 4 compiled to WebAssembly, a small wrapper, the demo
# page, a sample of the Cream of the Crop presets and the texture pack.
#
#   tools/projectm-viz/web/build.sh [number-of-presets]
#
# Needs the Emscripten SDK on the PATH (https://emscripten.org/docs/getting_started/,
# then `source emsdk_env.sh`), and tools/projectm-viz/build.sh run once for the sources,
# presets and textures.  Writes tools/projectm-viz/build/web/; serve that directory over
# HTTP (e.g. `python3 -m http.server -d tools/projectm-viz/build/web`) and open it.
set -eu

count="${1:-120}"
here=$(cd "$(dirname "$0")" && pwd)
build="$here/../build"
out="$build/web"
command -v emcc >/dev/null || { echo "emcc not found: source emsdk_env.sh first" >&2; exit 1; }
[ -d "$build/projectm" ] || { echo "run tools/projectm-viz/build.sh first" >&2; exit 1; }
cd "$build"

if [ ! -f prefix-web/lib/libprojectM-4.a ]; then
    # projectM's Emscripten build adds SDL for its own demo; the library doesn't use it
    rm -rf projectm-websrc projectm-web
    cp -r projectm projectm-websrc
    rm -rf projectm-websrc/build
    sed -i '/SHELL:-s USE_SDL=2/d' projectm-websrc/CMakeLists.txt
    emcmake cmake -S projectm-websrc -B projectm-web -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DENABLE_PLAYLIST=OFF -DENABLE_SYSTEM_PROJECTM_EVAL=OFF \
        -DCMAKE_INSTALL_PREFIX="$build/prefix-web" >/dev/null
    ninja -C projectm-web install >/dev/null
fi

rm -rf "$out"
mkdir -p "$out/presets" "$out/textures"
# the wrapper is C; projectM is C++, so em++ links in the C++ runtime
emcc -O2 -c "$here/pmweb.c" -Iprefix-web/include -DPROJECTM_STATIC_DEFINE -o projectm-web/pmweb.o
em++ -O2 projectm-web/pmweb.o prefix-web/lib/libprojectM-4.a -o "$out/pmweb.js" \
    -sMODULARIZE=1 -sEXPORT_NAME=createPmweb -sENVIRONMENT=web \
    -sMIN_WEBGL_VERSION=2 -sMAX_WEBGL_VERSION=2 -sFULL_ES2=1 -sFULL_ES3=1 \
    -sALLOW_MEMORY_GROWTH=1 -sNO_DISABLE_EXCEPTION_CATCHING -sFORCE_FILESYSTEM=1 \
    -sEXPORTED_FUNCTIONS=_malloc,_free \
    -sEXPORTED_RUNTIME_METHODS=FS,ccall,cwrap,HEAPF32,stringToNewUTF8

cp "$here/index.html" "$out/"
cp textures/textures/* "$out/textures/"

# a spread of presets: every Nth of the pack, so each category is represented
python3 - "$count" "$out" <<'PY'
import json, os, shutil, sys
count, out = int(sys.argv[1]), sys.argv[2]
files = sorted(os.path.join(d, f) for d, _, fs in os.walk("presets") for f in fs if f.endswith(".milk"))
step = max(1, len(files) // count)
chosen, names = files[::step][:count], []
for i, f in enumerate(chosen):
    dest = f"{i:03}.milk"
    shutil.copy(f, os.path.join(out, "presets", dest))
    names.append({"file": dest, "name": os.path.splitext(os.path.basename(f))[0],
                  "category": os.path.relpath(f, "presets").split(os.sep)[0]})
json.dump({"presets": names, "textures": sorted(os.listdir(os.path.join(out, "textures")))},
          open(os.path.join(out, "manifest.json"), "w"), indent=0)
PY

du -sh "$out"
echo "built $out: serve it over HTTP and open index.html"
