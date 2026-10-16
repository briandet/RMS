#!/bin/sh
# Build pmviz: fetch and build projectM 4 (static, OpenGL ES), compile pmviz against it, and
# fetch the "Cream of the Crop" preset pack and the MilkDrop texture pack.
#
#   tools/projectm-viz/build.sh
#
# Everything goes in tools/projectm-viz/build (ignored by git).  Needs, on Debian / Ubuntu:
#   sudo apt install build-essential cmake ninja-build git pkg-config ffmpeg \
#                    libegl-dev libgles-dev libegl-mesa0 libgl1-mesa-dri mesa-va-drivers
# (Fedora: gcc-c++ cmake ninja-build git ffmpeg mesa-libEGL-devel mesa-libGLES-devel;
#  Arch: base-devel cmake ninja git ffmpeg mesa)
set -eu

PROJECTM_VERSION=v4.1.8
cd "$(dirname "$0")"
here=$(pwd)
mkdir -p build
cd build

missing=""
for cmd in cc c++ cmake ninja git pkg-config ffmpeg; do
    command -v "$cmd" >/dev/null || missing="$missing $cmd"
done
pkg-config --exists egl glesv2 || missing="$missing libegl-dev/libgles-dev"
if [ -n "$missing" ]; then
    echo "missing:$missing (see the top of $0)" >&2
    exit 1
fi

if [ ! -f prefix/lib/libprojectM-4.a ]; then
    [ -d projectm ] || git clone --depth 1 --branch "$PROJECTM_VERSION" --recurse-submodules \
        --shallow-submodules https://github.com/projectM-visualizer/projectm.git projectm
    cmake -S projectm -B projectm/build -G Ninja -DCMAKE_BUILD_TYPE=Release \
        -DENABLE_GLES=ON -DBUILD_SHARED_LIBS=OFF -DENABLE_PLAYLIST=OFF \
        -DENABLE_SYSTEM_PROJECTM_EVAL=OFF -DCMAKE_INSTALL_PREFIX="$here/build/prefix" >/dev/null
    ninja -C projectm/build install >/dev/null
fi

cc -O2 -Wall -Wextra -o pmviz "$here/pmviz.c" -Iprefix/include -DPROJECTM_STATIC_DEFINE \
    prefix/lib/libprojectM-4.a -lstdc++ $(pkg-config --libs egl glesv2) -lm -lpthread

[ -d presets ] || git clone --depth 1 https://github.com/projectM-visualizer/presets-cream-of-the-crop.git presets
[ -d textures ] || git clone --depth 1 https://github.com/projectM-visualizer/presets-milkdrop-texture-pack.git textures

echo "built $here/build/pmviz; now run $here/bench.sh [some-music-file]"
