#!/bin/sh
# Build the site GitHub Pages serves: RMS's Cast receiver app (/receiver/) and the
# projectM browser demo (/demo/).
#
#   receiver/build.sh [number-of-presets]
#
# Needs the Emscripten SDK on the PATH (see tools/projectm-viz/web/build.sh).  Writes
# receiver/build/site/; try the receiver in a browser with
#   python3 -m http.server -d receiver/build/site 8000
#   http://localhost:8000/receiver/?dev&debug&src=<an audio URL>
set -eu

here=$(cd "$(dirname "$0")" && pwd)
web="$here/../tools/projectm-viz/build/web"
site="$here/build/site"

"$here/../tools/projectm-viz/web/build.sh" "${1:-400}"

rm -rf "$site"
mkdir -p "$site/receiver"
cp -r "$web" "$site/demo"
cp -r "$web/pmweb.js" "$web/pmweb.wasm" "$web/manifest.json" "$web/presets" "$web/textures" \
    "$site/receiver/"
cp "$here/index.html" "$here/receiver.js" "$site/receiver/"
# the drawing, the track info and the Web Audio player, shared with the web UI
for f in viz.js web-audio-player.js pcm-output.js pcm-worklet.js decoders.js demux.js; do
    cp "$here/../crates/rms/web/$f" "$site/receiver/"
done
cat > "$site/index.html" <<'HTML'
<!doctype html><meta charset="utf-8"><title>RMS</title>
<meta http-equiv="refresh" content="0; url=demo/">
<p><a href="demo/">projectM in the browser</a> · <a href="receiver/">RMS's Cast receiver</a></p>
HTML
touch "$site/.nojekyll"
du -sh "$site"
echo "built $site"
