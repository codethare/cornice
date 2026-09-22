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
- **Animation**: one at a time only (`Anim` + `AnimKind::{Enter, Exit}`). A new `Enter` overrides an in-flight `Exit`.
- **Island origin**: `island_start()` must be exactly `BarLayout.right_cluster`, and `right_cluster.right()` must equal `output_w` (the right end of the bar capsule is where the island departs from). During entry y is pinned to `bar.height` for the whole flight, h grows out of 0, and x/w interpolate from the right cluster to the card: the first frame has height 0 (i.e. "nothing has appeared yet"), and from the second frame on it shares one edge with the bar's bottom edge, the junction being the card's rounded shoulder tangent to the bar's bottom edge. Those two together are the "grows out of the right cluster" contract; drop either one and you get a seam or a square-cornered block. It follows that `bar.margin` acts on the vertical axis only, and bar and notification surfaces must stay in the same horizontal coordinate space.
- **Input region**: `set_input_region` must never be given `None` (an infinite region swallows clicks on the whole bar); pass an empty region on a miss. The hit rects used for drawing and the input region are the same data, and button rects come before the card rect.
- **First configure**: bar and notification surfaces must not attach a buffer before `configured`; the compositor disconnects the client with a protocol error.
- **Notification content is untrusted input**: summary, every body line and action label are clipped to the card's inner width; body is additionally bounded by `MAX_BODY_LINES = 5` / `MAX_BODY_CHARS = 300`. Multi-line bodies count toward card height.
- **Queue**: visible order is new → old; entries past `max_visible` are kept but not drawn; capacity is `max_visible × 4` (on overflow the oldest is silently dropped). Close reason 1 = timeout, 2 = user, 3 = `CloseNotification`.
- **Config**: the valid range for `bar.height` is `1..=256`; out of range is a `line:column` error at the parse layer — no clamping, no silent downgrade. A missing file is not an error.
- **Proportions**: `radius = height/2`, `card_gap = max(height/5, 2)`, `card_padding = max(height/2, 4)` (`theme.rs::Theme::defaults`). Two further values are derived typography rather than knobs: the rounded-end inset of the bar `bar::end_inset(radius) = r·(1-1/√2)`, and the text's cap-height placement `TextEngine::optical_top / cap_top` (the font metric is measured by rasterising an `H` on the first frame and then cached). For any new dimension, ask "can this be derived from height?" first.

## Files and responsibilities (do not cross the lines)

| File | Sole responsibility |
|---|---|
| `geom.rs` | `Rect` / `Color` primitives. Alpha passes straight through; only `to_shm_bytes` premultiplies |
| `canvas.rs` | Writing and clipping on a `wl_shm` byte buffer. Does not know about text |
| `text.rs` | cosmic-text wrapper + clipping by width. Does not know about layout |
| `widget.rs` | `Span` / `Action` / `Event` / `Module` primitives. Kept because design §5 mandates them |
| `theme.rs` | Colours and derived proportions |
| `config.rs` | TOML schema, validation, errors with line numbers. Does not know about rendering |
| `anim.rs` | Easing and tweening. No keyframes, no physics |
| `bar/mod.rs` | Left/centre/right layout and `BarLayout`. Does not draw text |
| `bar/modules.rs` | `clock` / `exec`. Does not know about layout |
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
