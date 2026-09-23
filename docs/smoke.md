# Smoke check

Each section states what it **can prove** and what it **cannot prove**. The first three sections run in the dev sandbox; section 4 can only be done by the user in the target environment.

## 1. Pure logic (`cargo test`)

```sh
cargo test
```

Covers: geometry (`Rect`/`Color`), canvas (rounded corners / blending / out of bounds), text truncation, config parsing (errors carry line numbers), bar layout (including the notification module's reserved width), the `clock`/`exec` modules, easing and tweening, the notification queue state machine, and the notification column — its geometry, the stretch, hit testing and that it paints no background inside the bar.

- **Proves**: pure logic has no regressions.
- **Does not prove**: any pixels, any protocol behaviour.

## 2. D-Bus (runs inside the sandbox)

```sh
export XDG_RUNTIME_DIR=/tmp/cornice-xdg; rm -rf $XDG_RUNTIME_DIR; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /dev/null >/tmp/sway.log 2>&1 &
sleep 3
dbus-run-session -- sh -c '
  WAYLAND_DISPLAY=wayland-1 ./target/debug/cornice >/tmp/cornice.log 2>&1 &
  sleep 1
  gdbus call --session --dest org.freedesktop.Notifications \
    --object-path /org/freedesktop/Notifications \
    --method org.freedesktop.Notifications.GetCapabilities
  gdbus call --session --dest org.freedesktop.Notifications \
    --object-path /org/freedesktop/Notifications \
    --method org.freedesktop.Notifications.Notify \
    test 0 "" "title" "body" "[]" "{}" -1
  notify-send "hi" "body"
  kill %1
'
```

- **Proves**: the `org.freedesktop.Notifications` name is owned, the `GetCapabilities`/`Notify` contract, that `notify-send` works, and signals (capture them with `dbus-monitor`).
- **Does not prove**: that notifications have any pixels, that cards animate, or that clicks work.

## 3. headless sway (runs inside the sandbox, **but is not the target environment**)

```sh
export XDG_RUNTIME_DIR=/tmp/cornice-xdg; rm -rf $XDG_RUNTIME_DIR; mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /dev/null >/tmp/sway.log 2>&1 &
sleep 3
WAYLAND_DISPLAY=wayland-1 timeout 5 ./target/debug/cornice; echo "exit=$?"
```

- **Proves**: cornice attaches to layer-shell, `bar configured: 1280x30`, and the notification surface maps without panicking under a real notification sequence.
- **Does not prove**: **sway uses wlroots' native layer-shell; river goes through the WM-forwarded `river-layer-shell-v1`, so behaviour is not guaranteed to match.** The sandbox has no screenshot tool, so "renders correctly" and "the animation feels right" cannot be verified here — only "it starts, the surface maps, it does not panic".

## 4. river + tailrace (`scripts/live/run.sh`)

The target stack, headless: real `river`, real `tailrace` as the WM, real `cornice` — one shell script, nothing
mocked. It starts `WLR_BACKENDS=headless` river with tailrace (the compositor and the WM are read-only inputs,
never modified), runs cornice under a private session bus, and then checks pixels and input:

* **pixels** from `grim` (river's `wlr-screencopy`), analysed by `scripts/live/shot.py` (stdlib Python, no PIL)
* **a window** from `scripts/live/probe.c win` — a coloured `xdg_toplevel` that logs pointer events — to prove the
  exclusive zone and click-through
* **a pointer** from `scripts/live/probe.c vp` — `zwlr_virtual_pointer_v1` (river advertises it), `at X Y` warps and
  clicks like a real mouse
* **notifications** from `gdbus` on the private bus, with `dbus-monitor` capturing `NotificationClosed`/`ActionInvoked`

```sh
cargo build && (cd ../tailrace && cargo build --release)
bash scripts/live/run.sh          # 52 checks, exit code = failures
python3 scripts/live/artifacts.py # rebuild testing/*.png from the run's screenshots
```

Environment overrides: `RIVER_BIN`, `TAILRACE_BIN`, `CORNICE_BIN`, `W` (scratch dir, default `/tmp/cornice-live`);
logs, screenshots and the test config stay under `W`. Needs `grim`, `gcc`, `wayland-scanner`, `dbus-run-session`,
`gdbus`. It kills only the PIDs it started (a `pkill river` would kill a real session).

Checks: bar band and height, left/center/right placement and the optical inset at the bar's rounded ends,
a window lands exactly at `bar height + vertical_gap` (exclusive zone reached the WM), that a short card is drawn
*inside* the bar while a long body stretches the same background below it (one unbroken column from the bar's row
down, attaching where the pill's bottom edge is straight), that a second card keeps stretching the same shape,
`expire_timeout` / critical / `CloseNotification`, expiry emits `NotificationClosed` reason 1, a replace stays put
while a new id is still stretching (measured), `max_visible` stacking, card click → reason 2, action button →
`ActionInvoked`, click-through on a transparent region, idle CPU, config errors with line numbers, and the
two-output behaviour.

- **Proves**: the whole checklist below except the two items marked *(human)* — on river + tailrace, with pixels and pointer input.
- **Does not prove**: that the animation *feels* right (the harness measures rects, it cannot judge easing), anything
  about real hardware (libinput devices, GPU, multi-monitor geometry), and the multi-output check only asserts
  "exactly one output" (see the compromise below).

### Manual pass on the real machine

- [ ] *(human)* The stretch reads as the bar's own material pulling downwards — to watch it slowly, set
      `[notification] enter_ms = 60000` and take a screenshot every second: the column's top edge stays at the bar's
      top while its bottom edge walks down, and at no frame is there a lighter or darker band where the two meet
- [ ] *(human)* `notify-send -r <id>` on a visible card updates it in place, without replaying the stretch
- [ ] *(human)* The bar's text sits comfortably inside the pill: float it against a screenshot and mirror the image,
      the left and right gaps should read the same (`docs/smoke.md`)
- [ ] *(human)* `notify-send -u critical ...` does not auto-dismiss; `notify-send -t 2000 ...` disappears after 2 seconds
      (the harness drives the same D-Bus API with `gdbus`; `notify-send` itself is not installed here)
- [ ] *(human)* `powertop`/`perf stat`: no sustained 60fps wakeups (the harness measures CPU time as a proxy: < 50 ticks
      per 5 idle seconds)
- [ ] *(human)* Multi-monitor with a real second output: the bar appears on both, notifications on one

### Known compromises (read first)

- **Multi-monitor**: notifications are pinned to one output — the one `ensure_notif_surface` happens to pick from
  `bars.keys().next()`, i.e. **HashMap order**, not "the first output" in any stable sense (measured: output 2 in
  one run, output 1 in the next).
  layer-shell client surfaces are **non-interactive** (keyboard focus is invisible), so there is no way to know which
  output currently has focus — hence design doc §7's "deliver to the focused output" cannot be implemented.
  Upgrade path: create one notification surface per output and show the same queue on each.
- **Target-environment gap**: passing section 3 on sway does not mean passing on river + tailrace (river's layer-shell is forwarded by the WM).
