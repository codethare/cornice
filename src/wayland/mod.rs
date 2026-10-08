//! Wayland connection, layer surfaces, shm buffers and the main event loop.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use calloop::{timer::{TimeoutAction, Timer}, EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData, Region},
    delegate_registry,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
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
    globals::{registry_queue_init, GlobalList},
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use smithay_client_toolkit::reexports::protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
// Re-exported by sctk, so no extra crate: the deprecated wlroots window list is the only place river tells a
// normal client which toplevel is focused (`river/Window.zig` sends `setAppId` and `setActivated` on it).
use smithay_client_toolkit::reexports::protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as wlr_handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as wlr_manager, ZwlrForeignToplevelManagerV1},
};
use wayland_client::backend::ObjectId;

use crate::config::NotificationPosition;
use crate::geom::Rect;
use crate::notify::queue::Notification;
use crate::notify::view::{Card, Motion};
use crate::widget::{Action, ControlRequest, Event, ModuleActions, Toplevel};

pub const LAYER_NAMESPACE: &str = "cornice";
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(1);
/// Bar repaints are coalesced to this period: a module that prints faster than this cannot turn into a
/// repaint per line, and a per-second clock still repaints immediately.
pub const BAR_REDRAW_INTERVAL: Duration = Duration::from_millis(50);
/// Notification frames the compositor may still be reading before the next one is rasterised. Without
/// this the 16 ms timer keeps allocating buffers for a compositor that is slower than the timer.
pub const MAX_PENDING_FRAMES: u8 = 2;

/// `Some(delay)` when a repaint still has to wait, `None` when it can happen now.
pub fn coalesce(now: Instant, last: Instant, interval: Duration) -> Option<Duration> {
    let elapsed = now.saturating_duration_since(last);
    (elapsed < interval).then(|| interval - elapsed)
}

/// The next wake the bar needs, or `None` when it needs none: no clock module and no pending expiry
/// must not leave a timer ticking once a second for nothing.
pub fn next_wake(now: Instant, clock: bool, next_expiry: Option<Instant>) -> Option<Instant> {
    let tick = clock.then(|| now + CLOCK_INTERVAL);
    let expiry = next_expiry.map(|expiry| expiry.max(now));
    match (tick, expiry) {
        (Some(tick), Some(expiry)) => Some(tick.min(expiry)),
        (tick, expiry) => tick.or(expiry),
    }
}

pub struct Bar {
    pub layer: LayerSurface,
    /// Created on the first draw: sizing it eagerly reserved a slot for every output before anything was known
    /// about the output's width.
    pub pool: Option<SlotPool>,
    pub width: u32,
    pub height: u32,
    pub configured: bool,
    /// Click and wheel targets in surface coordinates, from the last draw.
    pub hits: Vec<(Rect, ModuleActions)>,
}

pub struct NotifSurface {
    pub layer: LayerSurface,
    pub pool: SlotPool,
    pub width: u32,
    pub height: u32,
    pub configured: bool,
    pub output: Option<wl_output::WlOutput>,
    pub notification: Notification,
    pub card: Card,
    pub motion: Motion,
    pub hits: Vec<(Rect, Action)>,
    /// Frames committed but not presented yet; the `wl_surface.frame` callback only reports that number, so it
    /// is not stored.
    pending_frames: u8,
}

/// One mapped toplevel as the wlroots window list reports it.
#[derive(Default)]
struct FocusHandle {
    app_id: String,
    activated: bool,
}

/// Focus tracking. `manager` is `None` when the compositor does not offer the window list, in which case the
/// bar simply has no focus information and no chip is highlighted.
#[derive(Default)]
struct Focus {
    manager: Option<ZwlrForeignToplevelManagerV1>,
    handles: HashMap<ObjectId, (ZwlrForeignToplevelHandleV1, FocusHandle)>,
    /// Last value handed to the bar modules, so an event burst does not become a message burst.
    published: Option<String>,
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
    pub animating: bool,
    pub cfg: crate::config::Config,
    pub theme: crate::theme::Theme,
    pub text: crate::text::TextEngine,
    pub sections: crate::bar::Sections,
    pub foreign_toplevel_list: ForeignToplevelList,
    pub event_tx: calloop::channel::Sender<Event>,
    pub queue: crate::notify::queue::Queue,
    pub dbus: Option<zbus::blocking::Connection>,
    pub notifications: HashMap<u32, NotifSurface>,
    /// Whether the bar is shown at all; `cornice bar hide` clears this.
    pub bar_visible: bool,
    focus: Focus,
    frame_id: Option<RegistrationToken>,
    idle_id: Option<RegistrationToken>,
    /// Deadline the idle timer registration is armed for, so a new earlier expiry can re-arm it instead of
    /// waiting for the old one.
    idle_deadline: Option<Instant>,
    /// Pending coalesced bar repaint, and when the last one happened.
    bar_redraw_id: Option<RegistrationToken>,
    bar_drawn_at: Instant,
    loop_handle: LoopHandle<'static, State>,
}

pub fn run(cfg: crate::config::Config) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|error| format!("cannot connect to Wayland: {error}"))?;
    let (globals, event_queue) = registry_queue_init(&connection).map_err(|error| format!("failed to read globals: {error}"))?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).map_err(|error| format!("missing wl_compositor: {error}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|error| {
        format!("layer shell unavailable: {error}\n  on river the WM must implement river-layer-shell-v1 (tailrace does)")
    })?;
    let shm = Shm::bind(&globals, &qh).map_err(|error| format!("missing wl_shm: {error}"))?;
    let foreign_toplevel_list = ForeignToplevelList::new(&globals, &qh);

    let bar_height = cfg.bar.height as u32;
    let theme = cfg.theme.clone();
    let (sections, event_rx, event_tx) = crate::bar::Sections::from_config(&cfg);
    let max_visible = cfg.notification.as_ref().map_or(1, |notification| notification.max_visible);
    let (notification_tx, notification_rx) = calloop::channel::channel::<crate::notify::queue::Request>();
    let (control_tx, control_rx) = calloop::channel::channel::<ControlRequest>();
    let dbus = match crate::notify::service::spawn(notification_tx, control_tx) {
        Ok(connection) => Some(connection),
        Err(error) => {
            eprintln!("cornice: notifications unavailable: {error}");
            None
        }
    };
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().map_err(|error| format!("failed to create the event loop: {error}"))?;
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
        foreign_toplevel_list,
        event_tx,
        queue: crate::notify::queue::Queue::new(max_visible),
        dbus,
        notifications: HashMap::new(),
        bar_visible: true,
        focus: Focus::default(),
        frame_id: None,
        idle_id: None,
        idle_deadline: None,
        bar_redraw_id: None,
        bar_drawn_at: Instant::now(),
        loop_handle: handle.clone(),
    };

    state.bind_focus(&globals, &qh);

    WaylandSource::new(connection, event_queue).insert(handle.clone()).map_err(|error| format!("failed to insert the wayland source: {error}"))?;
    handle.insert_source(event_rx, |message, _metadata, state: &mut State| {
        let calloop::channel::Event::Msg(event) = message else { return };
        if state.sections.update(&event) { state.bar_changed(Instant::now()); }
    }).map_err(|error| format!("failed to insert the exec channel: {error}"))?;
    handle.insert_source(control_rx, |message, _metadata, state: &mut State| {
        let calloop::channel::Event::Msg(request) = message else { return };
        let visible = match request {
            ControlRequest::ToggleBar => !state.bar_visible,
            ControlRequest::SetBarVisible(visible) => visible,
        };
        state.set_bar_visible(visible);
    }).map_err(|error| format!("failed to insert the control channel: {error}"))?;
    handle.insert_source(notification_rx, |message, _metadata, state: &mut State| {
        let calloop::channel::Event::Msg(request) = message else { return };
        let now = Instant::now();
        let changed = match state.queue.apply(request, now) {
            crate::notify::queue::Outcome::Added(_) | crate::notify::queue::Outcome::Replaced(_) => true,
            crate::notify::queue::Outcome::CloseRequested(id) => state.remove_notification(id, 3),
            crate::notify::queue::Outcome::ClosedAll(ids) => {
                let mut changed = false;
                for id in ids { changed |= state.remove_notification(id, 3); }
                changed
            }
            crate::notify::queue::Outcome::Ignored => false,
        };
        // A new entry may be the first thing that needs a timer since startup.
        state.ensure_idle_timer();
        if changed {
            state.sync_notifications(now);
            state.redraw_notifications(now);
        }
    }).map_err(|error| format!("failed to insert the notification channel: {error}"))?;

    state.ensure_idle_timer();

    while !state.exit {
        event_loop.dispatch(None, &mut state).map_err(|error| format!("event loop error: {error}"))?;
    }
    Ok(())
}

impl State {
    fn add_bar(&mut self, output: wl_output::WlOutput) {
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some(LAYER_NAMESPACE), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        layer.set_margin(self.cfg.bar.margin, 0, 0, 0);
        layer.set_exclusive_zone(self.bar_height as i32 + 2 * self.cfg.bar.margin);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, self.bar_height);
        layer.commit();
        self.bars.insert(output, Bar { layer, pool: None, width: 0, height: self.bar_height, configured: false, hits: Vec::new() });
    }

    /// Reconcile the bars with the configured outputs and the current visibility. Called for output
    /// add/update, for `cornice bar show|hide`, and at startup.
    fn sync_bars(&mut self) {
        for output in self.output_state.outputs() {
            let wanted = self.bar_visible && self.output_allowed(&output);
            match (self.bars.contains_key(&output), wanted) {
                (true, false) => {
                    self.bars.remove(&output);
                }
                (false, true) => self.add_bar(output),
                _ => {}
            }
        }
    }

    fn output_allowed(&self, output: &wl_output::WlOutput) -> bool {
        let name = self.output_state.info(output).and_then(|info| info.name);
        crate::config::output_matches(&self.cfg.bar.outputs, name.as_deref())
    }

    fn set_bar_visible(&mut self, visible: bool) {
        if self.bar_visible == visible {
            return;
        }
        self.bar_visible = visible;
        self.sync_bars();
    }

    /// Arm the idle timer when anything still needs a wakeup, and drop it when nothing does: an idle bar
    /// without a clock module does not wake the process every second. Re-arming only happens when the deadline
    /// actually changes, so the steady state is the callback rescheduling itself.
    fn ensure_idle_timer(&mut self) {
        let now = Instant::now();
        let deadline = next_wake(now, self.sections.has_clock, self.queue.next_expiry());
        if deadline == self.idle_deadline {
            return;
        }
        if let Some(token) = self.idle_id.take() {
            self.loop_handle.remove(token);
        }
        self.idle_deadline = deadline;
        let Some(deadline) = deadline else { return };
        let token = self.loop_handle.insert_source(Timer::from_deadline(deadline), |_deadline, _metadata, state: &mut State| {
            let now = Instant::now();
            state.on_wake(now);
            state.wake_deadline(now)
        });
        if let Ok(token) = token {
            self.idle_id = Some(token);
        }
    }

    /// Timer callback tail: either the next deadline, or drop the source and forget its token.
    fn wake_deadline(&mut self, now: Instant) -> TimeoutAction {
        let deadline = next_wake(now, self.sections.has_clock, self.queue.next_expiry());
        self.idle_deadline = deadline;
        match deadline {
            Some(deadline) => TimeoutAction::ToInstant(deadline),
            None => {
                self.idle_id = None;
                TimeoutAction::Drop
            }
        }
    }

    /// Ask for a bar repaint, coalesced to one per `BAR_REDRAW_INTERVAL`.
    fn bar_changed(&mut self, now: Instant) {
        match coalesce(now, self.bar_drawn_at, BAR_REDRAW_INTERVAL) {
            None => {
                self.bar_drawn_at = now;
                self.redraw_all();
            }
            Some(delay) if self.bar_redraw_id.is_none() => {
                let token = self.loop_handle.insert_source(Timer::from_duration(delay), |_deadline, _metadata, state: &mut State| {
                    state.bar_redraw_id = None;
                    state.bar_drawn_at = Instant::now();
                    state.redraw_all();
                    TimeoutAction::Drop
                });
                if let Ok(token) = token {
                    self.bar_redraw_id = Some(token);
                }
            }
            Some(_) => {}
        }
    }

    /// Bind the compositor's window list when it offers one. Its absence is not an error: the bar then simply
    /// has no focused application.
    fn bind_focus(&mut self, globals: &GlobalList, qh: &QueueHandle<State>) {
        match globals.bind::<ZwlrForeignToplevelManagerV1, State, ()>(qh, 1..=3, ()) {
            Ok(manager) => self.focus.manager = Some(manager),
            Err(error) => eprintln!("cornice: no focused-window information ({error})"),
        }
    }

    fn focus_state(&mut self, handle: &ZwlrForeignToplevelHandleV1) -> Option<&mut FocusHandle> {
        self.focus.handles.get_mut(&handle.id()).map(|(_, info)| info)
    }

    /// Hand the focused `app_id` to the bar modules, but only when it actually changed. With no window list there
    /// is no focus to report, so nothing is sent at all.
    fn publish_focus(&mut self) {
        if self.focus.manager.is_none() {
            return;
        }
        let focused = self
            .focus
            .handles
            .values()
            .find(|(_, info)| info.activated)
            .map(|(_, info)| info.app_id.clone())
            .filter(|app_id| !app_id.is_empty());
        if focused == self.focus.published {
            return;
        }
        self.focus.published = focused.clone();
        let _ = self.event_tx.send(Event::FocusedApp(focused));
    }

    pub fn redraw_all(&mut self) {
        let outputs: Vec<_> = self.bars.keys().cloned().collect();
        for output in outputs { self.redraw(&output); }
    }

    /// Snapshot the compositor's toplevels and hand them to the bar modules without holding any Wayland type there.
    /// `exclude` is the handle sctk has not removed from its list yet when `toplevel_closed` runs.
    fn publish_toplevels_excluding(&mut self, exclude: Option<&ExtForeignToplevelHandleV1>) {
        let toplevels: Vec<Toplevel> = self
            .foreign_toplevel_list
            .toplevels()
            .iter()
            .filter(|handle| exclude != Some(*handle))
            .filter_map(|handle| self.foreign_toplevel_list.info(handle))
            .map(|info| Toplevel { app_id: info.app_id })
            .collect();
        let _ = self.event_tx.send(Event::Toplevels(toplevels));
    }

    fn publish_toplevels(&mut self) { self.publish_toplevels_excluding(None); }

    fn notification_settings(&self) -> Option<(NotificationPosition, u64, u64)> {
        self.cfg.notification.as_ref().map(|notification| (notification.position, notification.enter_ms, notification.exit_ms))
    }

    fn create_notification_surface(&mut self, card: Card, notification: Notification, now: Instant, enter_ms: u64) {
        let Some((position, _, _)) = self.notification_settings() else { return };
        let Ok(pool) = SlotPool::new(card.rect.w as usize * card.rect.h as usize * 4, &self.shm) else {
            eprintln!("cornice: cannot create the notification buffer pool");
            return;
        };
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let layer = self.layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(LAYER_NAMESPACE), None);
        let anchor = match position {
            NotificationPosition::Left => Anchor::TOP | Anchor::LEFT,
            NotificationPosition::Center => Anchor::TOP,
            NotificationPosition::Right => Anchor::TOP | Anchor::RIGHT,
        };
        let side = crate::notify::view::side_margin(&self.theme);
        layer.set_anchor(anchor);
        layer.set_margin(
            crate::notify::view::surface_top(&self.theme, self.cfg.bar.margin, card.top),
            if position == NotificationPosition::Right { side } else { 0 },
            0,
            if position == NotificationPosition::Left { side } else { 0 },
        );
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(card.rect.w as u32, card.rect.h as u32);
        layer.commit();
        let motion = Motion::new(now, card.top, enter_ms);
        self.notifications.insert(notification.id, NotifSurface {
            layer,
            pool,
            width: 0,
            height: 0,
            configured: false,
            output: None,
            notification,
            card,
            motion,
            hits: Vec::new(),
            pending_frames: 0,
        });
    }

    /// Reconcile protocol surfaces with queue state. Live cards get their content and target top; cards that
    /// left the queue keep a private copy long enough to animate out.
    fn sync_notifications(&mut self, now: Instant) -> bool {
        let Some((_, enter_ms, exit_ms)) = self.notification_settings() else {
            let changed = !self.notifications.is_empty();
            self.notifications.clear();
            self.animating = false;
            return changed;
        };
        let mut changed = false;

        for surface in self.notifications.values_mut() {
            let live = self.queue.visible().iter().any(|notification| notification.id == surface.card.id);
            if !live && !surface.motion.is_exiting() {
                surface.motion.begin_exit(now, exit_ms);
                changed = true;
            }
        }

        let live_cards = crate::notify::view::cards(self.queue.visible(), &self.theme);
        let mut missing = Vec::new();
        for card in live_cards {
            let Some(notification) = self.queue.visible().iter().find(|notification| notification.id == card.id) else { continue };
            if self.notifications.contains_key(&card.id) {
                let resize = self.notifications.get(&card.id).is_some_and(|surface| surface.card.rect != card.rect);
                let pool = if resize {
                    match SlotPool::new(card.rect.w as usize * card.rect.h as usize * 4, &self.shm) {
                        Ok(pool) => Some(pool),
                        Err(error) => {
                            eprintln!("cornice: cannot resize the notification buffer pool: {error}");
                            None
                        }
                    }
                } else {
                    None
                };
                if resize && pool.is_none() {
                    continue;
                }
                if let Some(surface) = self.notifications.get_mut(&card.id) {
                    changed |= surface.notification != *notification;
                    surface.notification = notification.clone();
                    surface.card = card;
                    if let Some(pool) = pool {
                        surface.configured = false;
                        surface.width = 0;
                        surface.height = 0;
                        surface.hits.clear();
                        surface.pool = pool;
                        surface.layer.set_size(surface.card.rect.w as u32, surface.card.rect.h as u32);
                        surface.layer.commit();
                        changed = true;
                    }
                    changed |= surface.motion.retarget(now, surface.card.top, enter_ms);
                }
            } else {
                missing.push((card, notification.clone()));
            }
        }
        for (card, notification) in missing {
            self.create_notification_surface(card, notification, now, enter_ms);
            changed = true;
        }

        let completed: Vec<_> = self.notifications.iter()
            .filter(|(_, surface)| surface.motion.is_exiting() && !surface.motion.is_animating(now))
            .map(|(id, _)| *id)
            .collect();
        for id in completed {
            self.notifications.remove(&id);
            changed = true;
        }

        self.animating = self.notifications.values().any(|surface| surface.motion.is_animating(now));
        if self.animating { self.ensure_frame_source(); }
        changed
    }

    fn ensure_frame_source(&mut self) {
        if self.frame_id.is_some() || !self.animating { return; }
        let token = self.loop_handle.insert_source(
            Timer::from_duration(FRAME_INTERVAL),
            |_deadline, _metadata, state: &mut State| {
                let now = Instant::now();
                state.on_wake(now);
                if state.animating {
                    TimeoutAction::ToInstant(now + FRAME_INTERVAL)
                } else {
                    state.frame_id = None;
                    TimeoutAction::Drop
                }
            },
        );
        if let Ok(token) = token { self.frame_id = Some(token); }
    }

    fn on_wake(&mut self, now: Instant) {
        let mut changed = false;
        let visible_expired: Vec<_> = self.queue.visible().iter()
            .filter(|notification| notification.expire.is_some_and(|duration| now.saturating_duration_since(notification.created) >= duration))
            .map(|notification| notification.id)
            .collect();
        for id in visible_expired { changed |= self.remove_notification(id, 1); }
        for id in self.queue.expire(now) {
            if let Some(connection) = &self.dbus { let _ = crate::notify::service::emit_closed(connection, id, 1); }
        }
        if self.sections.update(&crate::widget::Event::Wake) { self.bar_changed(now); }
        changed |= self.sync_notifications(now);
        if changed || self.animating { self.redraw_notifications(now); }
    }

    fn remove_notification(&mut self, id: u32, reason: u32) -> bool {
        if self.queue.remove(id).is_none() { return false; }
        if let Some(connection) = &self.dbus { let _ = crate::notify::service::emit_closed(connection, id, reason); }
        true
    }

    fn redraw_notifications(&mut self, now: Instant) {
        let Some((_, _, _)) = self.notification_settings() else { return };
        if self.notifications.is_empty() { return; }
        let qh = self.qh.clone();
        let side = crate::notify::view::side_margin(&self.theme);
        let position = self.notification_settings().map(|settings| settings.0).expect("settings exist");

        for notification in self.notifications.values_mut() {
            let top = crate::notify::view::surface_top(&self.theme, self.cfg.bar.margin, notification.motion.top(now));
            notification.layer.set_margin(
                top,
                if position == NotificationPosition::Right { side } else { 0 },
                0,
                if position == NotificationPosition::Left { side } else { 0 },
            );
            // The compositor is the clock: while it still holds `MAX_PENDING_FRAMES` un-presented frames,
            // rasterising another one only grows the buffer pool. The last frame of an animation is always
            // drawn, so a card can never get stuck half-faded.
            if notification.pending_frames >= MAX_PENDING_FRAMES && notification.motion.is_animating(now) {
                continue;
            }
            if !notification.configured || notification.width == 0 || notification.height == 0 { continue; }

            let width = notification.width as i32;
            let height = notification.height as i32;
            let visual = notification.motion.visual(now, notification.card.rect.w, notification.card.rect.h);
            let Ok((buffer, data)) = notification.pool.create_buffer(width, height, width * 4, wl_shm::Format::Argb8888) else {
                eprintln!("cornice: cannot create the notification buffer");
                continue;
            };
            let mut canvas = crate::canvas::Canvas::new(data, width, height);
            canvas.clear();
            canvas.set_clip(Some(visual.rect));
            crate::notify::view::render_background(&mut canvas, visual, &self.theme);
            let card_hits = crate::notify::view::render(&mut canvas, &notification.card, visual.alpha, &notification.notification, &self.theme, &mut self.text);
            canvas.set_clip(None);

            let mut hits = Vec::new();
            if !notification.motion.is_exiting() && visual.alpha > 0.0 {
                for (rect, action) in card_hits {
                    let visible = rect.intersect(visual.rect);
                    if !visible.is_empty() { hits.push((visible, action)); }
                }
                hits.push((visual.rect, Action::NotificationBody(notification.card.id)));
            }

            match Region::new(&self.compositor) {
                Ok(region) => {
                    for (rect, _) in &hits { region.add(rect.x, rect.y, rect.w, rect.h); }
                    notification.layer.set_input_region(Some(region.wl_region()));
                }
                Err(error) => eprintln!("cornice: cannot create the notification input region: {error}"),
            }
            // Requested before the commit so the callback reports this frame; it is also what releases the
            // next one (see `pending_frames`).
            let surface = notification.layer.wl_surface().clone();
            let _ = surface.frame(&qh, FrameCallbackData(surface.clone()));
            notification.pending_frames = notification.pending_frames.saturating_add(1);
            notification.layer.wl_surface().damage_buffer(0, 0, width, height);
            if let Err(error) = buffer.attach_to(notification.layer.wl_surface()) {
                eprintln!("cornice: cannot attach the notification buffer: {error}");
                continue;
            }
            notification.layer.commit();
            notification.hits = hits;
        }
    }

    fn redraw(&mut self, output: &wl_output::WlOutput) {
        let State { bars, compositor, shm, theme, text, sections, .. } = self;
        if let Some(bar) = bars.get_mut(output) {
            draw_bar(bar, compositor, shm, theme, text, sections);
        }
    }
}

fn draw_bar(bar: &mut Bar, compositor: &CompositorState, shm: &Shm, theme: &crate::theme::Theme, text: &mut crate::text::TextEngine, sections: &mut crate::bar::Sections) {
    if !bar.configured { return; }
    let width = bar.width.max(1) as i32;
    let height = bar.height.max(1) as i32;
    if bar.pool.is_none() {
        // One slot per configured buffer; the pool grows on its own if a repaint overlaps the previous one.
        match SlotPool::new((width as usize) * (height as usize) * 4, shm) {
            Ok(pool) => bar.pool = Some(pool),
            Err(error) => {
                eprintln!("cornice: cannot create the bar buffer pool: {error}");
                return;
            }
        }
    }
    let widths = sections.widths(text, theme);
    let layout = crate::bar::layout(&widths, width, theme);
    let Some(pool) = bar.pool.as_mut() else { return };
    let Ok((buffer, data)) = pool.create_buffer(width, height, width * 4, wl_shm::Format::Argb8888) else {
        eprintln!("cornice: cannot create the bar buffer");
        return;
    };
    let mut canvas = crate::canvas::Canvas::new(data, width, height);
    canvas.clear();
    canvas.fill_rect(Rect::new(0, 0, width, height), theme.bar_background());
    let top = text.optical_top(&theme.font, 0, theme.height);
    bar.hits.clear();
    let rows = [
        (&layout.left[..], &mut sections.left[..], &sections.left_actions[..]),
        (&layout.center[..], &mut sections.center[..], &sections.center_actions[..]),
        (&layout.right[..], &mut sections.right[..], &sections.right_actions[..]),
    ];
    for (rects, modules, actions) in rows {
        for ((rect, module), actions) in rects.iter().zip(modules.iter_mut()).zip(actions.iter()) {
            if rect.is_empty() { continue; }
            // The whole module is the click target, including the gaps between its spans.
            if actions.any() { bar.hits.push((*rect, actions.clone())); }
            let spans = crate::bar::fit_text(&module.spans(), rect.w, text, theme);
            let mut x = rect.x;
            for (index, span) in spans.iter().enumerate() {
                if index > 0 { x += theme.spacing.max(0); }
                if let Some(count) = span.badge {
                    let icon = theme.app_icon.min(rect.h).max(1);
                    let chip = Rect::new(x, rect.y + (rect.h - icon) / 2, icon, icon);
                    let (background, color) = if span.focused {
                        (theme.app_icon_focused_background(), theme.accent)
                    } else {
                        (theme.app_icon_background(), span.color.unwrap_or(theme.foreground))
                    };
                    canvas.fill_rounded_rect(chip, theme.app_icon / 4, background);
                    let (label_width, _) = text.measure(&span.text, &theme.font);
                    let label_x = chip.x + ((chip.w - label_width.ceil() as i32) / 2).max(0);
                    let label_y = text.optical_top(&theme.font, chip.y, chip.h);
                    text.draw(&mut canvas, &span.text, label_x, label_y, &theme.font, color);
                    if count > 1 {
                        let style = theme.app_badge_style();
                        let label = text.fit_text(&count.to_string(), &style, (chip.w - 2).max(1) as f32);
                        let (badge_width, _) = text.measure(&label, &style);
                        let badge_x = chip.right() - badge_width.ceil() as i32;
                        let metrics = text.cap_metrics(&style);
                        let badge_y = chip.bottom() - (metrics.top + metrics.cap).round() as i32;
                        text.draw(&mut canvas, &label, badge_x, badge_y, &style, theme.accent);
                    }
                    x += icon;
                } else {
                    let color = span.color.unwrap_or(theme.foreground);
                    text.draw(&mut canvas, &span.text, x, top, &theme.font, color);
                    x += text.measure(&span.text, &theme.font).0.ceil() as i32;
                }
            }
        }
    }
    // Same data as the hit rects: the bar swallows clicks only where a module actually listens, so empty
    // bar space stays click-through. Never `None` — that would be an infinite region.
    match Region::new(compositor) {
        Ok(region) => {
            for (rect, _) in &bar.hits { region.add(rect.x, rect.y, rect.w, rect.h); }
            bar.layer.set_input_region(Some(region.wl_region()));
        }
        Err(error) => eprintln!("cornice: cannot create the bar input region: {error}"),
    }
    bar.layer.wl_surface().damage_buffer(0, 0, width, height);
    if let Err(error) = buffer.attach_to(bar.layer.wl_surface()) {
        eprintln!("cornice: cannot attach the bar buffer: {error}");
        return;
    }
    bar.layer.commit();
}

impl LayerShellHandler for State {
    fn closed(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        let closed_notification = self.notifications.iter().find(|(_, surface)| surface.layer.wl_surface() == layer.wl_surface()).map(|(id, _)| *id);
        if let Some(id) = closed_notification {
            self.notifications.remove(&id);
            let now = Instant::now();
            self.sync_notifications(now);
            self.redraw_notifications(now);
            return;
        }
        if self.bars.values().any(|bar| bar.layer.wl_surface() == layer.wl_surface()) { self.exit = true; }
    }

    fn configure(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        let notification_id = self.notifications.iter().find(|(_, surface)| surface.layer.wl_surface() == layer.wl_surface()).map(|(id, _)| *id);
        if let Some(id) = notification_id {
            if let Some(surface) = self.notifications.get_mut(&id) {
                if let Some(width) = NonZeroU32::new(configure.new_size.0) { surface.width = width.get(); }
                if let Some(height) = NonZeroU32::new(configure.new_size.1) { surface.height = height.get(); }
                surface.configured = true;
            }
            self.redraw_notifications(Instant::now());
            return;
        }

        let Some(output) = self.bars.iter().find(|(_, bar)| bar.layer.wl_surface() == layer.wl_surface()).map(|(output, _)| output.clone()) else { return };
        if let Some(bar) = self.bars.get_mut(&output) {
            if let Some(width) = NonZeroU32::new(configure.new_size.0) { bar.width = width.get(); }
            if let Some(height) = NonZeroU32::new(configure.new_size.1) { bar.height = height.get(); }
            bar.configured = true;
        }
        self.redraw(&output);
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    // The output's name usually arrives after the global is announced, so the bars are reconciled on both
    // events: a bar for an unwanted output is created first only when the name is still unknown.
    fn new_output(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) { self.sync_bars(); }
    fn update_output(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) { self.sync_bars(); }
    fn output_destroyed(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.bars.remove(&output);
        let removed: Vec<_> = self.notifications.iter()
            .filter(|(_, surface)| surface.output.as_ref() == Some(&output))
            .map(|(id, _)| *id)
            .collect();
        for id in removed { self.notifications.remove(&id); }
        let now = Instant::now();
        self.sync_notifications(now);
        self.redraw_notifications(now);
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _connection: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() { self.pointer = self.seat_state.get_pointer(qh, &seat).ok(); }
    }
    fn remove_capability(&mut self, _connection: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && let Some(pointer) = self.pointer.take() { pointer.release(); }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, _pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mut closed = None;
        for event in events {
            let (x, y) = (event.position.0 as i32, event.position.1 as i32);
            if let Some(bar) = self.bars.values().find(|bar| bar.layer.wl_surface() == &event.surface) {
                let Some((_, actions)) = bar.hits.iter().find(|(rect, _)| rect.contains(x, y)) else { continue };
                let command = match event.kind {
                    PointerEventKind::Press { button: BTN_LEFT, .. } => actions.on_click.as_deref(),
                    // A positive value120 is the direction the content moves, i.e. a wheel notch downwards.
                    PointerEventKind::Axis { vertical, .. } if vertical.value120 > 0 => actions.on_scroll_down.as_deref(),
                    PointerEventKind::Axis { vertical, .. } if vertical.value120 < 0 => actions.on_scroll_up.as_deref(),
                    _ => None,
                };
                if let Some(command) = command { crate::bar::modules::spawn_detached(command); }
                continue;
            }
            let Some((_, surface)) = self.notifications.iter().find(|(_, surface)| surface.layer.wl_surface() == &event.surface) else { continue };
            let PointerEventKind::Press { button, .. } = event.kind else { continue };
            match crate::notify::view::hit(&surface.hits, x, y) {
                Some(Action::NotificationAction { id, key }) if button == BTN_LEFT => {
                    if let Some(connection) = &self.dbus { let _ = crate::notify::service::emit_action(connection, id, &key); }
                    closed = Some(id);
                }
                Some(Action::NotificationBody(id)) if button == BTN_LEFT || button == BTN_MIDDLE => {
                    // A client's `default` action means "the notification itself is the button"; the middle
                    // button only dismisses, which is what the card has always done.
                    let has_default = button == BTN_LEFT
                        && self.notifications.get(&id).is_some_and(|surface| crate::notify::view::has_default_action(&surface.notification));
                    if has_default && let Some(connection) = &self.dbus {
                        let _ = crate::notify::service::emit_action(connection, id, "default");
                    }
                    closed = Some(id);
                }
                _ => {}
            }
        }
        if let Some(id) = closed {
            let now = Instant::now();
            if self.remove_notification(id, 2) {
                self.sync_notifications(now);
                self.redraw_notifications(now);
            }
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        // The presented frame releases both the buffer slot and the next repaint of this surface.
        let Some(notification) = self.notifications.values_mut().find(|notification| notification.layer.wl_surface() == surface) else { return };
        notification.pending_frames = notification.pending_frames.saturating_sub(1);
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        let Some((_, notification)) = self.notifications.iter_mut().find(|(_, notification)| notification.layer.wl_surface() == surface) else { return };
        if notification.output.as_ref() != Some(output) { notification.output = Some(output.clone()); }
    }
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList { &mut self.foreign_toplevel_list }

    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) { self.publish_toplevels(); }
    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: ExtForeignToplevelHandleV1) { self.publish_toplevels(); }
    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, handle: ExtForeignToplevelHandleV1) { self.publish_toplevels_excluding(Some(&handle)); }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(state: &mut Self, _manager: &ZwlrForeignToplevelManagerV1, event: wlr_manager::Event, _data: &(), _conn: &Connection, _qh: &QueueHandle<Self>) {
        match event {
            wlr_manager::Event::Toplevel { toplevel } => {
                state.focus.handles.insert(toplevel.id(), (toplevel, FocusHandle::default()));
            }
            wlr_manager::Event::Finished => {
                state.focus.handles.clear();
                // Report the lost focus before forgetting the source, or a highlighted chip would stay lit.
                state.publish_focus();
                state.focus.manager = None;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        wlr_manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(state: &mut Self, handle: &ZwlrForeignToplevelHandleV1, event: wlr_handle::Event, _data: &(), _conn: &Connection, _qh: &QueueHandle<Self>) {
        match event {
            wlr_handle::Event::AppId { app_id } => {
                if let Some(info) = state.focus_state(handle) { info.app_id = app_id; }
            }
            // A positive value120 is the direction the content moves, i.e. a wheel notch downwards.
            wlr_handle::Event::State { state: states } => {
                let activated = states.as_chunks::<4>().0.iter().any(|value| u32::from_le_bytes(*value) == wlr_handle::State::Activated as u32);
                if let Some(info) = state.focus_state(handle) { info.activated = activated; }
            }
            wlr_handle::Event::Closed => {
                state.focus.handles.remove(&handle.id());
            }
            // Title, output membership, parent and the deprecated single-state events carry nothing the bar shows.
            _ => return,
        }
        state.publish_focus();
    }
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

    #[test]
    fn an_idle_bar_with_a_clock_wakes_once_a_second() {
        let now = Instant::now();
        assert_eq!(next_wake(now, true, None), Some(now + CLOCK_INTERVAL));
    }

    /// The point of the whole exercise: no clock, nothing pending — no timer at all.
    #[test]
    fn an_idle_bar_without_a_clock_does_not_wake() {
        let now = Instant::now();
        assert_eq!(next_wake(now, false, None), None);
    }

    #[test]
    fn the_earliest_deadline_wins_and_never_returns_the_past() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(5);
        let later = now + Duration::from_secs(30);
        assert_eq!(next_wake(now, true, Some(soon)), Some(soon));
        assert_eq!(next_wake(now, true, Some(later)), Some(now + CLOCK_INTERVAL));
        assert_eq!(next_wake(now, false, Some(later)), Some(later), "without a clock only the expiry matters");
        assert_eq!(next_wake(now, false, Some(now - Duration::from_secs(1))), Some(now));
    }

    #[test]
    fn repaints_are_coalesced_to_one_per_interval() {
        let now = Instant::now();
        let interval = Duration::from_millis(50);
        assert_eq!(coalesce(now, now - interval, interval), None, "the interval has passed, draw now");
        assert_eq!(coalesce(now, now, interval), Some(interval));
        assert_eq!(coalesce(now, now - Duration::from_millis(20), interval), Some(Duration::from_millis(30)));
        // A last draw in the future (a clock that jumped) waits a full interval instead of a negative one.
        assert_eq!(coalesce(now, now + Duration::from_secs(1), interval), Some(interval));
    }
}
