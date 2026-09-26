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
- per-card enter/exit/reflow motion endpoints and independence;
- queue replacement, visibility, capacity, timeout and close semantics;
- D-Bus-independent notification state transitions.

- **Proves**: pure logic has no regressions.
- **Does not prove**: layer-shell placement, pixels, pointer input or animation feel.

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
  gdbus call --session --dest org.freedesktop.Notifications \
    --object-path /org/freedesktop/Notifications \
    --method org.freedesktop.Notifications.Notify \
    test 0 "" "title" "body" "[]" "{}" -1
  sleep 1
  kill %1
'
```

- **Proves**: the daemon owns `org.freedesktop.Notifications`, accepts `notify-send`-compatible requests and does not crash while creating card surfaces.
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

Expected: the bar configures as `1280x30`; sending notifications produces one `notif <id> configured: WxH` line per visible card and no protocol error before the timeout.

- **Proves**: attachment to wlroots layer-shell, first-configure ordering and basic per-card surface mapping.
- **Does not prove**: target river behaviour or visual correctness.

## 4. Manual river + tailrace pass

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
- [ ] All notification-card corners read as continuous/squircle curves. The translucent material does not show a doubled overlap, black seam or square notch.
- [ ] `left`, `center`, and `right` anchor the whole vertical column correctly. Side cards keep `2 × card_gap` from the screen edge; the centre card is truly centred.
- [ ] Three simultaneous notifications produce three independent cards in new → old order with one `card_gap` between them. No card is collapsed into a peek pill.
- [ ] Each card enters with its own scale/fade spring; older cards spring to new vertical positions. Replacing an id updates in place without replaying enter.
- [ ] Closing one card fades/scales it out while the remaining cards spring upward. The exiting card cannot be clicked.
- [ ] Left-clicking an action emits `ActionInvoked` before closing; clicking elsewhere on that card emits `NotificationClosed(..., 2)`. Middle-click also closes the card body.
- [ ] Transparent pixels inside and around each surface pass clicks through to the window below; removing `[notification]` creates no card surface while the D-Bus daemon still runs.
- [ ] No buffer is attached before the first configure; the compositor log has no protocol error.
- [ ] `background_transparency = -1` and `101` both fail with `line:column`; values `0`, `50`, and `100` are accepted.
- [ ] Sending more than `max_visible` keeps the excess queued. Closing a visible card promotes the next notification with an enter animation.
- [ ] With a second output focused, each newly created card appears on that output. Removing the output rebuilds live cards on the remaining/default output.
- [ ] Idle CPU does not show sustained 60 fps wakeups after all enter/exit/reflow motion has completed.

## Known environment limits

- Sway uses wlroots' native layer-shell. River forwards it through the WM, so passing sway does not prove river compatibility.
- The development sandbox may not contain river, tailrace, a usable GPU/device input stack, or a screenshot session. Never report those checks as passed unless they were actually run.
- Visual motion is judged by a human. Logged rects or screenshots can confirm geometry, but not whether the spring feels right.
