#!/bin/sh
# Build RMS and install (or upgrade) it as a systemd service on Debian / Ubuntu.
#
#   packaging/install.sh [--edit-files] [music-dir[,music-dir...]]
#
# --edit-files lets the service write to the music folders, for changing tags from the web
# UI (Settings → Library → "Change tags in music files"; it also needs a login).  Without it
# the service can only read them.
#
# Run from the repository as a normal user; it uses sudo for the system changes.
# Running it again upgrades: rebuild, replace the binary, restart.  Your settings in
# /etc/default/rms and the data in /var/lib/rms are kept.
set -eu

cd "$(dirname "$0")/.."
EDIT_FILES=0
if [ "${1:-}" = "--edit-files" ]; then
    EDIT_FILES=1
    shift
fi
MUSIC="${1:-}"

command -v cargo >/dev/null || {
    echo "cargo not found: install Rust with rustup (https://rustup.rs)" >&2
    exit 1
}
command -v ffmpeg >/dev/null ||
    echo "warning: ffmpeg not found (sudo apt install ffmpeg); transcoding and seeking in some formats will be unavailable" >&2
for svc in lyrionmusicserver logitechmediaserver; do
    if systemctl is-active --quiet "$svc" 2>/dev/null; then
        echo "warning: $svc is running and uses the same ports; stop it with: sudo systemctl disable --now $svc" >&2
    fi
done

echo "==> Building"
cargo build --release --locked -p rms --bin rms

echo "==> Installing /usr/local/bin/rms"
sudo install -m 755 target/release/rms /usr/local/bin/rms

# The TV visualiser (optional): pmviz and the MilkDrop presets, when its build dependencies
# are installed (see tools/projectm-viz/build.sh).
echo "==> Building the TV visualiser (pmviz)"
if tools/projectm-viz/build.sh; then
    sudo install -d /usr/local/lib/rms /usr/local/share/rms/projectm/presets /usr/local/share/rms/projectm/textures
    sudo install -m 755 tools/projectm-viz/build/pmviz /usr/local/lib/rms/pmviz
    for d in presets textures; do
        tar -C tools/projectm-viz/build/$d --exclude=.git -cf - . |
            sudo tar -C /usr/local/share/rms/projectm/$d -xf -
    done
else
    echo "note: the TV visualiser isn't installed (see above); Chromecasts play audio only" >&2
fi

if ! id rms >/dev/null 2>&1; then
    echo "==> Creating the rms system user"
    sudo useradd --system --home-dir /var/lib/rms --no-create-home --shell /usr/sbin/nologin rms
fi
# the GPU, for the visualiser
for g in render video; do
    if getent group "$g" >/dev/null && ! id -nG rms | grep -qw "$g"; then
        sudo usermod -aG "$g" rms
    fi
done

if [ ! -e /etc/default/rms ]; then
    echo "==> Writing /etc/default/rms"
    sudo install -m 644 packaging/rms.default /etc/default/rms
    if [ -n "$MUSIC" ]; then
        sudo sed -i "s|^RMS_MUSIC_DIR=.*|RMS_MUSIC_DIR=$MUSIC|" /etc/default/rms
    fi
elif [ -n "$MUSIC" ]; then
    echo "note: /etc/default/rms exists; edit RMS_MUSIC_DIR there to change the music folders" >&2
fi

echo "==> Installing the systemd service"
sudo install -m 644 packaging/rms.service /etc/systemd/system/rms.service
sudo systemctl daemon-reload
sudo systemctl enable rms >/dev/null 2>&1
sudo systemctl restart rms

dirs=$(sed -n 's/^RMS_MUSIC_DIR=//p' /etc/default/rms | tr ',' ' ')
dropin=/etc/systemd/system/rms.service.d/edit-files.conf
if [ "$EDIT_FILES" = 1 ]; then
    # the service's file system is read-only but for its data; open the music folders
    echo "==> Letting the service write to the music folders"
    sudo mkdir -p "$(dirname "$dropin")"
    printf '[Service]\nReadWritePaths=%s\n' "$dirs" | sudo tee "$dropin" >/dev/null
    sudo systemctl daemon-reload
    sudo systemctl restart rms
fi
# The service reads the music (and with --edit-files writes it); check it can.
for d in $dirs; do
    if ! sudo -u rms test -r "$d" -a -x "$d"; then
        echo "warning: the rms user cannot read $d; for example: sudo setfacl -R -m u:rms:rX -m d:u:rms:rX '$d'" >&2
    elif [ -e "$dropin" ] && ! sudo -u rms test -w "$d"; then
        echo "warning: the rms user cannot write to $d; for example: sudo setfacl -R -m u:rms:rwX -m d:u:rms:rwX '$d'" >&2
    fi
done

ip=$(hostname -I 2>/dev/null | cut -d' ' -f1)
port=$(sed -n 's/^RMS_HTTP_PORT=//p' /etc/default/rms)
cat <<MSG

RMS is running:  http://${ip:-<server>}:${port:-9000}/
  status:  systemctl status rms
  logs:    journalctl -u rms -f
  config:  /etc/default/rms   (then: sudo systemctl restart rms)
Firewall: allow TCP+UDP 3483 (players) and TCP ${port:-9000} (web UI, Squeezer), on the LAN only.
MSG
