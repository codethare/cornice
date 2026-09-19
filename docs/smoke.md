# Smoke check

Each section states what it **can prove** and what it **cannot prove**. The first three sections run in the dev sandbox; section 4 can only be done by the user in the target environment.

## 1. Pure logic (`cargo test`)

```sh
cargo test
```

Covers: geometry (`Rect`/`Color`), canvas (rounded corners / blending / out of bounds), text truncation, config parsing (errors carry line numbers), bar layout, the `clock`/`exec` modules, easing and tweening, the notification queue state machine, and notification card geometry and hit testing.

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

## 4. river + tailrace (user-verified only)

- [ ] After `cornice` starts a band appears at the top, its height matches `[bar] height`, and windows are not covered (the exclusive zone takes effect)
- [ ] Left/center/right sections sit where the docs say; a non-existent module (e.g. `river.tags`) in the config produces an error with a line number
- [ ] Notifications: after `notify-send "title" "body"` a card Morphs out of the right-hand bar component in the top-right corner
- [ ] `notify-send -u critical ...` does not auto-dismiss; `notify-send -t 2000 ...` disappears after 2 seconds
- [ ] Same-id replace: run `notify-send -r 1 ...` twice; the second updates in place and does not replay the animation
- [ ] Multiple: once consecutive `notify-send` calls exceed `max_visible`, the extras stay hidden and slide in as earlier ones disappear
- [ ] `notify-send --action=open=Open ...` shows a button, and clicking it produces an `ActionInvoked` visible in `dbus-monitor`
- [ ] Clicking a card / middle-click → the card disappears and `NotificationClosed` (reason 2) is emitted
- [ ] Click-through on transparent areas: regions outside the notification cards still reach the window below
- [ ] **Multi-monitor: notifications appear only on the first output that has a bar** — a known compromise (see below), not a bug
- [ ] When idle, `powertop` or `perf stat` shows no sustained 60fps-level wakeups

### Known compromises (read first)

- **Multi-monitor**: notifications are pinned to "the first output that has a bar". layer-shell client surfaces are **non-interactive** (keyboard focus is invisible), so there is no way to know which output currently has focus — hence design doc §7's "deliver to the focused output" cannot be implemented. Upgrade path: create one notification surface per output and show the same queue on each.
- **Target-environment gap**: passing section 3 on sway does not mean passing on river + tailrace (river's layer-shell is forwarded by the WM).
