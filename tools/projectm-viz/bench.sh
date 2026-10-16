#!/bin/sh
# Measure how well this machine draws projectM for a TV visualiser, and make a sample video.
#
#   tools/projectm-viz/bench.sh [music-file]
#
# Runs in a few minutes and writes everything it prints to build/bench-<host>.txt, to paste
# back.  The sample video, build/sample.mp4, can be played on (or cast to) the TV to judge it.
set -u

cd "$(dirname "$0")/build" 2>/dev/null || { echo "run build.sh first" >&2; exit 1; }
music="${1:-}"
log="bench-$(hostname).txt"
P="-p presets -t textures/textures --seed 42"

run() {
    echo
    echo "### pmviz $*"
    ./pmviz "$@" 2>&1 | grep -v '^pmviz: preset \|frames drawn in\|^pmviz: ffmpeg '
}

{
    echo "# $(hostname), $(date -u +%Y-%m-%dT%H:%MZ)"
    grep -m1 'model name' /proc/cpuinfo
    echo "cores: $(nproc)"
    ls /dev/dri 2>/dev/null | tr '\n' ' '; echo
    if [ ! -e /dev/dri/renderD128 ]; then
        echo "note: no /dev/dri/renderD128: no GPU driver loaded, so this measures software rendering"
    elif [ ! -r /dev/dri/renderD128 ]; then
        echo "note: /dev/dri/renderD128 isn't readable by $(id -un): add yourself to the render group"
        echo "      (sudo usermod -aG render $(id -un), then log in again) or the GPU can't be used"
    fi
    command -v vainfo >/dev/null && vainfo 2>/dev/null | grep -E 'Driver version|VAProfileH264(High|Main).*EncSlice' | head -3

    # 1. drawing alone, as fast as it goes: the headroom over 30 fps
    run --bench -T 60 -d 6 $P
    run --bench -T 60 -d 6 -m 64x48 $P
    run --bench -T 30 -d 6 -s 1920x1080 $P

    # 2. drawing and encoding together, as fast as it goes (the motion is wrong in these files;
    #    they only measure throughput)
    run -T 30 -d 6 --unpaced -o /tmp/pmviz-x264.mp4 $P
    run -T 30 -d 6 --unpaced -e h264_vaapi -o /tmp/pmviz-vaapi.mp4 $P

    # 3. the real thing: paced to the music, as a live stream would be
    if [ -n "$music" ]; then
        run -i "$music" -T 60 -d 15 -o sample.mp4 $P
    else
        run -T 60 -d 15 -o sample.mp4 $P
    fi
    rm -f /tmp/pmviz-x264.mp4 /tmp/pmviz-vaapi.mp4
} 2>&1 | tee "$log"

echo
echo "wrote $(pwd)/$log and $(pwd)/sample.mp4"
