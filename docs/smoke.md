# Smoke check

The automated gate is pure logic only. Protocol and pixel behaviour must be checked manually in the target environment.

## 1. Pure logic

```sh
cargo test
```

Covers:

- geometry, continuous canvas corners, alpha blending and text clipping;
- bar layout without a notification slot;
- `[notification]` optional enablement, `left | center | right`, and line-numbered config errors;
- independent card widths, content heights, vertical order/gaps, detail rhythm and untrusted action bounds;
- `applications` grouping by `app_id`, monogram derivation, chip width and count badge, and the focused-app marking;
- module actions (command parsing, empty = absent) and `bar.outputs`; `exec.interval` range errors at `line:column`;
- `fit_text` truncation width/precision and the single-pass cut;
- the idle-timer deadline (`next_wake`) and repaint coalescing (`coalesce`);
- per-card enter/exit/reflow motion endpoints and independence;
- queue replacement, visibility, capacity, timeout and close semantics;
- D-Bus-independent notification state transitions.

- **Proves**: pure logic has no regressions.
- **Does not prove**: layer-shell placement, pixels, pointer input or animation feel.

Section 4 below covers the control plane in a headless sway session; the rest of this file is the manual pass
for what only a real session can show.

## 2. D-Bus in the sandbox

```sh
export XDG_RUNTIME_DIR=/tmp/cornice-xdg
rm -rf "$XDG_RUNTIME_DIR"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /dev/null >/tmp/sway.log 2>&1 &
sleep 3

dbus-run-session -- sh -c '
  WAYLAND_DISPLAY=wayland-1 ./target/debug/cornice >/tmp/cornice.log 2>&1 &
  sleep 1
  gdbus call --session --dest org.freedesktop.Notifications \
    --object-path /org/freedesktop/Notifications \
    --method org.freedesktop.Notifications.GetCapabilities
  id=$(gdbus call --session --dest org.freedesktop.Notifications \
    --object-path /org/freedesktop/Notifications \
    --method org.freedesktop.Notifications.Notify \
    -- test 0 "" "title" "body" "[]" "{}" -1 | sed -e "s/.*uint32 //" -e "s/,.*//")
  ./target/debug/cornice notification dismiss "$id"
  ./target/debug/cornice notification dismiss --all
  sleep 1
  kill %1
'
```

Expected: `dismiss <id>` emits `NotificationClosed(id, 3)`; `dismiss --all` emits `NotificationClosed(id, 3)` for every queued id, visible or hidden; both exit 0, and an unknown subcommand or a non-numeric id exits 2 (no arguments still starts the daemon).

- **Proves**: the daemon owns `org.freedesktop.Notifications`, accepts `notify-send`-compatible requests, does not crash while creating card surfaces, and `notification dismiss` reaches the same daemon as a separate process.
- **Does not prove**: correct position, pixels, independent animations or clicks.

## 3. Headless sway startup

```sh
export XDG_RUNTIME_DIR=/tmp/cornice-xdg
rm -rf "$XDG_RUNTIME_DIR"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /dev/null >/tmp/sway.log 2>&1 &
sleep 3
WAYLAND_DISPLAY=wayland-1 timeout 5 ./target/debug/cornice
```

Expected: the bar configures as `1280x30`; sending notifications produces a card surface per visible card and no protocol error before the timeout (the configure log lines were removed, so watch for absence of errors and for the notification appearing in a screenshot).

- **Proves**: attachment to wlroots layer-shell, first-configure ordering and basic per-card surface mapping.
- **Does not prove**: target river behaviour or visual correctness.

## 4. Headless control-plane checks

These run in the sandbox (sway headless + a private session bus) and give real pass/fail answers for the bar
control surface, the id rule and the wake policy. They say nothing about pixels, motion or river.

```sh
export XDG_RUNTIME_DIR=/tmp/cornice-xdg XDG_CONFIG_HOME=/tmp/cornice-cfg
export SWAYSOCK=$XDG_RUNTIME_DIR/sway-ipc.0.sock
```

Start `sway` headless with `WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1` and run
everything below in one `dbus-run-session`, with `WAYLAND_DISPLAY=wayland-1`.

### 4.1 D-Bus, ids and the CLI

```sh
B=./target/debug/cornice
$B &                                        # the daemon
N() { gdbus call --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify -- "app$1" "$2" "" "s$1" body "[]" "{}" "$3" | sed -e 's/.*uint32 //' -e 's/,.*//'; }
N 1 0 -1      # -> 1
N 1 1 -1      # -> 1    a replacement keeps the handle the client already has
N 2 3 -1      # -> a fresh id (here 2): an invented replaces_id must not capture a later allocation
N 3 0 -1      # -> 3    distinct from the invented id
N 5 77 -1     # -> a fresh id, not 77: a stale handle from a previous run is a new notification
$B notification dismiss 1; $B bar hide; $B bar show; $B bar toggle
$B bar nonsense; echo $?   # 2, with usage on stderr
```

Expected: no id is ever repeated; every command above exits 0 except the usage error (2); the daemon is still
alive at the end; `cornice.log` is empty.

### 4.2 `bar.outputs` and bar show/hide, observed through the exclusive zone

With no bar at all, sway's workspace rect starts at `y = 0`; a 30 px bar with its exclusive zone pushes it to
`y = 30`. That offset is the observable proof that a bar exists on this output:

```sh
workspace_y() { swaymsg -t get_workspaces -r | python3 -c 'import json,sys; w=json.load(sys.stdin); print([x["rect"]["y"] for x in w if x["focused"]][0] if w else "?")'; }
# config `outputs = ["<the real output name>"]` -> 30;  `["NOPE-9"]` -> 0;  `[]` -> 30
$B bar hide    # -> 0   (the zone is released)
$B bar show    # -> 30  (and restored)
```

### 4.3 Idle wake policy

`/proc/<pid>/status` `voluntary_ctxt_switches` over five idle seconds:

- bar with no `clock` module and no pending expiry → **0** (nothing is armed);
- bar with a `clock` module → about 9 (the per-second tick plus its redraw).

### 4.4 Pixels, with `grim` and `foot` (both in the sandbox)

`grim -t ppm` captures the headless output, and a PPM is readable with twenty lines of Python, so geometry and
colour can be checked without a real session. Config: `height = 30`, `#1a1a1aee` / `#dcdcdc` / `#88c0d0`,
`[notification] position = "right"`, `[bar.right] applications`.

Bar and notification card (`grim -t ppm out.ppm` with no client windows, one notification):

- bar occupies y 0..29 across the full width, and its four corner pixels are the bar colour — square corners,
  nothing below y 30 unless a card exists;
- the card's box is x 968..1267 (**w 300** = `clamp(10 × 30, 80, 420)`), 12 px from the right edge
  (`2 × card_gap`), top at y 36 (`height + card_gap`) — measured h **98** = `cursor + 2 × card_padding` for a
  two-line body plus an action row;
- all four card corners are background (the continuous curve), all four edge midpoints are card colour;
- ink rows: 55–62 headline, 69–70 the full-width divider hairline, 76–96 two body lines, 104–118 the action
  pill; and with `['default','Open','dismiss','Dismiss']` exactly **one** pill exists (983..1036) — `default`
  takes no pill and no icon,
- the title is `#dcdcdc`; a normal-urgency notification shows no warning red.

Focus highlight (`grim -g "1220,0 60x30"` with one `foot -a code` and one `foot -a foot`):

- both chips are 18 px tall at y 6..23 (`app_icon`, centred in 30) with a 6 px gap and 8 px of bar padding;
- with `code` focused the left chip reads `rgb(53,68,72)` (accent over the bar) and its monogram is
  `rgb(131,185,200)`; the right chip is `rgb(59,59,59)` (neutral);
- after `swaymsg '[app_id="foot"] focus'` the two are swapped — the highlight follows the focused `app_id`;
- a second `foot -a code` window adds accent-tinted pixels at y ≥ 20 in that chip's bottom-right corner: the
  window-count badge, drawn only when the count exceeds one.

- **Proves**: the control surface answers and mutates the bar, the id rule holds end to end, timers are armed only
  when something needs them, the derived proportions and the focus highlight match the design in real pixels.
- **Does not prove**: layer-shell placement on river, pointer input or animation feel.

## 5. Manual river + tailrace pass

Use a real river + tailrace session and the target output. Set a longer duration when inspecting motion:

```toml
[notification]
position = "right" # repeat with left and center
max_visible = 4
enter_ms = 600
exit_ms = 400
```

- [ ] The bar is a full-width square-cornered rectangle like swaybar / i3bar; its four corners contain background with no capsule cutout. With `background_transparency = 0`, `50`, and `100`, only the bar background fades; at 100 the text remains visible and notification cards keep their normal opacity. It contains only its configured `clock` / `exec` modules and has no reserved notification gap.
- [ ] The first card starts below `bar.margin + bar.height + card_gap`; it neither overlaps nor shares material with the bar.
- [ ] A one-line card has the derived minimum height; body/actions grow the same card without changing its fixed width.
- [ ] The headline draws a short vertical hairline between title and source, centred on the text cap; it disappears when `app_name` is empty and neither text touches it.
- [ ] All notification-card corners read as continuous/squircle curves. The translucent material does not show a doubled overlap, black seam or square notch.
- [ ] `left`, `center`, and `right` anchor the whole vertical column correctly. Side cards keep `2 × card_gap` from the screen edge; the centre card is truly centred.
- [ ] Three simultaneous notifications produce three independent cards in new → old order with one `card_gap` between them. No card is collapsed into a peek pill.
- [ ] Each card enters with its own scale/fade spring; older cards spring to new vertical positions. Replacing an id updates in place without replaying enter.
- [ ] Closing one card fades/scales it out while the remaining cards spring upward. The exiting card cannot be clicked.
- [ ] Left-clicking an action emits `ActionInvoked` before closing; clicking elsewhere on that card emits `NotificationClosed(..., 2)`. Middle-click also closes the card body.
- [ ] Transparent pixels inside and around each surface pass clicks through to the window below; removing `[notification]` creates no card surface while the D-Bus daemon still runs.
- [ ] `{ kind = "applications" }` in the right, centre, or left list: each open app shows one square monogram chip; an app with three windows shows `3` at the chip's bottom-right, and an app with one window shows no badge. Opening/closing a window updates the chips and reflows the section.
- [ ] The bar stays healthy when `ext_foreign_toplevel_list_v1` is absent: no crash, no reserved width, notification cards still work.
- [ ] No buffer is attached before the first configure; the compositor log has no protocol error.
- [ ] `background_transparency = -1` and `101` both fail with `line:column`; values `0`, `50`, and `100` are accepted.
- [ ] Sending more than `max_visible` keeps the excess queued. Closing a visible card promotes the next notification with an enter animation.
- [ ] Debug restart: bind a WM key to `pkill -USR1 -x cornice` (river: `riverctl map normal Super+Shift R spawn 'pkill -USR1 -x cornice'`). One press rebuilds the bar and notification cards from a fresh process, re-reads the config, keeps the same PID, and does not report `org.freedesktop.Notifications is already taken`.
- [ ] With a second output focused, each newly created card appears on that output. Removing the output rebuilds live cards on the remaining/default output.
- [ ] Idle CPU does not show sustained 60 fps wakeups after all enter/exit/reflow motion has completed. With no `clock` module and no pending notification, `strace -c -p $(pidof cornice)` shows no per-second timer wakeups at all.
- [ ] With an `exec` module printing a line every few milliseconds (`while true; do date +%s%N; done`), the bar updates at most about 20 times per second and CPU stays low — the coalescing, not the printer, sets the rate.
- [ ] `{ kind = "clock", on_click = "..." }`: a left click on the clock runs the command; clicking the empty bar next to it does nothing and does not steal the click from a window below. Scroll up/down on a module with `on_scroll_up` / `on_scroll_down` runs the matching command and not the other one.
- [ ] A click command that writes to a file succeeds and leaves no zombie behind (`ps -o stat= -C sh` shows no `Z`).
- [ ] `cornice bar hide` removes the bar (and its exclusive zone) on every output; `cornice bar show` restores it; `cornice bar toggle` flips it. Both exit 0 and print nothing.
- [ ] With `bar.outputs = ["<your output name>"]`, the bar appears only there; on another output there is no bar and no reserved zone. An unknown name in `outputs` simply yields no bar.
- [ ] Focus highlight: with `{ kind = "applications" }`, focusing a window changes its chip to the accent background; focusing another app moves the highlight; closing all windows clears it. On a compositor that does not offer the wlroots window list (or the `wayland-info` global is absent), cornice logs one line and no chip is highlighted — it must not crash or blank the chips.
- [ ] `wayland-info | grep -i foreign_toplevel` on the target session lists `zwlr_foreign_toplevel_manager_v1` (the deprecated wlroots one) — this is what makes the highlight possible; if only `ext_foreign_toplevel_list_v1` is listed, focus cannot be shown.
- [ ] A notification whose client sends a `default` action: no pill is drawn for it, and a left click on the card body emits `ActionInvoked(id, "default")` and closes the card. Middle-click on the body closes without `ActionInvoked`.
- [ ] With a second process holding `org.freedesktop.Notifications` (start `dunst` and then cornice), cornice still starts, logs that notifications are unavailable, and `cornice bar toggle` still works.

## Known environment limits

- Sway uses wlroots' native layer-shell. River forwards it through the WM, so passing sway does not prove river compatibility.
- The development sandbox may not contain river, tailrace, a usable GPU/device input stack, or a screenshot session. Never report those checks as passed unless they were actually run.
- Visual motion is judged by a human. Logged rects or screenshots can confirm geometry, but not whether the spring feels right.
