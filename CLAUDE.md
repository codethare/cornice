# Architectural term: a cornice is the horizontal band at the top of a facade. Literally "the band stuck to the top edge of the screen", and its codified proportions resonate naturally.

One process is two things at once: a `wlr-layer-shell` status bar and an `org.freedesktop.Notifications` daemon. The target environment is river plus a WM that implements `river-layer-shell-v1` (tailrace); sway / hyprland also work. The visual goal is spare, elegant, consistent — and that consistency comes from every proportion being derived from `theme.height` instead of a hand-written pixel value in each place.

The design doc is the only authority: `docs/superpowers/specs/2026-09-19-cornice-design.md`. Manual verification checklist: `docs/smoke.md`. This file states constraints only and does not restate the design.

## Non-negotiable boundaries

- Dependencies stay inside the 9 in `Cargo.toml` (`smithay-client-toolkit` / `wayland-client` / `calloop` / `calloop-wayland-source` / `cosmic-text` / `serde` / `toml` / `chrono` / `zbus`). **No new dependencies** — ask first if you think you need one.
- Pure software rendering (`wl_shm` + cosmic-text). No GPU, no dmabuf, no wgpu, no glow.
- edition 2024, `rust-version = "1.88"`.
- **No tags / workspace / `river.*` modules.** `river-status-unstable-v1` exists only in river-classic; modern river's `river-window-management-v1` is handed to a single WM client. A third-party bar structurally cannot get that state — this is not a question of effort, so do not retry. The only way to show it is for the WM to draw it itself.
- Single-threaded main loop + a separate D-Bus thread. No async inside the main loop; cross-thread traffic goes through `calloop::channel` only; zbus interface methods must not block and must not use a blocking connection.
- Pure logic tests only (`cargo test`). No automated assertions at the protocol or pixel layer; maintain `docs/smoke.md` instead.

## Invariants (read this section before touching this code)

- **Waking**: there is exactly one timer, and it is only rescheduled inside its own callback. So **every path that starts an animation must call `ensure_frame_source()`** — otherwise the tween waits for the next per-second wake to be judged complete and a 220ms animation jumps. The frame source removes itself with `TimeoutAction::Drop`.
- **Animation**: one at a time only (`Anim` + `AnimKind::{Enter, Exit}`) and spring-based, not cubic (`Easing::Spring` when something opens, `Easing::Smooth` when it closes — Apple's motion language, and `ω = 8` keeps the natural period inside the configured `enter_ms`). A new `Enter` overrides an in-flight `Exit`; a new notification while the stack is *already open* extends it (`Enter { fade: false }`) instead of replaying the opening. `y` is 0 at every frame and only `h` moves — the shape stretches, it never slides.
- **Notification placement and the stretch**: the `notification` module (configurable once, in any of the three sections) is the anchor — the stack's x/w is that module's slot in `BarLayout`; `island_start()` / `right_cluster` no longer exist. The head card's trailing source label and summary share the bar's own text line (`optical_top(font, 0, theme.height)`, exactly like every other module); only rows that do not fit stretch below it. Everything behind the head is one bar-tall peek pill `card_gap` below, showing the next summary plus an `×N` count. When body or actions are present, the head first adds `card_gap / 2` and a `max(card_gap / 3, 1)` separator before the detail block; a short card remains entirely inside the bar. The head card is *one* shape with the bar and must not paint any background above the bar's bottom edge: the colour is translucent, so a second pass would darken the overlap and the stretch would read as a layer glued under the bar instead of the bar's own material. Its span must stay inside the bar's straight bottom edge `[radius, output_w - radius]` and its top corners stay square; under the pill's curve a rounded corner leaves a notch, which *is* the background coming apart at the join. Rows are clipped to the animating shape, and the head card's text fades in (`clamp((t - 0.35) / 0.65)`) because it does not move. `Sections` reserves one card's width for the module, and a zero-width module takes no gap either, so an idle bar does not shift when the first card arrives.
- **Input region**: `set_input_region` must never be given `None` (an infinite region swallows clicks on the whole bar); pass an empty region on a miss. The hit rects used for drawing and the input region are the same data, and button rects come before the card rect.
- **First configure**: bar and notification surfaces must not attach a buffer before `configured`; the compositor disconnects the client with a protocol error.
- **Notification content is untrusted input**: `app_name`, summary and action labels use only their first line and are clipped to the card's remaining inner width; every body line is clipped to the card's inner width, and body is additionally bounded by `MAX_BODY_LINES = 5` / `MAX_BODY_CHARS = 300`. Multi-line bodies count toward card height; action hit rects never extend past the card's inner edge, and an action-only card cannot overlap its headline.
- **Queue**: visible order is new → old; entries past `max_visible` are kept but not drawn; capacity is `max_visible × 4` (on overflow the oldest is silently dropped). Close reason 1 = timeout, 2 = user, 3 = `CloseNotification`.
- **Config**: the valid range for `bar.height` is `1..=256`; out of range is a `line:column` error at the parse layer — no clamping, no silent downgrade. A missing file is not an error. The `notification` module may be configured only once (a second one is a `line:column` error); without it no cards are drawn at all.
- **Proportions**: `radius = height/2`, `card_gap = max(height/5, 2)`, `card_padding = max(height/2, 4)` (`theme.rs::Theme::defaults`). Two further values are derived typography rather than knobs: the rounded-end inset of the bar `bar::end_inset(radius) = geom::corner_inset(radius)`, and the text's cap-height placement `TextEngine::optical_top / cap_top` (the font metric is measured by rasterising an `H` on the first frame and then cached). The notification card's size follows Apple's expanded Live Activity (371×84–160 pt and a 44 pt radius on a 36.67 pt island): `card_w = clamp(10 × height, 80, 420)` and `card_radius = 1.2 × height` — fixed sizes, not content-derived, so the stack is a column of equal cards. Every corner in the UI is a continuous (superellipse, `geom::CORNER_EXPONENT = 4`) corner, not a circular arc. For any new dimension, ask "can this be derived from height?" first.

## Files and responsibilities (do not cross the lines)

| File | Sole responsibility |
|---|---|
| `geom.rs` | `Rect` / `Color` primitives. Alpha passes straight through; only `to_shm_bytes` premultiplies |
| `canvas.rs` | Writing and clipping on a `wl_shm` byte buffer, including the continuous (superellipse) corner curve. Does not know about text |
| `text.rs` | cosmic-text wrapper + clipping by width. Does not know about layout |
| `widget.rs` | `Span` / `Action` / `Event` / `Module` primitives. Kept because design §5 mandates them |
| `theme.rs` | Colours and derived proportions |
| `config.rs` | TOML schema, validation, errors with line numbers. Does not know about rendering |
| `anim.rs` | Easing and tweening. No keyframes, no physics |
| `bar/mod.rs` | Left/centre/right layout and `BarLayout`. Does not draw text |
| `bar/modules.rs` | `clock` / `exec` / `notification` (the last draws no text: it reserves the card's width and marks where the stretch hangs) |
| `notify/queue.rs` | Notification state machine. Touches neither D-Bus nor rendering |
| `notify/service.rs` | zbus interface and signals. Does not touch rendering |
| `notify/view.rs` | Card layout, drawing, hit testing. Does not touch the protocol |
| `wayland/mod.rs` | The only place that holds `State` and the protocol callbacks. No Wayland types may appear in any other file |

## Style

- Comments in English, explaining "why" and never "what"; new error messages also carry `line:column` so they can be located.
- If it fits on one line, do not write two. No "might need it later" abstractions; the only exception is the `Span` primitive design §5 mandates.
- State lives in `State`, accessed from a single thread. No locks, no `Arc<Mutex>`.
- `cargo build` must produce no warnings. `#[allow]` may not be used to hide one (only exception: the primitives design §5 mandates in `widget.rs`).
- Every test must be able to fail. A test whose assertion cannot fail is no test at all.
- Commit with `jj commit -m "..."`, message in English (the repo is jj/git colocated, no main branch).
