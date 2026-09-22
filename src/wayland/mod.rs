//! Wayland connection, layer surface, shm buffers and the main event loop.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use calloop::{timer::{TimeoutAction, Timer}, EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{BTN_LEFT, BTN_MIDDLE, PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::anim::Tween;
use crate::geom::Rect;
use crate::notify::queue::Notification;
use crate::widget::Action;

pub const LAYER_NAMESPACE: &str = "cornice";

/// Animation frame interval (~60fps); used only while animating.
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// The clock's minimum tick resolution; the only wakeup source when idle.
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(1);

/// The next instant needing a wakeup: the frame being animated, the clock tick, notification expiry — whichever comes first.
/// Keep a single timer source that wakes once per second when idle, never spinning at 60fps.
pub fn next_deadline(now: Instant, animating: bool, next_expiry: Option<Instant>) -> Instant {
    let mut d = now + CLOCK_INTERVAL;
    if animating {
        d = d.min(now + FRAME_INTERVAL);
    }
    if let Some(e) = next_expiry {
        // an already-expired time must not be returned as a past instant, or it becomes a busy wait
        d = d.min(e.max(now));
    }
    d
}

pub struct Bar {
    pub layer: LayerSurface,
    pub pool: SlotPool,
    pub width: u32,
    pub height: u32,
    pub configured: bool,
}

pub struct NotifSurface {
    pub layer: LayerSurface,
    pub pool: SlotPool,
    pub width: u32,
    pub height: u32,
    pub configured: bool,
}

/// The geometric endpoints of the notification animation. Only one animation at a time (see `redraw_notifications`).
#[derive(Clone)]
enum AnimKind {
    /// The new card grows out of the right cluster; `id` is the notification entering (it must still be at the head).
    Enter(u32),
    /// A closed/expired card shrinks back to the right cluster; the snapshot keeps content drawable during the leave.
    Exit(Notification),
}

#[derive(Clone)]
struct Anim {
    kind: AnimKind,
    tween: Tween,
    start: Rect,
    end: Rect,
}

pub struct State {
    pub qh: QueueHandle<State>,
    pub registry: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub compositor: CompositorState,
    pub layer_shell: LayerShell,
    pub shm: Shm,
    pub pointer: Option<wl_pointer::WlPointer>,
    pub bars: HashMap<wl_output::WlOutput, Bar>,
    pub bar_height: u32,
    pub exit: bool,
    /// Whether an enter/leave animation is running; maintained by `set_anim` and driving the frame source at ~16ms.
    pub animating: bool,
    pub cfg: crate::config::Config,
    pub theme: crate::theme::Theme,
    /// Text layout engine; used for drawing from Task 5 on, created here to avoid changing `run`'s signature again.
    pub text: crate::text::TextEngine,
    /// The left/center/right module lists.
    pub sections: crate::bar::Sections,
    /// The notification queue (pure state; rendering arrives in Task 9).
    pub queue: crate::notify::queue::Queue,
    /// the D-Bus connection; None when the bus name is taken or the session bus is unreachable (degrade, do not exit).
    pub dbus: Option<zbus::blocking::Connection>,
    /// Notification layer surface; created while the queue is non-empty, destroyed once it empties.
    pub notif: Option<NotifSurface>,
    /// The clickable regions drawn in the last frame (the same data as set_input_region).
    pub hits: Vec<(Rect, Action)>,
    /// the in-flight enter/leave animation; at most one at a time. An internal detail; the type is not exposed.
    anim: Option<Anim>,
    /// The output the notification surface hangs on (the first output that has a bar).
    notif_output: Option<wl_output::WlOutput>,
    /// Registration handle for the frame source: inserted while animating, set to None after it self-Drops when idle.
    frame_id: Option<RegistrationToken>,
    /// Event-loop handle so `set_anim` can insert the frame source from any callback.
    /// `'static` because `State` itself is `'static` (no field borrows a local scope).
    loop_handle: LoopHandle<'static, State>,
}

pub fn run(cfg: crate::config::Config) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("cannot connect to Wayland: {e}"))?;
    let (globals, event_queue) = registry_queue_init(&conn).map_err(|e| format!("failed to read globals: {e}"))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).map_err(|e| format!("missing wl_compositor: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| {
        format!("layer shell unavailable: {e}\n  on river the WM must implement river-layer-shell-v1 (tailrace does)")
    })?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("missing wl_shm: {e}"))?;

    let bar_height = cfg.bar.height as u32;
    let theme = cfg.theme.clone();
    let (sections, exec_rx) = crate::bar::Sections::from_config(&cfg);
    let max_visible = cfg.notification.max_visible;
    let (notif_tx, notif_rx) = calloop::channel::channel::<crate::notify::queue::Request>();
    let dbus = match crate::notify::service::spawn(notif_tx) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("cornice: notifications unavailable: {e}");
            None
        }
    };
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().map_err(|e| format!("failed to create the event loop: {e}"))?;
    let handle = event_loop.handle();

    let mut state = State {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        qh: qh.clone(),
        pointer: None,
        bars: HashMap::new(),
        bar_height,
        exit: false,
        animating: false,
        cfg,
        theme,
        text: crate::text::TextEngine::new(),
        sections,
        queue: crate::notify::queue::Queue::new(max_visible),
        dbus,
        notif: None,
        hits: Vec::new(),
        anim: None,
        notif_output: None,
        frame_id: None,
        loop_handle: handle.clone(),
    };
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| format!("failed to insert the wayland source: {e}"))?;
    handle.insert_source(exec_rx, |msg, _meta, state: &mut State| {
        let calloop::channel::Event::Msg(ev) = msg else { return };
        if state.sections.update(&ev) { state.redraw_all(); }
    }).map_err(|e| format!("failed to insert the exec channel: {e}"))?;

    handle.insert_source(notif_rx, |msg, _meta, state: &mut State| {
        let calloop::channel::Event::Msg(req) = msg else { return };
        let now = Instant::now();
        match state.queue.apply(req, now) {
            crate::notify::queue::Outcome::Added(id) => {
                state.start_enter(id, now);
                state.redraw_notifications(now);
            }
            // An in-place replace does not replay the animation (design §6), it only redraws.
            crate::notify::queue::Outcome::Replaced(_) => state.redraw_notifications(now),
            crate::notify::queue::Outcome::CloseRequested(id) => {
                state.close_visible(id, 3, now);
                state.redraw_notifications(now);
            }
            crate::notify::queue::Outcome::Ignored => {}
        }
    }).map_err(|e| format!("failed to insert the notification channel: {e}"))?;

    // Idle timer: handles the per-second clock tick and notification expiry only; animation frames come from a separate frame source (see ensure_frame_source).
    // so animating is passed as a fixed false here — otherwise it would form two 16ms sources with the frame source during animation.
    // A calloop timer's event is the *deadline* it was scheduled for, not the current time (calloop-0.14
    // sources/timer.rs: "can be earlier than the current time depending on the event loop congestion"), and
    // `ToInstant(deadline + 16ms)` lets that deadline fall further behind on every slow frame. tweening and
    // expiry compare it against `Instant::now()` stamps, so the deadline is not a usable clock here: take the clock here.
    handle.insert_source(Timer::from_duration(CLOCK_INTERVAL), |_deadline, _meta, state: &mut State| {
        let now = Instant::now();
        state.on_wake(now);
        TimeoutAction::ToInstant(next_deadline(now, false, state.queue.next_expiry()))
    }).map_err(|e| format!("failed to insert the timer: {e}"))?;

    while !state.exit {
        event_loop.dispatch(None, &mut state).map_err(|e| format!("event loop error: {e}"))?;
    }
    Ok(())
}

impl State {
    fn add_bar(&mut self, output: wl_output::WlOutput) {
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some(LAYER_NAMESPACE), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        // The margin is vertical only (top): this keeps the horizontal coordinate systems of the bar and notification surface identical (both span the full width),
        // the island's t=0 "aligned to the right cluster pixel" only holds then; a horizontal inset would break that premise, so it is deliberately unsupported.
        layer.set_margin(self.cfg.bar.margin, 0, 0, 0);
        // Design §3: exclusive_zone = bar height + 2×margin (a margin above and below).
        layer.set_exclusive_zone(self.bar_height as i32 + 2 * self.cfg.bar.margin);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, self.bar_height); // width 0 + both horizontal anchors = the compositor gives full width
        layer.commit();
        let pool = SlotPool::new((self.bar_height as usize) * 4096 * 4, &self.shm).expect("failed to create the shm pool");
        self.bars.insert(output, Bar { layer, pool, width: 0, height: self.bar_height, configured: false });
    }

    /// Redraw the bar on every output once.
    pub fn redraw_all(&mut self) {
        let outputs: Vec<_> = self.bars.keys().cloned().collect();
        for o in outputs { self.redraw(&o); }
    }

    /// Timer fired: clean up expired notifications (visible ones play the leave animation), step animations, refresh the clock.
    /// exec output goes through the channel and never passes through here.
    fn on_wake(&mut self, now: Instant) {
        // Expire first, then compute the next wakeup: when the expiry lands in the past,
        // e.max(now) in next_deadline returns now, and ToInstant(now) becomes a busy loop.
        let visible_expired: Vec<u32> = self.queue.visible()
            .iter()
            .filter(|n| n.expire.is_some_and(|d| now.saturating_duration_since(n.created) >= d))
            .map(|n| n.id)
            .collect();
        for id in visible_expired {
            self.close_visible(id, 1, now);
        }
        // An invisible expired entry (outside the visible window) has no leave animation: clear it and emit the close signal.
        for id in self.queue.expire(now) {
            if let Some(c) = &self.dbus {
                let _ = crate::notify::service::emit_closed(c, id, 1);
            }
        }
        // Refresh the clock/bar layout first: the enter animation's island origin must follow the latest right cluster (the clock ticking moves it).
        if self.sections.update(&crate::widget::Event::Wake) {
            self.redraw_all();
        }
        // Then draw notifications: while animating, step at a 16ms cadence; once done, redraw_notifications clears animating.
        if self.animating {
            self.redraw_notifications(now);
        }
    }

    /// Set/clear the animation and maintain `animating` in the same change, otherwise
    /// Leaving `animating` true would spin the timer at 16ms forever (ruling #9).
    fn set_anim(&mut self, anim: Option<Anim>) {
        self.animating = anim.is_some();
        self.anim = anim;
        if self.animating {
            self.ensure_frame_source();
        }
    }

    /// Frame source: drives `on_wake` at ~16ms while animating; when idle the callback does its own `TimeoutAction::Drop`.
    /// Make sure it exists whenever `animating` flips from false to true — whichever callback starts the animation
    /// (notification channel, pointer clicks, expiry cleanup). Otherwise the single idle timer may sleep for up to 1s before waking,
    /// while enter/leave lasts only 160~220ms, so the tween has long finished and the card jumps (final review Critical #1).
    fn ensure_frame_source(&mut self) {
        if self.frame_id.is_some() || !self.animating {
            return;
        }
        let token = self.loop_handle.insert_source(
            Timer::from_duration(FRAME_INTERVAL),
            |_deadline, _meta, state: &mut State| {
                let now = Instant::now(); // the deadline drifts behind real time on slow frames; see the idle timer above
                state.on_wake(now);
                if state.animating {
                    TimeoutAction::ToInstant(now + FRAME_INTERVAL)
                } else {
                    // Animation finished: clear the registration; the source removes itself after the callback returns (TimeoutAction::Drop → PostAction::Remove).
                    state.frame_id = None;
                    TimeoutAction::Drop
                }
            },
        );
        if let Ok(token) = token {
            self.frame_id = Some(token);
        }
    }

    /// The bar width (full width) of the output the notification surface should target.
    fn notif_output_width(&self) -> Option<i32> {
        self.notif_output
            .as_ref()
            .and_then(|o| self.bars.get(o))
            .or_else(|| self.bars.values().next())
            .map(|b| (b.width.max(1)) as i32)
    }

    /// Notification surface height: the gap below the bar plus max_visible cards (each estimated at up to 3 lines of text).
    fn notif_surface_height(&self) -> u32 {
        let line = (self.theme.font.size * 1.35).ceil() as i32;
        // Worst-case height per card: summary 1 line + body up to MAX_BODY_LINES lines + actions 1 line.
        // must be ≥ the true upper bound of card_size, or the bottom-most card gets clipped.
        let lines_per_card = 1 + crate::notify::queue::MAX_BODY_LINES as i32 + 1;
        let per_card = self.theme.card_padding * 2 + line * lines_per_card + self.theme.card_gap;
        let n = self.cfg.notification.max_visible as i32;
        // The head card hangs straight off the bar; only the gaps between cards are added.
        let h = self.cfg.bar.margin + self.cfg.bar.height + n * per_card;
        h.max(1) as u32
    }

    /// Compute the island origin (the bar's right cluster) and the static rects of the currently visible cards.
    fn notif_geometry(&mut self, output_w: i32) -> (Rect, Vec<Rect>) {
        let widths = self.sections.widths(&mut self.text, &self.theme);
        let bar_layout = crate::bar::layout(&widths, output_w, &self.theme);
        let island = crate::notify::view::island_start(&bar_layout, &self.theme);
        let visible = self.queue.visible();
        let mut sizes = Vec::with_capacity(visible.len());
        for n in visible {
            sizes.push(crate::notify::view::card_size(n, &mut self.text, &self.theme));
        }
        // The head card starts at the bar's bottom edge, not below a gap: that shared edge is what
        // makes the card read as extruded from the bar instead of parked under it.
        let top = self.theme.height;
        let rects = crate::notify::view::card_rects(visible.len(), &sizes, output_w, &self.theme, top);
        (island, rects)
    }

    /// Make sure the notification surface exists while the queue is non-empty or a leave animation is running.
    fn ensure_notif_surface(&mut self) {
        if self.notif.is_some() { return; }
        let Some(output) = self.notif_output.clone().or_else(|| self.bars.keys().next().cloned()) else { return };
        self.notif_output = Some(output.clone());
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(LAYER_NAMESPACE), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        layer.set_margin(self.cfg.bar.margin, 0, 0, 0);
        layer.set_exclusive_zone(-1); // takes no space
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let height = self.notif_surface_height();
        layer.set_size(0, height);
        layer.commit();
        let pool = SlotPool::new((height as usize) * 4096 * 4, &self.shm).expect("failed to create the notification shm pool");
        self.notif = Some(NotifSurface { layer, pool, width: 0, height, configured: false });
    }

    /// Destroy the notification surface. sctk 0.21's LayerSurface has no destroy(); its Drop destroys the proxy.
    fn destroy_notif_surface(&mut self) {
        self.notif = None;
        self.hits.clear();
        self.notif_output = None;
    }

    /// Start the enter animation (the id must be the one just inserted at the head).
    fn start_enter(&mut self, id: u32, now: Instant) {
        let Some(output_w) = self.notif_output_width() else { return };
        let (island, rects) = self.notif_geometry(output_w);
        let Some(end) = rects.first().copied() else { return };
        self.set_anim(Some(Anim {
            kind: AnimKind::Enter(id),
            tween: Tween::new(now, self.cfg.notification.enter_ms),
            start: island,
            end,
        }));
    }

    /// Close one notification and emit the signal; if it is on screen, play the leave animation.
    fn close_visible(&mut self, id: u32, reason: u32, now: Instant) {
        let Some(output_w) = self.notif_output_width() else {
            // No bar yet: remove and signal directly, no animation.
            if self.queue.remove(id).is_some() {
                if let Some(c) = &self.dbus {
                    let _ = crate::notify::service::emit_closed(c, id, reason);
                }
            }
            return;
        };
        let (island, rects) = self.notif_geometry(output_w);
        let Some(idx) = self.queue.visible().iter().position(|n| n.id == id) else {
            // Outside the visible window → remove and signal directly, no animation.
            if self.queue.remove(id).is_some() {
                if let Some(c) = &self.dbus {
                    let _ = crate::notify::service::emit_closed(c, id, reason);
                }
            }
            return;
        };
        let start = rects.get(idx).copied().unwrap_or(island);
        let Some(snapshot) = self.queue.remove(id) else { return };
        self.set_anim(Some(Anim {
            kind: AnimKind::Exit(snapshot),
            tween: Tween::new(now, self.cfg.notification.exit_ms),
            start,
            end: island,
        }));
        if let Some(c) = &self.dbus {
            let _ = crate::notify::service::emit_closed(c, id, reason);
        }
    }

    /// Redraw the notification surface: lifetime, animation stepping, card drawing and input region in one pass.
    fn redraw_notifications(&mut self, now: Instant) {
        // 1. Finish animations: done, or the Enter id is no longer at the head (displaced/closed).
        let mut drop_anim = false;
        if let Some(anim) = &self.anim {
            if anim.tween.is_done(now) {
                drop_anim = true;
            } else if let AnimKind::Enter(id) = anim.kind {
                if !self.queue.visible().first().is_some_and(|n| n.id == id) {
                    drop_anim = true;
                }
            }
        }
        if drop_anim {
            self.set_anim(None);
        }

        // 2. Queue empty and no leave animation → destroy the surface.
        let exit_in_flight = matches!(&self.anim, Some(a) if matches!(a.kind, AnimKind::Exit(_)));
        if self.queue.is_empty() && !exit_in_flight {
            if self.notif.is_some() {
                self.destroy_notif_surface();
            }
            return;
        }

        // 3. Make sure the surface exists.
        self.ensure_notif_surface();
        let Some(output_w) = self.notif_output_width() else { return };

        // 4. Geometry (independent of whether the surface is configured; uses the bar width).
        let (island, rects) = self.notif_geometry(output_w);
        // The enter animation's origin must follow the latest bar layout (the clock ticking / a bar width change moves the right cluster).
        // A leave animation's start is the slot at the moment of closing and must not drift with re-layout, so only Enter is changed here.
        if let Some(anim) = self.anim.as_mut() {
            if matches!(&anim.kind, AnimKind::Enter(_)) {
                anim.start = island;
            }
        }
        let visible = self.queue.visible();

        // 5. Draw.
        let Some(notif) = self.notif.as_mut() else { return };
        if notif.width == 0 {
            return; // wait for the configure callback
        }
        let w = notif.width as i32;
        let h = notif.height as i32;
        let (buffer, canvas) = notif.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).expect("failed to create the notification buffer");
        let mut c = crate::canvas::Canvas::new(canvas, w, h);
        c.clear();

        let mut hits: Vec<(Rect, Action)> = Vec::new();

        // Enter animation: the head card grows out of the island.
        let mut enter: Option<(Rect, f32)> = None;
        if let Some(anim) = &self.anim {
            if let AnimKind::Enter(id) = anim.kind {
                if visible.first().is_some_and(|n| n.id == id) {
                    let t = anim.tween.progress(now);
                    let rect = crate::notify::view::island_enter_rect(anim.start, anim.end, &anim.tween, now);
                    let alpha = ((t - 0.35) / 0.65).clamp(0.0, 1.0);
                    enter = Some((rect, alpha));
                }
            }
        }

        for (i, n) in visible.iter().enumerate() {
            let static_rect = *rects.get(i).unwrap_or(&island);
            let (rect, alpha) = if i == 0 {
                if let Some((r, a)) = enter { (r, a) } else { (static_rect, 1.0) }
            } else {
                (static_rect, 1.0)
            };
            if rect.bottom() > h {
                break; // defensive: the capacity estimate should already keep this in bounds
            }
            let mut card_hits = crate::notify::view::render(&mut c, rect, self.theme.radius, alpha, n, &self.theme, &mut self.text);
            hits.append(&mut card_hits); // buttons first
            hits.push((rect, Action::NotificationClose(n.id))); // card body after
        }

        // Leave animation: the closed card shrinks back (its content is gone from the queue, so a snapshot is drawn).
        if let Some(anim) = &self.anim {
            if let AnimKind::Exit(snapshot) = &anim.kind {
                let rect = crate::notify::view::island_exit_rect(anim.start, anim.end, &anim.tween, now);
                if rect.bottom() <= h {
                    let mut card_hits = crate::notify::view::render(&mut c, rect, self.theme.radius, 1.0, snapshot, &self.theme, &mut self.text);
                    hits.append(&mut card_hits);
                    hits.push((rect, Action::NotificationClose(snapshot.id)));
                }
            }
        }

        // Input region: always Some (an empty region responds to nothing; None makes the whole surface swallow clicks).
        let region = Region::new(&self.compositor).expect("failed to create the input region");
        for (r, _) in &hits {
            if !r.is_empty() {
                region.add(r.x, r.y, r.w, r.h);
            }
        }
        notif.layer.set_input_region(Some(region.wl_region()));

        notif.layer.wl_surface().damage_buffer(0, 0, w, h);
        buffer.attach_to(notif.layer.wl_surface()).expect("attach failed");
        notif.layer.commit();

        self.hits = hits;
    }

    fn redraw(&mut self, output: &wl_output::WlOutput) {
        let qh = self.qh.clone();
        let theme = &self.theme;
        let text = &mut self.text;
        let sections = &mut self.sections;
        if let Some(bar) = self.bars.get_mut(output) {
            draw_bar(bar, &qh, theme, text, sections);
        }
    }
}

/// Draw the whole bar as the left/center/right layout.
fn draw_bar(bar: &mut Bar, _qh: &QueueHandle<State>, theme: &crate::theme::Theme, text: &mut crate::text::TextEngine, sections: &mut crate::bar::Sections) {
    // No buffer may be attached before the first configure: wlroots treats "a buffer before configure" as a protocol error
    // (final review Critical #3). The configure callback sets configured = true before reaching here, so this
    // only stops paths like "the exec module emits before configure → redraw_all".
    if !bar.configured {
        return;
    }
    let w = bar.width.max(1) as i32;
    let h = bar.height.max(1) as i32;
    let widths = sections.widths(text, theme);
    let l = crate::bar::layout(&widths, w, theme);
    let (buffer, canvas) = bar.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).expect("failed to create the buffer");
    let mut c = crate::canvas::Canvas::new(canvas, w, h);
    c.clear();
    c.fill_rounded_rect(crate::geom::Rect::new(0, 0, w, h), theme.radius, theme.background);
    let rows = [
        (&l.left[..], &mut sections.left[..]),
        (&l.center[..], &mut sections.center[..]),
        (&l.right[..], &mut sections.right[..]),
    ];
    // One baseline for the whole bar: all modules share the style, so the cap-height centre is the same.
    let top = text.optical_top(&theme.font, 0, theme.height);
    for (rects, modules) in rows {
        for (r, m) in rects.iter().zip(modules.iter_mut()) {
            if r.is_empty() { continue; }
            let spans = crate::bar::fit_text(&m.spans(), r.w, text, theme);
            let mut x = r.x;
            for s in spans {
                let color = s.color.unwrap_or(theme.foreground);
                text.draw(&mut c, &s.text, x, top, &theme.font, color);
                x += text.measure(&s.text, &theme.font).0.ceil() as i32;
            }
        }
    }
    bar.layer.wl_surface().damage_buffer(0, 0, w, h);
    buffer.attach_to(bar.layer.wl_surface()).expect("attach failed");
    bar.layer.commit();
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor closing the notification surface (e.g. its output was removed) ≠ process exit.
        if self.notif.as_ref().is_some_and(|n| n.layer.wl_surface() == layer.wl_surface()) {
            // The compositor closed the notification surface (usually because its output was removed).
            // Clear the state before redrawing: ensure_notif_surface falls back to a remaining output when the queue is non-empty,
            // otherwise a resident notification with expire=None would stay invisible forever.
            self.notif = None;
            self.hits.clear();
            self.set_anim(None);
            self.notif_output = None;
            self.redraw_notifications(Instant::now());
            return;
        }
        // Bar closed → process exit. Only exit when it really matches a bar: when an output is removed we have already
        // output_destroyed tears down the notification surface itself; a stale closed arriving afterwards belongs to no bar and is ignored.
        if self.bars.values().any(|b| b.layer.wl_surface() == layer.wl_surface()) {
            self.exit = true;
        }
    }

    fn configure(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        // The notification surface's configure.
        if self.notif.as_ref().is_some_and(|n| n.layer.wl_surface() == layer.wl_surface()) {
            if let Some(notif) = self.notif.as_mut() {
                if let Some(w) = NonZeroU32::new(configure.new_size.0) {
                    notif.width = w.get();
                }
                if let Some(h) = NonZeroU32::new(configure.new_size.1) {
                    notif.height = h.get();
                }
                if !notif.configured {
                    notif.configured = true;
                    eprintln!("notif surface configured: {}x{}", notif.width, notif.height);
                }
            }
            let now = Instant::now();
            self.redraw_notifications(now);
            return;
        }
        // Bar surface: find the output for that layer surface, then let `redraw` run layout and drawing uniformly
        let Some(output) = self.bars.iter().find(|(_, b)| b.layer.wl_surface() == layer.wl_surface()).map(|(o, _)| o.clone()) else {
            return;
        };
        if let Some(bar) = self.bars.get_mut(&output) {
            if let Some(w) = NonZeroU32::new(configure.new_size.0) {
                bar.width = w.get();
            }
            if let Some(h) = NonZeroU32::new(configure.new_size.1) {
                bar.height = h.get();
            }
            if !bar.configured {
                bar.configured = true;
                eprintln!("bar configured: {}x{}", bar.width, bar.height);
            }
        }
        self.redraw(&output);
        // A notification may arrive before the first output configures (ensure_notif_surface was a no-op then).
        // One catch-up pass once the bar appears: no-op on an empty queue, otherwise create/draw the surface (final review Important #3).
        self.redraw_notifications(Instant::now());
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) { self.add_bar(output); }
    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.bars.remove(&output);
        // The notification surface's output is gone: do not depend on the order of closed events — tear it down and fall back to
        // The remaining output. Clear state before redrawing — redraw_notifications is a no-op when the queue is empty.
        if self.notif_output.as_ref() == Some(&output) {
            self.notif = None;
            self.hits.clear();
            self.set_anim(None);
            self.notif_output = None;
            self.redraw_notifications(Instant::now());
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _conn: &Connection, _: &QueueHandle<Self>, _seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() { p.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        // Only clicks on the notification surface are handled; the bar surface does not respond.
        let Some(notif_surface) = self.notif.as_ref().map(|n| n.layer.wl_surface().clone()) else { return };
        let mut closed: Option<u32> = None;
        for ev in events {
            if &ev.surface != &notif_surface {
                continue;
            }
            let PointerEventKind::Press { button, .. } = ev.kind else { continue };
            let x = ev.position.0 as i32;
            let y = ev.position.1 as i32;
            match crate::notify::view::hit(&self.hits, x, y) {
                // The button wins: a left click triggers the action and closes.
                Some(Action::NotificationAction { id, key }) => {
                    if button == BTN_LEFT {
                        if let Some(c) = &self.dbus {
                            let _ = crate::notify::service::emit_action(c, id, &key);
                        }
                        closed = Some(id);
                    }
                }
                // Card body: left/middle click closes.
                Some(Action::NotificationClose(id)) => {
                    if button == BTN_LEFT || button == BTN_MIDDLE {
                        closed = Some(id);
                    }
                }
                None => {}
            }
        }
        if let Some(id) = closed {
            let now = Instant::now();
            self.close_visible(id, 2, now);
            self.redraw_notifications(now);
        }
    }
}

// No frame callback is needed: redraws are driven by a calloop timer and a callback would add no extra cadence information.
impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn animating_asks_for_next_frame() {
        let t = Instant::now();
        assert_eq!(next_deadline(t, true, None), t + FRAME_INTERVAL);
    }

    #[test]
    fn idle_waits_for_the_clock_tick() {
        let t = Instant::now();
        assert_eq!(next_deadline(t, false, None), t + CLOCK_INTERVAL);
    }

    #[test]
    fn earliest_deadline_wins() {
        let t = Instant::now();
        // Expiry earlier than the next frame → use the expiry
        let soon = t + Duration::from_millis(5);
        assert_eq!(next_deadline(t, true, Some(soon)), soon);
        let later = t + Duration::from_secs(30);
        assert_eq!(next_deadline(t, true, Some(later)), t + FRAME_INTERVAL);
        // and an expired time is never returned as a past instant either
        assert_eq!(next_deadline(t, false, Some(t - Duration::from_secs(1))), t);
    }
}
