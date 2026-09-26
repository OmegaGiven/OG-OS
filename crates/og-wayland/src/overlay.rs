//! Generic wlr-layer-shell full-screen overlay: one transparent,
//! click-through, always-on-top surface per connected output, driven by
//! whatever `Effect` the caller triggers. Extracted from og-notify, which
//! was the first (and so far only) app needing this — any future overlay
//! app (a taskbar popup, say) gets the compositor/output/shm/layer-shell
//! boilerplate for free instead of rewriting it.
//!
//! This module owns the whole event loop — `run_daemon`/`run_preview`
//! block forever (or until the process exits). The caller only ever sees
//! an `Effect` trait object and an `OverlayHandle` to trigger one with.

use std::num::NonZeroU32;
use std::time::Duration;

use calloop::channel::{self, Sender};
use calloop::timer::Timer;
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

/// Something that can draw one frame of itself into a premultiplied
/// ARGB8888 canvas. `render` is called once per output per frame while
/// active; returning `true` means "done", after which this window stops
/// asking for further frames until triggered again.
pub trait Effect: Send {
    fn render(&self, width: u32, height: u32, canvas: &mut [u8]) -> bool;
}

/// A factory called once per connected output (with that output's actual
/// pixel size) to build the `Effect` instance for it — each output can
/// get independently-randomized effect state (different streak positions,
/// etc.) from the same trigger.
pub type EffectFactory = Box<dyn Fn(u32, u32) -> Box<dyn Effect> + Send>;

/// Handle for triggering the overlay from anywhere (another thread, a
/// dbus-watcher loop, ...) — cheap to clone, just a channel sender.
#[derive(Clone)]
pub struct OverlayHandle {
    tx: Sender<EffectFactory>,
}

impl OverlayHandle {
    /// (Re)starts `make_effect` on every currently-connected output.
    pub fn trigger(&self, make_effect: impl Fn(u32, u32) -> Box<dyn Effect> + Send + 'static) {
        let _ = self.tx.send(Box::new(make_effect));
    }
}

/// Runs forever as a persistent daemon. `on_ready` is called once,
/// synchronously, with a handle you can clone into a background thread
/// (e.g. a dbus watcher) to call `.trigger(...)` whenever something should
/// fire the overlay later.
pub fn run_daemon(on_ready: impl FnOnce(OverlayHandle)) -> ! {
    let (mut event_loop, mut state, qh) = bootstrap();

    let (tx, channel) = channel::channel::<EffectFactory>();
    event_loop
        .handle()
        .insert_source(channel, move |event, _, state: &mut State| {
            if let channel::Event::Msg(make_effect) = event {
                state.trigger(&qh, &make_effect);
            }
        })
        .expect("insert overlay trigger channel");

    on_ready(OverlayHandle { tx });

    event_loop.run(None, &mut state, |_| {}).expect("overlay event loop");
    std::process::exit(0);
}

/// Fires `make_effect` once immediately on every connected output, waits
/// for every window to finish (a streak's fall time varies, so this can't
/// just be a flat timer without risking cutting the animation off
/// mid-fall), then exits the process. A 10s safety net covers the case
/// where a compositor never sends a frame callback for some reason.
pub fn run_preview(make_effect: impl Fn(u32, u32) -> Box<dyn Effect> + Send + 'static) -> ! {
    let (mut event_loop, mut state, qh) = bootstrap();

    let make_effect: EffectFactory = Box::new(make_effect);
    state.trigger(&qh, &make_effect);
    if state.windows.iter().all(|w| w.effect.is_none()) {
        // Nothing actually started (e.g. no outputs) — nothing to wait for.
        std::process::exit(0);
    }

    event_loop
        .handle()
        .insert_source(Timer::from_duration(Duration::from_secs(10)), |_, _, _| {
            std::process::exit(0);
        })
        .expect("insert exit timer");

    state.exit_when_all_done = true;
    event_loop.run(None, &mut state, |_| {}).expect("overlay event loop");
    std::process::exit(0);
}

fn bootstrap() -> (EventLoop<'static, State>, State, QueueHandle<State>) {
    let conn = Connection::connect_to_env().expect("no wayland connection");
    let (globals, mut event_queue) = registry_queue_init(&conn).expect("registry init");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell missing (compositor unsupported)");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");

    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        windows: Vec::new(),
        exit_when_all_done: false,
    };

    // Drain the initial burst of output announcements so every connected
    // monitor gets a window before we settle into the steady-state loop.
    for _ in 0..5 {
        let _ = event_queue.roundtrip(&mut state);
    }

    let event_loop: EventLoop<State> = EventLoop::try_new().expect("calloop init");
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .expect("insert wayland source");

    (event_loop, state, qh)
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    windows: Vec<Window>,
    /// Only set in `run_preview` — `run_daemon` never exits on its own.
    exit_when_all_done: bool,
}

impl State {
    fn trigger(&mut self, qh: &QueueHandle<State>, make_effect: &EffectFactory) {
        for win in self.windows.iter_mut() {
            win.effect = Some(make_effect(win.width, win.height));
            if !win.frame_pending {
                win.draw(qh);
            }
        }
    }
}

struct Window {
    layer: LayerSurface,
    pool: SlotPool,
    width: u32,
    height: u32,
    configured: bool,
    effect: Option<Box<dyn Effect>>,
    frame_pending: bool,
}

impl Window {
    fn draw(&mut self, qh: &QueueHandle<State>) {
        let Some(effect) = &self.effect else { return };
        let width = self.width;
        let height = self.height;
        let stride = width as i32 * 4;

        let Ok((buffer, canvas)) =
            self.pool.create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };

        canvas.fill(0);
        let done = effect.render(width, height, canvas);

        self.layer.wl_surface().damage_buffer(0, 0, width as i32, height as i32);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        self.frame_pending = true;
        let _ = buffer.attach_to(self.layer.wl_surface());
        self.layer.commit();

        if done {
            self.effect = None;
        }
    }

    fn draw_blank(&mut self) {
        let stride = self.width as i32 * 4;
        if let Ok((buffer, canvas)) =
            self.pool.create_buffer(self.width as i32, self.height as i32, stride, wl_shm::Format::Argb8888)
        {
            canvas.fill(0);
            let _ = buffer.attach_to(self.layer.wl_surface());
            self.layer.commit();
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _time: u32) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.layer.wl_surface() == surface) {
            win.frame_pending = false;
            if win.effect.is_some() {
                win.draw(qh);
            }
        }
        if self.exit_when_all_done && self.windows.iter().all(|w| w.effect.is_none()) {
            std::process::exit(0);
        }
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("og-wayland-overlay"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        if let Ok(region) = Region::new(&self.compositor) {
            layer.wl_surface().set_input_region(Some(region.wl_region()));
        }
        layer.commit();

        let pool = SlotPool::new(4, &self.shm).expect("shm pool");
        self.windows.push(Window {
            layer,
            pool,
            width: 1,
            height: 1,
            configured: false,
            effect: None,
            frame_pending: false,
        });
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.windows.retain(|w| w.layer.wl_surface() != layer.wl_surface());
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(win) = self.windows.iter_mut().find(|w| w.layer.wl_surface() == layer.wl_surface()) else { return };
        win.width = NonZeroU32::new(configure.new_size.0).map_or(1920, NonZeroU32::get);
        win.height = NonZeroU32::new(configure.new_size.1).map_or(1080, NonZeroU32::get);
        if !win.configured {
            win.configured = true;
            // First commit must happen even with nothing to show, or the
            // surface never maps.
            win.draw_blank();
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_layer!(State);
delegate_registry!(State);
