#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Record a demo of cornice running on river (river-paddd) + tailrace and write an animated GIF:
#
#   scripts/live/demo.sh                     -> /var/tmp/cornice-live-demo/cornice-live.gif
#   W=/tmp/x scripts/live/demo.sh            -> /tmp/x/cornice-live.gif
#
# Full-resolution frames come from grim as fast as it manages (~25 fps, `grim -c` so the pointer is in the
# picture) and land in $W/frames — a few hundred MB, which is why $W defaults to the disk-backed /var/tmp.
# gif.py then halves them and assembles the GIF. river and tailrace are read-only inputs, as in run.sh.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
W=${W:-/var/tmp/cornice-live-demo}
RIVER_BIN=${RIVER_BIN:-$HOME/.local/bin/river}
TAILRACE_BIN=${TAILRACE_BIN:-$ROOT/../tailrace/target/release/tailrace}
CORNICE_BIN=${CORNICE_BIN:-$ROOT/target/debug/cornice}
PROBE=$W/build/probe
OUT_W=1280
OUT_H=720
BAR_H=30

stop_tree() {
    [ -n "${1:-}" ] || return 0
    kill -TERM -- "-$1" 2>/dev/null || kill -TERM "$1" 2>/dev/null
    return 0
}
cleanup() {
    rm -f "$W/stop"
    stop_tree "${rec_bg:-}"
    stop_tree "${cornice_bg:-}"
    stop_tree "${win_bg:-}"
    stop_tree "${vp_bg:-}"
    stop_tree "${river_bg:-}"
    exec 9>&- 2>/dev/null
}
trap cleanup EXIT

for b in "$RIVER_BIN" "$TAILRACE_BIN" "$CORNICE_BIN"; do
    [ -x "$b" ] || { echo "missing binary: $b"; exit 2; }
done

mkdir -p "$W/build"
if [ ! -x "$PROBE" ] || [ "$HERE/probe.c" -nt "$PROBE" ]; then
    rm -rf "$W/build"; mkdir -p "$W/build"
    wayland-scanner client-header /usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml "$W/build/xdg-shell-client-protocol.h"
    wayland-scanner private-code  /usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml "$W/build/xdg-shell-protocol.c"
    wayland-scanner client-header "$HERE/wlr-virtual-pointer-unstable-v1.xml" "$W/build/vlr.h"
    wayland-scanner private-code  "$HERE/wlr-virtual-pointer-unstable-v1.xml" "$W/build/vlr.c"
    mv "$W/build/vlr.h" "$W/build/wlr-virtual-pointer-unstable-v1-client-protocol.h"
    cc -o "$PROBE" "$HERE/probe.c" "$W/build/xdg-shell-protocol.c" "$W/build/vlr.c" -I"$W/build" \
        $(pkg-config --cflags --libs wayland-client) || { echo "probe build failed"; exit 2; }
    rm -f "$W/build/vlr.h" "$W/build/vlr.c"
fi

# ---------------------------------------------------------------- session

rm -rf "$W/run" "$W/cfg"; mkdir -p "$W/run" "$W/cfg/cornice"; chmod 700 "$W/run"
cat > "$W/cfg/cornice/config.toml" <<'EOF'
# The demo config uses a slower enter so each independent card can be sampled clearly.
[bar]
height   = 30
margin   = 0
padding  = 8
spacing  = 6

[theme]
background = "#1a1a1aee"
foreground = "#dcdcdc"
accent     = "#88c0d0"
radius     = 15

[notification]
position = "right"
max_visible = 4
enter_ms = 1200
exit_ms = 450

[bar.left]
modules = [ { kind = "clock", format = "%H:%M" } ]

[bar.center]
modules = [ { kind = "exec", command = "echo cornice", format = "{out}" } ]

[bar.right]
modules = [ { kind = "exec", command = "echo 87%", format = "{out}" } ]
EOF
export XDG_RUNTIME_DIR=$W/run
export WAYLAND_DISPLAY=wayland-1
export XDG_CONFIG_HOME=$W/cfg

WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    setsid "$RIVER_BIN" -c "$TAILRACE_BIN" >"$W/river.log" 2>&1 </dev/null &
river_bg=$!
for _ in $(seq 60); do [ -S "$W/run/wayland-1" ] && break; sleep 0.1; done

cat > "$W/bus.sh" <<EOF
#!/bin/sh
echo "\$DBUS_SESSION_BUS_ADDRESS" > $W/dbus.addr
exec $CORNICE_BIN
EOF
chmod +x "$W/bus.sh"
setsid dbus-run-session -- "$W/bus.sh" >"$W/cornice.log" 2>&1 </dev/null &
cornice_bg=$!
for _ in $(seq 60); do grep -q 'bar configured' "$W/cornice.log" 2>/dev/null && break; sleep 0.1; done

# the toplevel the demo clicks through to: a dark slate "window" that flashes white on click
setsid env PROBE_COLOR=2f4858 PROBE_FLASH=e8f0f8 "$PROBE" win >"$W/win.log" 2>&1 </dev/null &
win_bg=$!
rm -f "$W/fifo"; mkfifo "$W/fifo"
setsid "$PROBE" vp <"$W/fifo" >"$W/vp.log" 2>&1 &
vp_bg=$!
exec 9>"$W/fifo"

vp() { printf '%s\n' "$*" >&9; }
notify() {
    DBUS_SESSION_BUS_ADDRESS=$(cat "$W/dbus.addr") gdbus call --session \
        --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify -- "$@" >/dev/null
}
close_all() { local i; for i in $(seq 1 40); do
    DBUS_SESSION_BUS_ADDRESS=$(cat "$W/dbus.addr") gdbus call --session \
        --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.CloseNotification -- "$i" >/dev/null 2>&1; done; }

curx=640; cury=430
glide() { # glide X Y [steps] — the virtual pointer jumps, so walk it to show real motion
    local tx=$1 ty=$2 steps=${3:-10} i
    for i in $(seq 1 "$steps"); do
        vp at $(( curx + (tx - curx) * i / steps )) $(( cury + (ty - cury) * i / steps ))
        sleep 0.03
    done
    curx=$tx; cury=$ty
    sleep 0.12
}
click() { vp click left; }

# ---------------------------------------------------------------- recording

record_start() {
    rm -rf "$W/frames"; mkdir -p "$W/frames"; rm -f "$W/stop"
    (
        i=0
        while [ ! -f "$W/stop" ]; do
            grim -c -t ppm "$W/frames/f$(printf %04d "$i").ppm" 2>/dev/null
            date +%s%N >>"$W/frames/times"
            i=$((i + 1))
            sleep 0.03
        done
    ) &
    rec_bg=$!
}
record_stop() {
    : >"$W/stop"
    sleep 1
}

still() { grim -c -t ppm "$W/still.ppm"; }

echo "recording: river + tailrace + cornice, ${OUT_W}x${OUT_H}"
vp at $curx $cury
record_start
sleep 0.8

glide 1200 22 6                                   # up to the top-right notification anchor
notify demo 0 "" "cornice" "independent cards" "[]" "{}" 0
sleep 1.8
notify demo 0 "" "battery" "87% - 3h 20m left" "['open', 'Open']" "{}" 0
sleep 1.8
notify demo 0 "" "构建完成" "cargo build --release" "[]" "{}" 0
sleep 2.4

still                                             # dismiss the top card
read -r x0 y0 x1 y1 _ < <(python3 "$HERE/shot.py" bbox "$W/still.ppm" 900 $((BAR_H + 1)) 1280 400)
glide $((x0 + 30)) $((y0 + 20)) 8
click
sleep 1.4

still                                             # click the action button of the remaining card
read -r ax0 ay0 ax1 ay1 n < <(python3 "$HERE/shot.py" near "$W/still.ppm" 900 $((BAR_H + 1)) 1280 400 136 192 208 4)
if [ "$n" -gt 0 ]; then
    echo "  action button at $ax0,$ay0-$ax1,$ay1 (ActionInvoked)"
    glide $(((ax0 + ax1) / 2)) $(((ay0 + ay1) / 2)) 8
    click
    sleep 1.4
fi

glide 300 430 12                                  # over the window: the layer region is transparent there
click
sleep 1.2

close_all
sleep 1.6
record_stop

frames=$(ls "$W/frames"/*.ppm | wc -l)
echo "frames: $frames"
python3 "$HERE/gif.py" --frames "$W/frames" --times "$W/frames/times" --out "$W/cornice-live.gif" \
    --colors 128 --scale 2 --check
ls -l "$W/cornice-live.gif"
echo "GIF: $W/cornice-live.gif"
