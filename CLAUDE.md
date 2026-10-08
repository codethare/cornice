# Architectural term: a cornice is the horizontal band at the top of a facade. Literally "the band stuck to the top edge of the screen", and its codified proportions resonate naturally.

One process is two things at once: a `wlr-layer-shell` status bar and an `org.freedesktop.Notifications` daemon. The target environment is river plus a WM that implements `river-layer-shell-v1` (tailrace); sway / hyprland also work. The visual goal is spare, elegant, consistent — and that consistency comes from every proportion being derived from `theme.height` instead of a hand-written pixel value in each place.

The design doc is the only authority: `docs/superpowers/specs/2026-09-19-cornice-design.md`. Manual verification checklist: `docs/smoke.md`. This file states constraints only and does not restate the design.

## Non-negotiable boundaries

- Dependencies stay inside the 9 in `Cargo.toml` (`smithay-client-toolkit` / `wayland-client` / `calloop` / `calloop-wayland-source` / `cosmic-text` / `serde` / `toml` / `chrono` / `zbus`). **No new dependencies** — ask first if you think you need one.
- Pure software rendering (`wl_shm` + cosmic-text). No GPU, no dmabuf, no wgpu, no glow.
- edition 2024, `rust-version = "1.88"`.
- **No tags / workspace / `river.*` modules.** `river-status-unstable-v1` exists only in river-classic; modern river's `river-window-management-v1` is handed to a single WM client. A third-party bar structurally cannot get that state — this is not a question of effort, so do not retry. The only way to show it is for the WM to draw it itself. **Focus is the one exception, and it is not a `river.*` module**: river keeps the deprecated wlroots `zwlr_foreign_toplevel_manager_v1` (v0.4.6 and v0.4.8 both do; `Window.zig` sets `app_id` and `activated`, and `Server.zig::globalFilter` hands every global to normal clients), so the focused `app_id` comes from there through sctk's `reexports::protocols_wlr` — no new dependency. It must degrade silently when the global is absent; do not switch the chip source to it.
- Single-threaded main loop + a separate D-Bus thread. No async inside the main loop; cross-thread traffic goes through `calloop::channel` only; zbus interface methods must not block and must not use a blocking connection.
- Pure logic tests only (`cargo test`). No automated assertions at the protocol or pixel layer; maintain `docs/smoke.md` instead.

## Invariants (read this section before touching this code)

- **Waking**: at most one idle timer, one frame source and one coalescing repaint timer exist, each holding a registration token and removing itself with `TimeoutAction::Drop` when nothing is left to do. So **every path that starts or retargets a notification motion must call `ensure_frame_source()`** — otherwise a 220 ms animation waits for the next wake and jumps. The idle timer is armed by `ensure_idle_timer()` and dropped by `next_wake()` returning `None`: a bar with no `clock` module and no pending expiry must not wake once a second, and the notification channel handler re-arms it because a new entry may be the first thing to need a timer. Bar repaints go through `bar_changed()`, never `redraw_all()` directly: repaints are coalesced to `BAR_REDRAW_INTERVAL` so a chatty `exec` module cannot turn into a repaint per line.
- **Compositor pacing**: a notification surface requests a `wl_surface.frame` callback before each commit and counts the frames the compositor has not presented yet; at `MAX_PENDING_FRAMES` it stops rasterising while a motion is running. The timer is only the animation clock, so a compositor slower than 16 ms must not grow the shm pool. A finished motion always draws once, so a card can never be left half-faded.
- **Animation**: motion is stored per notification id, so cards may animate independently, but all share one frame source. Enter and reflow use `Easing::Spring`; exit uses `Easing::Smooth`; both are spring-based rather than cubic. A card entering scales/fades independently, remaining cards spring to their new column positions, and an exiting card is frozen in place, non-interactive, and removed only after its exit tween. `replaces_id` never replays enter.
- **Notification placement**: notifications do not participate in `BarLayout`; `[notification].position` (`left | center | right`) anchors a new → old vertical column below the bar. Every visible or exiting notification owns one output-less `Overlay` surface, so river + tailrace applies its focused-output default independently and `surface_enter` records the selected output. `left` uses `TOP | LEFT`, `center` uses `TOP` (the protocol centers the unanchored horizontal axis), and `right` uses `TOP | RIGHT`. Every card has its own background, text, input region and motion; there is no head/peek pair and no bar-overlap special case.
- **Input region**: `set_input_region` must never be given `None` (an infinite region swallows clicks on the whole bar); pass an empty region on a miss. The hit rects used for drawing and the input region are the same data, and button rects come before the card rect. The bar's region contains exactly the modules that have a command in `ModuleSpec::actions`; if the region cannot be created, the previous one stays in place rather than being cleared.
- **Clipping cost**: text is cut with `TextEngine::fit_text`, which shapes once and cuts at the glyph x positions and caches the `…` advance. Never re-measure a candidate string per character inside a per-frame path: that cost ~8 ms per card per frame, and an enter animation draws every frame.
- **First configure**: bar and notification surfaces must not attach a buffer before `configured`; the compositor disconnects the client with a protocol error.
- **Notification content is untrusted input**: `app_name`, summary and action labels use only their first line and are clipped to the card's remaining inner width; every body line is clipped to the card's inner width, and body is additionally bounded by `MAX_BODY_LINES = 5` / `MAX_BODY_CHARS = 300`. The headline reserves `card_gap / 2` plus the rule thickness before the right-aligned source, and the rule is drawn only when the source is non-empty. Multi-line bodies count toward card height; action hit rects never extend past the card's inner edge, and an action-only card cannot overlap its headline.
- **Queue**: visible order is new → old; entries past `max_visible` are kept but not drawn; capacity is `max_visible × 4` (on overflow the oldest is silently dropped). Close reason 1 = timeout, 2 = user, 3 = `CloseNotification`.
- **Config**: the valid ranges are `bar.height = 1..=256`, `bar.background_transparency = 0..=100` and `exec.interval = 1..=86400000`; out of range is a `line:column` error at the parse layer — no clamping, no silent downgrade. Transparency defaults to 0, meaning no extra transparency beyond `theme.background`'s own alpha; 100 makes only the bar background invisible. `bar.outputs` is empty by default, which means every output; an output whose name is unknown matches. A missing file is not an error. A missing `[notification]` section leaves the D-Bus daemon running but creates no cards; when present, `position` accepts only `left`, `center`, or `right`. The old `{ kind = "notification" }` bar module and `theme.radius` are invalid. A module's command strings are trimmed, and an empty one counts as absent.
- **Bar shape**: the bar is always a square-cornered full-width rectangle, like swaybar / i3bar. Draw it with `fill_rect` using `Theme::bar_background()`; there is no radius field, end inset, or rounded-end optical correction. Rounded geometry belongs only to notification cards and action pills.
- **Proportions**: `card_gap = max(height/5, 2)`, `card_padding = max(height/2, 4)` (`theme.rs::Theme::defaults`). Derived typography: text uses `TextEngine::optical_top`; the font cap metric is measured from an `H` on the first frame and cached. Notification width is fixed at `clamp(10 × height, 80, 420)`, radius is `1.2 × height`, minimum height is `Theme::card_min_h`, and content may grow it; the first card starts at `bar.margin + height + card_gap`, with side inset `2 × card_gap`. Notification corners are continuous (`geom::CORNER_EXPONENT = 4`), not circular arcs. For any new dimension, ask "can this be derived from height?" first.

- **Debug restart**: `SIGUSR1` re-execs the current binary through a raw async-signal-safe `execv` handler in `main.rs`; the key binding lives in the WM (`pkill -USR1 -x cornice`). cornice is `KeyboardInteractivity::None`, so it can never receive global keys itself.
- **CLI**: no arguments starts the daemon; `cornice notification dismiss <id>`, `cornice notification dismiss -a|--all` and `cornice bar toggle | show | hide` are short-lived client processes that only make a D-Bus call (no config, no Wayland). The verbs follow `makoctl` / `fnottctl`, and the two groups keep the notification and bar commands apart. Single dismiss is the spec `CloseNotification`; `--all` is cornice's own `org.cornice.Control.CloseAll` on the notification object path, which stays off the spec interface; the `bar` group addresses `org.cornice.Control` as the bus name, so hiding the bar still works when another process owns `org.freedesktop.Notifications`.

## Files and responsibilities (do not cross the lines)

| File | Sole responsibility |
|---|---|
| `geom.rs` | `Rect` / `Color` primitives. Alpha passes straight through; only `to_shm_bytes` premultiplies |
| `canvas.rs` | Writing and clipping on a `wl_shm` byte buffer, including the continuous (superellipse) corner curve. Does not know about text |
| `text.rs` | cosmic-text wrapper + clipping by width. Does not know about layout |
| `widget.rs` | `Span` / `Action` / `Event` / `Module` / `Toplevel` primitives. Kept because design §5 mandates them |
| `theme.rs` | Colours and derived proportions |
| `config.rs` | TOML schema, validation, errors with line numbers. Does not know about rendering |
| `anim.rs` | Easing and tweening. No keyframes, no physics |
| `bar/mod.rs` | Left/centre/right layout and `BarLayout`. Does not draw text |
| `bar/modules.rs` | `clock` / `exec` / `applications`, plus `spawn_detached` for module click commands |
| `notify/queue.rs` | Notification state machine. Touches neither D-Bus nor rendering |
| `notify/service.rs` | zbus interface and signals. Does not touch rendering |
| `notify/view.rs` | Card layout, drawing, hit testing. Does not touch the protocol |
| `wayland/mod.rs` | The only place that holds `State` and the protocol callbacks, including the `foreign_toplevel_list` snapshot and the `zwlr_foreign_toplevel_manager_v1` focus snapshot. No Wayland types may appear in any other file |
| `main.rs` | Process assembly: config load, `wayland::run`, and the `SIGUSR1` debug re-exec handler |
| `cli.rs` | Client mode: one subcommand → one D-Bus call. No config, no Wayland |

## Applications module

- The compositor reports toplevels through `ext-foreign-toplevel-list-v1`; sctk exposes it as `foreign_toplevel_list` and re-exports the protocol types from `reexports::protocols`, so no Cargo dependency is added. That list has no focus state, so the focused `app_id` comes from the separate wlroots window list above; the two are independent inputs and either may be missing.
- `bar/modules.rs::Applications` is pure logic: it groups `app_id`s, sorts them, and returns one `Span::application(monogram, count, focused)` per app. The module may sit in any of the three lists.
- A chip is `app_icon = min(clamp(3 × height / 5, 1, 32), height)` square; the count is drawn at the chip's bottom-right at `app_icon / 3` and only when the count is greater than one. The focused chip uses `Theme::app_icon_focused_background` and accent text; with no focus source nothing is highlighted.

## Module input

- A module's `on_click` / `on_scroll_up` / `on_scroll_down` commands come from `ModuleSpec::actions` into `Sections::*_actions`; `draw_bar` copies the module rects into `Bar::hits` and sets the bar's input region from exactly those rects. A module with no command is not in the region, so empty bar space stays click-through, and `set_input_region` is never `None`.
- Commands run through `bar::modules::spawn_detached` (`sh -c`, stdio discarded, reaped on a throwaway thread); the main loop never waits for one.
- `bar.outputs` (empty = every output) is reconciled by `State::sync_bars`; a bar for an output whose name has not arrived yet is created and then removed if the name turns out not to match.
- Real image icons are out of scope: neither the protocol nor the dependency set can decode one.


## Style

- Comments in English, explaining "why" and never "what"; new error messages also carry `line:column` so they can be located.
- If it fits on one line, do not write two. No "might need it later" abstractions; the only exception is the `Span` primitive design §5 mandates.
- State lives in `State`, accessed from a single thread. No locks, no `Arc<Mutex>`.
- `cargo build` must produce no warnings. `#[allow]` may not be used to hide one (only exception: the primitives design §5 mandates in `widget.rs`).
- Every test must be able to fail. A test whose assertion cannot fail is no test at all.
- Commit with `jj commit -m "..."`, message in English (the repo is jj/git colocated, no main branch).
