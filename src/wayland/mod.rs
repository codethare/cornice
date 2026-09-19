//! Wayland connection, layer surface, shm buffers and the main event loop.

use std::collections::HashMap;
use std::num::NonZeroU32;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerHandler},
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

pub const LAYER_NAMESPACE: &str = "cornice";

pub struct Bar {
    pub layer: LayerSurface,
    pub pool: SlotPool,
    pub width: u32,
    pub height: u32,
    pub configured: bool,
}

pub struct State {
    /// A copy of the connection for later tasks; this probe does not read it yet.
    #[allow(dead_code)]
    pub conn: Connection,
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
    pub cfg: crate::config::Config,
    pub theme: crate::theme::Theme,
    /// Text layout engine; used for drawing from Task 5 on, created here to avoid changing `run`'s signature again.
    pub text: crate::text::TextEngine,
    /// The left/center/right module lists.
    pub sections: crate::bar::Sections,
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
        conn: conn.clone(),
        cfg,
        theme,
        text: crate::text::TextEngine::new(),
        sections,
    };

    let mut event_loop: EventLoop<State> = EventLoop::try_new().map_err(|e| format!("failed to create the event loop: {e}"))?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue).insert(handle.clone()).map_err(|e| format!("failed to insert the wayland source: {e}"))?;
    handle.insert_source(exec_rx, |msg, _meta, state: &mut State| {
        let calloop::channel::Event::Msg(ev) = msg else { return };
        if state.sections.update(&ev) { state.redraw_all(); }
    }).map_err(|e| format!("failed to insert the exec channel: {e}"))?;

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
        layer.set_exclusive_zone(self.bar_height as i32);
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
    for (rects, modules) in rows {
        for (r, m) in rects.iter().zip(modules.iter_mut()) {
            if r.is_empty() { continue; }
            let spans = crate::bar::fit_text(&m.spans(), r.w, text, theme);
            let mut x = r.x;
            for s in spans {
                let color = s.color.unwrap_or(theme.foreground);
                text.draw(&mut c, &s.text, x, r.y + (r.h - theme.font.size as i32) / 2 - 2, &theme.font, color);
                x += text.measure(&s.text, &theme.font).0.ceil() as i32;
            }
        }
    }
    bar.layer.wl_surface().damage_buffer(0, 0, w, h);
    buffer.attach_to(bar.layer.wl_surface()).expect("attach failed");
    bar.layer.commit();
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        // find the output for that layer surface, then let `redraw` run layout and drawing uniformly
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
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) { self.add_bar(output); }
    fn update_output(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _output: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, output: wl_output::WlOutput) { self.bars.remove(&output); }
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
    fn pointer_frame(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _pointer: &wl_pointer::WlPointer, _events: &[PointerEvent]) {
        // Task 9 handles notification clicks; nothing here yet
    }
}

// This task draws no frames and needs no frame callback; animations are driven by a calloop timer (Task 6/9).
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
