#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Live test: cornice on river (river-paddd) + tailrace, headless, real pixels and real pointer input.
#
#   scripts/live/run.sh              # uses ~/.local/bin/river and ../tailrace/target/release/tailrace
#   RIVER_BIN=... TAILRACE_BIN=... scripts/live/run.sh
#
# This is docs/smoke.md section 4 turned into a check. river and tailrace are read-only inputs here:
# nothing under their trees is written, and every process is killed by recorded pid (never `pkill river`,
# another river may belong to a different session).
#
# Reads pixels with grim (river's wlr-screencopy), drives the pointer through zwlr_virtual_pointer_v1
# (probe.c), and speaks the notification D-Bus API with gdbus inside a private session bus.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
W=${W:-/tmp/cornice-live}
RIVER_BIN=${RIVER_BIN:-$HOME/.local/bin/river}
TAILRACE_BIN=${TAILRACE_BIN:-$ROOT/../tailrace/target/release/tailrace}
CORNICE_BIN=${CORNICE_BIN:-$ROOT/target/debug/cornice}
PROBE=$W/build/probe
OUT_W=1280
OUT_H=720
BAR_H=30
GAP=9

pass=0; fail=0; skip=0
ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail+1)); }
note() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; skip=$((skip+1)); }
sec()  { printf '\n== %s\n' "$1"; }

# `want actual` as positional equality
eq()   { if [ "$1" = "$2" ]; then ok "$3 ($2)"; else bad "$3: want $1, got $2"; fi; }
ge()   { if [ "$2" -ge "$1" ] 2>/dev/null; then ok "$3 ($2 >= $1)"; else bad "$3: want >= $1, got $2"; fi; }
le()   { if [ "$2" -le "$1" ] 2>/dev/null; then ok "$3 ($2 <= $1)"; else bad "$3: want <= $1, got $2"; fi; }

shot() { grim -t ppm "$W/$1.ppm" 2>"$W/grim.err" || bad "grim failed: $(cat "$W/grim.err")"; }
px()   { python3 "$HERE/shot.py" "$@"; }
# bbox: "x0 y0 x1 y1 count" of pixels differing from black inside a region
region() { px bbox "$1" "$2" "$3" "$4" "$5"; }
card_region() { region "$1" 900 30 1280 300; }
alive() { kill -0 "$1" 2>/dev/null; }

stop_tree() { # kill the session started by `setsid X &` (negative pid = process group)
    [ -n "${1:-}" ] || return 0
    kill -TERM -- "-$1" 2>/dev/null || kill -TERM "$1" 2>/dev/null
    return 0
}

cleanup() {
    stop_tree "${cornice_bg:-}"
    stop_tree "${win_bg:-}"
    stop_tree "${vp_bg:-}"
    stop_tree "${river_bg:-}"
    exec 9>&- 2>/dev/null
}
trap cleanup EXIT

# ---------------------------------------------------------------- setup

[ -x "$RIVER_BIN" ]    || { echo "river not found: $RIVER_BIN (build with: zig build --prefix ~/.local install)"; exit 2; }
[ -x "$TAILRACE_BIN" ] || { echo "tailrace not found: $TAILRACE_BIN (build with: cargo build --release)"; exit 2; }
[ -x "$CORNICE_BIN" ]  || { echo "cornice not found: $CORNICE_BIN (build with: cargo build)"; exit 2; }

mkdir -p "$W/build"
if [ ! -x "$PROBE" ] || [ "$HERE/probe.c" -nt "$PROBE" ]; then
    rm -rf "$W/build"; mkdir -p "$W/build"
    wayland-scanner client-header /usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml "$W/build/xdg-shell.h"
    wayland-scanner private-code  /usr/share/wayland-protocols/stable/xdg-shell/xdg-shell.xml "$W/build/xdg-shell.c"
    wayland-scanner client-header "$HERE/wlr-virtual-pointer-unstable-v1.xml" "$W/build/vp.h"
    wayland-scanner private-code  "$HERE/wlr-virtual-pointer-unstable-v1.xml" "$W/build/vp.c"
    mv "$W/build/xdg-shell.h" "$W/build/xdg-shell-client-protocol.h"
    mv "$W/build/vp.h" "$W/build/wlr-virtual-pointer-unstable-v1-client-protocol.h"
    cc -o "$PROBE" "$HERE/probe.c" "$W/build/xdg-shell.c" "$W/build/vp.c" -I"$W/build" \
        $(pkg-config --cflags --libs wayland-client) || { echo "probe build failed"; exit 2; }
fi

up() { # up [n_outputs]
    local n=${1:-1}
    stop_tree "${cornice_bg:-}"
    stop_tree "${win_bg:-}"
    stop_tree "${vp_bg:-}"
    stop_tree "${river_bg:-}"
    exec 9>&- 2>/dev/null
    sleep 1
    rm -rf "$W/run" "$W/cfg"; mkdir -p "$W/run" "$W/cfg/cornice"; chmod 700 "$W/run"
    cat > "$W/cfg/cornice/config.toml" <<'EOF'
# enter_ms is deliberately long so the per-card scale/fade can be sampled by grim.
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
enter_ms = 1500
exit_ms = 400

[bar.left]
modules = [ { kind = "clock", format = "%H:%M" } ]

[bar.center]
modules = [ { kind = "exec", command = "echo mid", format = "{out}" } ]

[bar.right]
modules = [ { kind = "exec", command = "echo 42", format = "L{out}" } ]
EOF
    XDG_RUNTIME_DIR=$W/run WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=$n WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
        setsid "$RIVER_BIN" -c "$TAILRACE_BIN" -log-level debug >"$W/river.log" 2>&1 </dev/null &
    river_bg=$!
    for _ in $(seq 60); do [ -S "$W/run/wayland-1" ] && break; sleep 0.1; done
    # cornice owns a private session bus (dbus-run-session) so the test never touches the user's bus
    cat > "$W/bus.sh" <<EOF
#!/bin/sh
echo "\$DBUS_SESSION_BUS_ADDRESS" > $W/dbus.addr
stdbuf -oL dbus-monitor --session "interface='org.freedesktop.Notifications'" > $W/dbus.log 2>&1 &
exec $CORNICE_BIN
EOF
    chmod +x "$W/bus.sh"
    setsid dbus-run-session -- "$W/bus.sh" >"$W/cornice.log" 2>&1 </dev/null &
    cornice_bg=$!
    for _ in $(seq 60); do grep -q 'bar configured' "$W/cornice.log" 2>/dev/null && break; sleep 0.1; done
    rm -f "$W/vp.fifo"; mkfifo "$W/vp.fifo"
    setsid "$PROBE" vp <"$W/vp.fifo" >"$W/vp.log" 2>&1 &
    vp_bg=$!
    exec 9>"$W/vp.fifo"
}

vp() { # vp <command...>
    printf '%s\n' "$*" >&9
    sleep 0.25
}

notify() { # notify APP ID ICON SUMMARY BODY ACTIONS HINTS TIMEOUT -> id
    DBUS_SESSION_BUS_ADDRESS=$(cat "$W/dbus.addr") gdbus call --session \
        --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify -- "$@" | sed -n 's/.*uint32 \([0-9]\+\).*/\1/p'
}

close_id() {
    DBUS_SESSION_BUS_ADDRESS=$(cat "$W/dbus.addr") gdbus call --session \
        --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.CloseNotification -- "$1" >/dev/null 2>&1
}

close_all() { local i; for i in $(seq 1 40); do close_id "$i"; done; sleep 0.5; }

# notifications signals seen so far, as "id reason" lines
closed_pairs() {
    awk '/member=NotificationClosed/ {getline a; getline b; sub(/.*uint32 /,"",a); sub(/.*uint32 /,"",b); print a, b}' \
        "$W/dbus.log" 2>/dev/null
}

export XDG_RUNTIME_DIR=$W/run
export WAYLAND_DISPLAY=wayland-1
export XDG_CONFIG_HOME=$W/cfg

sec "session: river + tailrace"
echo "  river    $RIVER_BIN"
echo "  tailrace $TAILRACE_BIN"
echo "  cornice  $CORNICE_BIN"
up 1
if ! alive "$river_bg" || ! alive "$cornice_bg"; then
    bad "river/cornice died during startup"
    tail -20 "$W/river.log"; exit 1
fi
ok "river + tailrace up (headless ${OUT_W}x${OUT_H})"
if grep -q 'bar configured: 1280x30' "$W/cornice.log"; then
    ok "cornice attached to river-layer-shell: $(head -1 "$W/cornice.log")"
else
    bad "no 'bar configured: 1280x30' in cornice.log: $(cat "$W/cornice.log")"
fi
if grep -q 'does not exist' "$W/cornice.log"; then
    bad "cornice did not read the test config"
else
    ok "cornice read $XDG_CONFIG_HOME/cornice/config.toml"
fi

sec "bar geometry and sections"
sleep 1
shot bar
read -r bx0 by0 bx1 by1 bn < <(region "$W/bar.ppm" 0 0 "$OUT_W" 60)
eq 0   "$bx0" "bar starts at the left edge"
eq 0   "$by0" "bar starts at the top edge"
eq 1279 "$bx1" "bar spans the full width"
eq $((BAR_H - 1)) "$by1" "bar is $BAR_H px tall (matches [bar] height)"
eq "24,24,24" "$(px px "$W/bar.ppm" 5 5 | tr ' ' ',')" "bar background is #1a1a1a over black"
eq "0,0,0"    "$(px px "$W/bar.ppm" 5 40 | tr ' ' ',')" "below the bar is the compositor background"
read -r _ _ _ _ lcount < <(px near "$W/bar.ppm" 0 0 300 "$BAR_H" 220 220 220 60)
read -r _ _ _ _ rcount < <(px near "$W/bar.ppm" 900 0 "$OUT_W" "$BAR_H" 220 220 220 60)
ge 1 "$lcount" "left section draws text (clock)"
ge 1 "$rcount" "right section draws text (exec)"
# Optical inset at the pill's rounded ends: padding (8) + the corner keyline r(1-1/√2) = 4 at radius 15,
# so the ink must clear x = 12 by its own side bearing and both ends must match.
read -r lx0 _ _ _ _ < <(px near "$W/bar.ppm" 0 0 300 "$BAR_H" 220 220 220 60)
read -r _ _ rx1 _ _ < <(px near "$W/bar.ppm" 900 0 "$OUT_W" "$BAR_H" 220 220 220 60)
ge 12 "$lx0" "left text clears the pill's rounded end (ink starts at $lx0)"
le 15 "$lx0" "...without drifting away from it"
ge $((OUT_W - 16)) "$rx1" "right text keeps the mirrored inset (ink ends at $rx1)"
le $((OUT_W - 13)) "$rx1" "...without drifting away from it"
read -r mx0 _ mx1 _ mcount < <(px near "$W/bar.ppm" 500 0 780 "$BAR_H" 220 220 220 60)
if [ "$mcount" -gt 0 ]; then
    mid=$(( (mx0 + mx1) / 2 ))
    if [ "$mid" -ge $((OUT_W / 2 - 30)) ] && [ "$mid" -le $((OUT_W / 2 + 30)) ]; then
        ok "center section is centred (text centre $mid, output centre $((OUT_W / 2)))"
    else
        bad "center section is not centred: text centre $mid, want $((OUT_W / 2)) ± 30"
    fi
else
    bad "no center section text found"
fi

sec "exclusive zone: a window is pushed below the bar"
setsid env PROBE_COLOR=00ff00 "$PROBE" win >"$W/win.log" 2>&1 </dev/null &
win_bg=$!
sleep 1.5
shot win
read -r wx0 wy0 wx1 wy1 wcount < <(region "$W/win.ppm" 0 $((BAR_H + 1)) "$OUT_W" "$OUT_H")
if grep -q 'win: mapped' "$W/win.log"; then ok "probe toplevel mapped"; else bad "probe toplevel never mapped"; fi
eq $GAP "$wx0" "window left edge == tailrace horizontal_gap ($GAP)"
eq $((BAR_H + GAP)) "$wy0" "window top edge == bar height + gap (exclusive zone reached the WM)"
ge 100000 "$wcount" "window is actually rendered"

sec "notifications: independent cards below the bar"
# The probe toplevel from the section above fills everything below the bar with green, so stop it before
# measuring the translucent card material. A fresh toplevel is started in the click-through section.
stop_tree "${win_bg:-}"; win_bg=
bands() { px bands "$W/$1.ppm" "$((OUT_W - 20))" $((BAR_H + 1)) "$OUT_H"; }
close_all
read -r _ _ _ _ bar_ink < <(px near "$W/bar.ppm" 900 0 "$OUT_W" "$BAR_H" 220 220 220 60)

n1=$(notify cornice-test 0 "" "Short summary" "" "[]" "{}" 0)
sleep 1.8
shot short
read -r short_x0 short_y0 short_x1 short_y1 short_count < <(card_region "$W/short.ppm")
ge 1 "$short_count" "a one-line notification creates a card"
eq $((OUT_W - 12 - 300)) "$short_x0" "right-anchored card starts at the derived 2*card_gap inset"
eq $((OUT_W - 13)) "$short_x1" "…and ends one pixel before its fixed-width edge"
eq $((BAR_H + 6)) "$short_y0" "the card starts one card_gap below the bar"
eq $((short_y0 + 74)) "$short_y1" "a short card uses the derived 2.5*height minimum"
read -r _ _ _ _ gap_above < <(region "$W/short.ppm" 900 "$BAR_H" "$OUT_W" "$short_y0")
eq 0 "$gap_above" "the independent card does not touch or merge with the bar"
read -r _ _ _ _ short_bar_ink < <(px near "$W/short.ppm" 900 0 "$OUT_W" "$BAR_H" 220 220 220 60)
eq "$bar_ink" "$short_bar_ink" "notification arrival does not change the bar"
close_all

n2=$(notify cornice-test 0 "" "Long" $'one\ntwo\nthree' "[]" "{}" 0)
sleep 1.8
shot long
read -r long_x0 _ long_x1 long_y1 long_count < <(card_region "$W/long.ppm")
ge 1 "$long_count" "a body grows the same independent card"
eq "$short_x0" "$long_x0" "short and long cards keep the same fixed-width left edge"
eq "$short_x1" "$long_x1" "short and long cards keep the same fixed-width right edge"
ge $((short_y1 + 1)) "$long_y1" "multi-line content increases card height"

n3=$(notify cornice-test 0 "" "Second" "also here" "[]" "{}" 0)
sleep 1.8
shot two
read -r _ _ _ two_y1 _ < <(card_region "$W/two.ppm")
ge "$long_y1" "$two_y1" "the second independent card extends the column"
eq 2 "$(bands two)" "two notifications render as two separate shapes"
second_top=$((short_y0 + 75 + 6))
read -r _ _ _ _ inter_gap < <(region "$W/two.ppm" 900 $((second_top - 3)) "$OUT_W" $((second_top - 1)))
eq 0 "$inter_gap" "independent cards keep one card_gap between them"

n4=$(notify cornice-test 0 "" "Third" "also here" "[]" "{}" 0)
sleep 1.8
shot three
read -r _ _ _ three_y1 _ < <(card_region "$W/three.ppm")
ge "$two_y1" "$three_y1" "a third notification gets its own row"
eq 3 "$(bands three)" "three notifications render as three separate shapes"
close_all
shot nt0
eq 0 "$(bands nt0)" "CloseNotification removes every card"

n4=$(notify cornice-test 0 "" "Fades" "gone in 1.5s" "[]" "{}" 1500)
sleep 0.6
shot nt2a
eq 1 "$(bands nt2a)" "expire_timeout=1500 card is visible at first"
sleep 1.6
shot nt2b
eq 0 "$(bands nt2b)" "…and is gone 2.2 s later"
eq 1 "$(closed_pairs | grep -c "^$n4 1$")" "expiry emits NotificationClosed(id=$n4, reason=1)"

n5=$(notify cornice-test 0 "" "Critical" "stays" "[]" "{'urgency': <byte 2>}" -1)
sleep 2.5
shot nt3
eq 1 "$(bands nt3)" "critical urgency does not auto-dismiss (default timeout)"
close_all

sec "replace in place keeps the same notification surface"
body5=$'a\nb\nc\nd\ne'
n6=$(notify cornice-test 0 "" "Replace" "$body5" "[]" "{}" 0)
sleep 1.8
n7=$(notify cornice-test 0 "" "Fresh" "$body5" "[]" "{}" 0)
sleep 1.8
shot fresh_done
read -r _ _ _ fresh_y1 _ < <(card_region "$W/fresh_done.ppm")
eq 2 "$(bands fresh_done)" "two notification surfaces are visible"

notify cornice-test "$n7" "" "Fresh" $'v\nw\nx\ny\nz' "[]" "{}" 0 >/dev/null
sleep 0.5
shot replaced
read -r _ _ _ replaced_y1 replaced_count < <(card_region "$W/replaced.ppm")
ge 1 "$replaced_count" "replaces_id updates the existing card in place"
ge "$fresh_y1" "$replaced_y1" "the replacement remains at the settled column position"
eq 0 "$(closed_pairs | grep -c "^$n7 ")" "replaces_id emits no NotificationClosed"
close_all

sec "queue: max_visible cards, then promotion"
close_all
notify cornice-test 0 "" "Card 1" "body 1" "[]" "{}" 0 >/dev/null
sleep 1.8
shot q1
read -r _ _ _ q1y _ < <(card_region "$W/q1.ppm")
eq 1 "$(bands q1)" "one queued notification renders one card"

notify cornice-test 0 "" "Card 2" "body 2" "[]" "{}" 0 >/dev/null
sleep 1.8
shot q2
read -r _ _ _ q2y _ < <(card_region "$W/q2.ppm")
ge "$q1y" "$q2y" "a second notification extends the independent column"
eq 2 "$(bands q2)" "both notifications are separate cards"

ids=""
for i in 3 4 5 6; do
    ids="$ids $(notify cornice-test 0 "" "Card $i" "body $i" "[]" "{}" 0)"
    sleep 0.15
done
sleep 1.8
shot stack
read -r _ _ _ stack_y1 _ < <(card_region "$W/stack.ppm")
ge "$q2y" "$stack_y1" "six queued notifications use the full visible column"
eq 4 "$(bands stack)" "only max_visible=4 cards are drawn"

newest=$(echo $ids | awk '{print $NF}')
close_id "$newest"
sleep 0.8
shot stack2
eq 3 "$(bands stack2)" "closing the newest card leaves three independent cards"
if closed_pairs | grep -q "^$newest 3$"; then ok "CloseNotification emits reason=3 for $newest"; else bad "no reason=3 for $newest"; fi

oldest=$(echo $ids | awk '{print $1}')
close_id "$oldest"
sleep 0.8
shot stack3
eq 4 "$(bands stack3)" "closing a visible card promotes the next queued notification"
if closed_pairs | grep -q "^$oldest 3$"; then ok "the promoted queue also preserves close reason=3"; else bad "no reason=3 for $oldest"; fi
close_all

sec "pointer: card click, action button, click-through"
close_all
na=$(notify cornice-test 0 "" "Clickable" "click me" "[]" "{}" 0)
sleep 1.8
shot click
read -r cx0 cy0 cx1 cy1 _ < <(card_region "$W/click.ppm")
vp at $((cx0 + 20)) $((cy0 + 8))
vp click left
sleep 0.8
if closed_pairs | grep -q "^$na 2$"; then
    ok "clicking the card emits NotificationClosed(id=$na, reason=2)"
else
    bad "clicking the card did not emit NotificationClosed(2) for id=$na, got: $(closed_pairs | tail -3 | tr '\n' ' ')"
fi
shot click2
read -r _ _ _ _ after < <(card_region "$W/click2.ppm")
eq 0 "$after" "the clicked card is gone from the pixels"

nb=$(notify cornice-test 0 "" "Actionable" "has a button" "['open', 'Open']" "{}" 0)
sleep 1.8
shot action
read -r ax0 ay0 ax1 ay1 acount < <(px near "$W/action.ppm" 900 30 "$OUT_W" 300 136 192 208 4)
if [ "$acount" -gt 0 ]; then
    ok "the action button is drawn in the accent colour at $ax0,$ay0"
    vp at $(( (ax0 + ax1) / 2 )) $(( (ay0 + ay1) / 2 ))
    vp click left
    sleep 0.8
    if grep -q 'member=ActionInvoked' "$W/dbus.log" && grep -A2 'member=ActionInvoked' "$W/dbus.log" | grep -q 'open'; then
        ok "clicking the button emits ActionInvoked(id=$nb, key=open)"
    else
        bad "no ActionInvoked(open); id=$nb, button rect $ax0,$ay0-$ax1,$ay1, closed so far: $(closed_pairs | tr '\n' ' ')"
    fi
else
    bad "no accent-coloured action button found in the card"
fi

nc=$(notify cornice-test 0 "" "Passthrough" "transparent around me" "[]" "{}" 0)
sleep 1.8
# A fresh toplevel: the one from the exclusive-zone section was stopped so the card could be measured.
setsid env PROBE_COLOR=00ff00 "$PROBE" win >"$W/win.log" 2>&1 </dev/null &
win_bg=$!
sleep 1.5
p0=$(grep -c 'win: pointer button' "$W/win.log")
# (100,100) is outside the right-anchored card surface and inside the probe toplevel, so the click must
# reach the toplevel rather than an invisible full-width notification region.
vp at 100 100
vp click left
sleep 0.5
shot through
read -r _ _ _ _ still < <(card_region "$W/through.ppm")
ge 1 "$still" "the card survives a click on a transparent region"
if [ "$(grep -c 'win: pointer button' "$W/win.log")" -gt "$p0" ]; then
    ok "click-through: the toplevel under the transparent region got the button event"
else
    bad "the toplevel never saw the click (cornice consumed it?)"
fi
if closed_pairs | grep -q "^$nc "; then bad "the passthrough click closed the card"; else ok "the passthrough click did not close the card"; fi
close_all

sec "idle"
sleep 1
cpid=$(pgrep -f "$CORNICE_BIN" | head -1)
idle_cpu() { awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null; }
c0=$(idle_cpu "$cpid")
sleep 5
c1=$(idle_cpu "$cpid")
ticks=$(( c1 - c0 ))
echo "  (cornice used $ticks ticks over 5 idle seconds = $(( ticks * 10 )) ms of CPU)"
le 50 "$ticks" "idle CPU stays low (no 60fps spin)"

sec "config errors carry a line number"
mkdir -p "$W/badcfg/cornice"
printf '[bar]\nheight = 30\n\n[bar.left]\nmodules = [ { kind = "river.tags" } ]\n' > "$W/badcfg/cornice/config.toml"
out=$(XDG_CONFIG_HOME=$W/badcfg "$CORNICE_BIN" 2>&1); rc=$?
if [ "$rc" -ne 0 ] && echo "$out" | grep -q 'line 5'; then
    ok "unknown module rejected at line 5"
else
    bad "unknown module not rejected with a line number (rc=$rc): $out"
fi
printf '[bar]\nheight = 30\n\n[theme]\nbackground = "#zzzzzz"\n' > "$W/badcfg/cornice/config.toml"
out=$(XDG_CONFIG_HOME=$W/badcfg "$CORNICE_BIN" 2>&1); rc=$?
if [ "$rc" -ne 0 ] && echo "$out" | grep -qE ': [0-9]+:[0-9]+: theme.background:'; then
    ok "bad colour rejected as $(echo "$out" | head -1 | cut -c1-60)"
else
    bad "bad colour not rejected with line:col (rc=$rc): $out"
fi

sec "two outputs: notifications follow the first output with a bar"
up 2
if ! alive "$cornice_bg"; then
    bad "cornice died with two outputs"; tail -5 "$W/cornice.log"
else
    sleep 1
    grim -t ppm -o HEADLESS-1 "$W/mo1.ppm" && grim -t ppm -o HEADLESS-2 "$W/mo2.ppm"
    notify cornice-test 0 "" "Multi" "output" "[]" "{}" 0 >/dev/null
    sleep 1.8
    grim -t ppm -o HEADLESS-1 "$W/mo1n.ppm" && grim -t ppm -o HEADLESS-2 "$W/mo2n.ppm"
    read -r _ _ _ _ b1 < <(region "$W/mo1.ppm" 0 0 "$OUT_W" "$BAR_H")
    read -r _ _ _ _ b2 < <(region "$W/mo2.ppm" 0 0 "$OUT_W" "$BAR_H")
    ge 1 "$b1" "output 1 has a bar"
    ge 1 "$b2" "output 2 has a bar"
    read -r _ _ _ _ c1 < <(card_region "$W/mo1n.ppm")
    read -r _ _ _ _ c2 < <(card_region "$W/mo2n.ppm")
    if { [ "$c1" -gt 0 ] && [ "$c2" -eq 0 ]; } || { [ "$c1" -eq 0 ] && [ "$c2" -gt 0 ]; }; then
        ok "the card lands on exactly one output (output $([ "$c1" -gt 0 ] && echo 1 || echo 2)), the documented compromise"
    else
        bad "the card must land on exactly one output, got output 1: $c1 px, output 2: $c2 px"
    fi
fi

printf '\n%d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
[ "$fail" -eq 0 ]
