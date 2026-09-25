//! Wayland connection, layer surfaces, shm buffers and the main event loop.

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

use crate::config::NotificationPosition;
use crate::geom::Rect;
use crate::notify::queue::Notification;
use crate::notify::view::{Card, Motion};
use crate::widget::Action;

pub const LAYER_NAMESPACE: &str = "cornice";
pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub const CLOCK_INTERVAL: Duration = Duration::from_secs(1);

pub fn next_deadline(now: Instant, animating: bool, next_expiry: Option<Instant>) -> Instant {
    let mut deadline = now + CLOCK_INTERVAL;
    if animating { deadline = deadline.min(now + FRAME_INTERVAL); }
    if let Some(expiry) = next_expiry { deadline = deadline.min(expiry.max(now)); }
    deadline
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
    pub output: Option<wl_output::WlOutput>,
    pub notification: Notification,
    pub card: Card,
    pub motion: Motion,
    pub hits: Vec<(Rect, Action)>,
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
    pub queue: crate::notify::queue::Queue,
    pub dbus: Option<zbus::blocking::Connection>,
    pub notifications: HashMap<u32, NotifSurface>,
    frame_id: Option<RegistrationToken>,
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

    let bar_height = cfg.bar.height as u32;
    let theme = cfg.theme.clone();
    let (sections, exec_rx) = crate::bar::Sections::from_config(&cfg);
    let max_visible = cfg.notification.as_ref().map_or(1, |notification| notification.max_visible);
    let (notification_tx, notification_rx) = calloop::channel::channel::<crate::notify::queue::Request>();
    let dbus = match crate::notify::service::spawn(notification_tx) {
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
        queue: crate::notify::queue::Queue::new(max_visible),
        dbus,
        notifications: HashMap::new(),
        frame_id: None,
        loop_handle: handle.clone(),
    };

    WaylandSource::new(connection, event_queue).insert(handle.clone()).map_err(|error| format!("failed to insert the wayland source: {error}"))?;
    handle.insert_source(exec_rx, |message, _metadata, state: &mut State| {
        let calloop::channel::Event::Msg(event) = message else { return };
        if state.sections.update(&event) { state.redraw_all(); }
    }).map_err(|error| format!("failed to insert the exec channel: {error}"))?;
    handle.insert_source(notification_rx, |message, _metadata, state: &mut State| {
        let calloop::channel::Event::Msg(request) = message else { return };
        let now = Instant::now();
        let changed = match state.queue.apply(request, now) {
            crate::notify::queue::Outcome::Added(_) | crate::notify::queue::Outcome::Replaced(_) => true,
            crate::notify::queue::Outcome::CloseRequested(id) => state.remove_notification(id, 3),
            crate::notify::queue::Outcome::Ignored => false,
        };
        if changed {
            state.sync_notifications(now);
            state.redraw_notifications(now);
        }
    }).map_err(|error| format!("failed to insert the notification channel: {error}"))?;
    handle.insert_source(Timer::from_duration(CLOCK_INTERVAL), |_deadline, _metadata, state: &mut State| {
        let now = Instant::now();
        state.on_wake(now);
        TimeoutAction::ToInstant(next_deadline(now, false, state.queue.next_expiry()))
    }).map_err(|error| format!("failed to insert the idle timer: {error}"))?;

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
        let pool = SlotPool::new(self.bar_height as usize * 4096 * 4, &self.shm).expect("failed to create the bar shm pool");
        self.bars.insert(output, Bar { layer, pool, width: 0, height: self.bar_height, configured: false });
    }

    pub fn redraw_all(&mut self) {
        let outputs: Vec<_> = self.bars.keys().cloned().collect();
        for output in outputs { self.redraw(&output); }
    }

    fn notification_settings(&self) -> Option<(NotificationPosition, u64, u64)> {
        self.cfg.notification.as_ref().map(|notification| (notification.position, notification.enter_ms, notification.exit_ms))
    }

    fn create_notification_surface(&mut self, card: Card, notification: Notification, now: Instant, enter_ms: u64) {
        let Some((position, _, _)) = self.notification_settings() else { return };
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
            if position == NotificationPosition::Left { side } else { 0 },
            if position == NotificationPosition::Right { side } else { 0 },
            0,
        );
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(card.rect.w as u32, card.rect.h as u32);
        layer.commit();
        let pool = SlotPool::new(card.rect.w as usize * card.rect.h as usize * 4, &self.shm).expect("failed to create the notification shm pool");
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
                let pool = resize.then(|| SlotPool::new(card.rect.w as usize * card.rect.h as usize * 4, &self.shm).expect("failed to resize the notification shm pool"));
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
        if self.sections.update(&crate::widget::Event::Wake) { self.redraw_all(); }
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
        let side = crate::notify::view::side_margin(&self.theme);
        let position = self.notification_settings().map(|settings| settings.0).expect("settings exist");

        for notification in self.notifications.values_mut() {
            let top = crate::notify::view::surface_top(&self.theme, self.cfg.bar.margin, notification.motion.top(now));
            notification.layer.set_margin(
                top,
                if position == NotificationPosition::Left { side } else { 0 },
                if position == NotificationPosition::Right { side } else { 0 },
                0,
            );
            if !notification.configured || notification.width == 0 || notification.height == 0 { continue; }

            let width = notification.width as i32;
            let height = notification.height as i32;
            let visual = notification.motion.visual(now, notification.card.rect.w, notification.card.rect.h);
            let (buffer, data) = notification.pool.create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)
                .expect("failed to create the notification buffer");
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
                hits.push((visual.rect, Action::NotificationClose(notification.card.id)));
            }

            let region = Region::new(&self.compositor).expect("failed to create the notification input region");
            for (rect, _) in &hits { region.add(rect.x, rect.y, rect.w, rect.h); }
            notification.layer.set_input_region(Some(region.wl_region()));
            notification.layer.wl_surface().damage_buffer(0, 0, width, height);
            buffer.attach_to(notification.layer.wl_surface()).expect("attach failed");
            notification.layer.commit();
            notification.hits = hits;
        }
    }

    fn redraw(&mut self, output: &wl_output::WlOutput) {
        let qh = self.qh.clone();
        if let Some(bar) = self.bars.get_mut(output) { draw_bar(bar, &qh, &self.theme, &mut self.text, &mut self.sections); }
    }
}

fn draw_bar(bar: &mut Bar, _qh: &QueueHandle<State>, theme: &crate::theme::Theme, text: &mut crate::text::TextEngine, sections: &mut crate::bar::Sections) {
    if !bar.configured { return; }
    let width = bar.width.max(1) as i32;
    let height = bar.height.max(1) as i32;
    let widths = sections.widths(text, theme);
    let layout = crate::bar::layout(&widths, width, theme);
    let (buffer, data) = bar.pool.create_buffer(width, height, width * 4, wl_shm::Format::Argb8888).expect("failed to create the bar buffer");
    let mut canvas = crate::canvas::Canvas::new(data, width, height);
    canvas.clear();
    canvas.fill_rect(Rect::new(0, 0, width, height), theme.background);
    let top = text.optical_top(&theme.font, 0, theme.height);
    let rows = [
        (&layout.left[..], &mut sections.left[..]),
        (&layout.center[..], &mut sections.center[..]),
        (&layout.right[..], &mut sections.right[..]),
    ];
    for (rects, modules) in rows {
        for (rect, module) in rects.iter().zip(modules.iter_mut()) {
            if rect.is_empty() { continue; }
            let spans = crate::bar::fit_text(&module.spans(), rect.w, text, theme);
            let mut x = rect.x;
            for span in spans {
                let color = span.color.unwrap_or(theme.foreground);
                text.draw(&mut canvas, &span.text, x, top, &theme.font, color);
                x += text.measure(&span.text, &theme.font).0.ceil() as i32;
            }
        }
    }
    bar.layer.wl_surface().damage_buffer(0, 0, width, height);
    buffer.attach_to(bar.layer.wl_surface()).expect("attach failed");
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
                if !surface.configured {
                    surface.configured = true;
                    eprintln!("notif {} configured: {}x{}", id, surface.width, surface.height);
                }
            }
            self.redraw_notifications(Instant::now());
            return;
        }

        let Some(output) = self.bars.iter().find(|(_, bar)| bar.layer.wl_surface() == layer.wl_surface()).map(|(output, _)| output.clone()) else { return };
        if let Some(bar) = self.bars.get_mut(&output) {
            if let Some(width) = NonZeroU32::new(configure.new_size.0) { bar.width = width.get(); }
            if let Some(height) = NonZeroU32::new(configure.new_size.1) { bar.height = height.get(); }
            if !bar.configured {
                bar.configured = true;
                eprintln!("bar configured: {}x{}", bar.width, bar.height);
            }
        }
        self.redraw(&output);
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) { self.add_bar(output); }
    fn update_output(&mut self, _connection: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
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
            let Some((_, surface)) = self.notifications.iter().find(|(_, surface)| surface.layer.wl_surface() == &event.surface) else { continue };
            let PointerEventKind::Press { button, .. } = event.kind else { continue };
            match crate::notify::view::hit(&surface.hits, event.position.0 as i32, event.position.1 as i32) {
                Some(Action::NotificationAction { id, key }) if button == BTN_LEFT => {
                    if let Some(connection) = &self.dbus { let _ = crate::notify::service::emit_action(connection, id, &key); }
                    closed = Some(id);
                }
                Some(Action::NotificationClose(id)) if button == BTN_LEFT || button == BTN_MIDDLE => closed = Some(id),
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        let Some((_, notification)) = self.notifications.iter_mut().find(|(_, notification)| notification.layer.wl_surface() == surface) else { return };
        if notification.output.as_ref() != Some(output) { notification.output = Some(output.clone()); }
    }
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

    #[test]
    fn animating_asks_for_next_frame() {
        let now = Instant::now();
        assert_eq!(next_deadline(now, true, None), now + FRAME_INTERVAL);
    }

    #[test]
    fn idle_waits_for_the_clock_tick() {
        let now = Instant::now();
        assert_eq!(next_deadline(now, false, None), now + CLOCK_INTERVAL);
    }

    #[test]
    fn earliest_deadline_wins_and_never_returns_the_past() {
        let now = Instant::now();
        let soon = now + Duration::from_millis(5);
        let later = now + Duration::from_secs(30);
        assert_eq!(next_deadline(now, true, Some(soon)), soon);
        assert_eq!(next_deadline(now, false, Some(later)), now + CLOCK_INTERVAL);
        assert_eq!(next_deadline(now, false, Some(now - Duration::from_secs(1))), now);
    }
}
