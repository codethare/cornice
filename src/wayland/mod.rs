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
    };

    let mut event_loop: EventLoop<State> = EventLoop::try_new().map_err(|e| format!("failed to create the event loop: {e}"))?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue).insert(handle).map_err(|e| format!("failed to insert the wayland source: {e}"))?;

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
}

/// Fill the whole bar with a solid colour. Replaced by Canvas drawing from Task 3.
fn draw_bar(bar: &mut Bar, _qh: &QueueHandle<State>, pixels: &[u8; 4]) {
    let w = bar.width.max(1) as i32;
    let h = bar.height.max(1) as i32;
    let (buffer, canvas) = bar.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).expect("failed to create the buffer");
    // wl_shm Argb8888 is little-endian premultiplied ARGB, stored as B,G,R,A
    for px in canvas.chunks_exact_mut(4) {
        px.copy_from_slice(pixels);
    }
    bar.layer.wl_surface().damage_buffer(0, 0, w, h);
    buffer.attach_to(bar.layer.wl_surface()).expect("attach failed");
    bar.layer.commit();
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _serial: u32) {
        // Take the background colour first, avoiding the mutable borrow of self.bars below
        let pixels = self.theme.background.to_shm_bytes();
        let Some((_output, bar)) = self.bars.iter_mut().find(|(_, b)| b.layer.wl_surface() == layer.wl_surface()) else {
            return;
        };
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
        draw_bar(bar, qh, &pixels);
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
