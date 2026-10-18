//! Lean runtime for procedural scenes: raw Wayland (wlr-layer-shell) + one
//! EGL/GLES 3 context, no toolkit. One thread sleeps in poll() on the
//! Wayland socket; frames are paced by the compositor's frame callbacks
//! (capped at `wallpaper_fps`), so nothing runs while sway isn't showing a
//! surface. On top of that, sway IPC (sway.rs) freezes outputs whose
//! workspace has windows tiled over the desktop.
//!
//! Per output size a board is generated once on a worker thread and
//! uploaded as textures; after that a frame costs one 128-byte uniform
//! upload and one fullscreen triangle.

pub mod gl;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use khronos_egl as egl;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_callback, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_egl::WlEglSurface;
use wayland_protocols::wp::cursor_shape::v1::client::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use gl::{Board, Context, Frame, Generated, Renderer, Scene};

/// A frame callback that hasn't arrived after this long while the output is
/// supposed to animate is treated as lost (render anyway).
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(1);

enum Msg {
    Exposed(Option<Vec<String>>),
    Board((u32, u32), Box<dyn std::any::Any + Send>),
}

struct Surf {
    wl: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    /// Logical size from the last configure.
    logical: (u32, u32),
    /// Buffer size in pixels (logical × scale).
    size: (u32, u32),
    egl: Option<(WlEglSurface, egl::Surface, bool)>,
    /// A frame callback is outstanding (and when it was requested).
    pending: Option<Instant>,
    /// Last frame drawn with the board (not just the clear color).
    drawn: bool,
    any_drawn: bool,
    next_due: Instant,
}

struct Out<S: Scene> {
    global: u32,
    output: wl_output::WlOutput,
    name: Option<String>,
    scale: i32,
    surf: Option<Surf>,
    state: S::Out,
}

struct State<S: Scene> {
    qh: QueueHandle<Self>,
    compositor: wl_compositor::WlCompositor,
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    cursor_shape: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    /// Kept bound so pointer capability events keep arriving.
    _seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_dev: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    /// Surface under the pointer and the position on it (logical px).
    pointer_at: Option<(wayland_client::backend::ObjectId, f64, f64)>,
    outs: Vec<Out<S>>,
    /// None = unknown (sway IPC unavailable): animate everything.
    exposed: Option<Vec<String>>,
    ctx: Context,
    r: Renderer,
    g: S::Gl,
    scene: S,
    boards: HashMap<(u32, u32), Board<S::Info>>,
    generating: Vec<(u32, u32)>,
    tx: mpsc::Sender<Msg>,
    wake: UnixStream,
    start: Instant,
    interval: Duration,
}

pub fn run<S: Scene>(scene: S, fps: u32) -> Result<(), String> {
    // One malloc arena for every thread: boards are generated on worker
    // threads, and glibc would otherwise keep each worker's arena (tens of
    // MB of freed generation buffers) mapped for the life of the process.
    // With a single arena, malloc_trim after upload hands it all back.
    // SAFETY: plain glibc tunable, set before any worker thread exists.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 1);
    }
    // Mesa helper threads we don't need for one tiny draw stream: GL command
    // marshalling, gallium's threaded context, the on-disk shader cache, and
    // (radeonsi) background recompiles of "optimized" shader variants, which
    // also grew its compiler thread pool. Measured: same GPU time without.
    for (k, v) in [
        ("mesa_glthread", "false"),
        ("GALLIUM_THREAD", "0"),
        ("MESA_SHADER_CACHE_DISABLE", "true"),
        ("AMD_DEBUG", "nooptvariant"),
    ] {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
    let conn = Connection::connect_to_env().map_err(|e| format!("wayland: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State<S>>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
    let qh = queue.handle();
    let compositor = globals.bind(&qh, 4..=6, ()).map_err(|e| format!("wl_compositor: {e}"))?;
    let layer_shell = globals.bind(&qh, 1..=4, ()).map_err(|e| format!("zwlr_layer_shell_v1: {e}"))?;
    let cursor_shape = globals.bind(&qh, 1..=1, ()).ok();
    let seat = globals.bind(&qh, 1..=7, ()).ok();

    let ctx = Context::new(Some(conn.backend().display_ptr() as *mut _))?;
    ctx.egl
        .make_current(ctx.display, None, None, Some(ctx.context))
        .map_err(|e| format!("eglMakeCurrent (surfaceless): {e}"))?;
    let r = Renderer::new(ctx.glow())?;
    let g = S::init_gl(&r)?;

    let (tx, rx) = mpsc::channel();
    let (mut wake_r, wake) = UnixStream::pair().map_err(|e| e.to_string())?;
    wake_r.set_nonblocking(true).map_err(|e| e.to_string())?;
    {
        let tx = tx.clone();
        let mut w = wake.try_clone().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            crate::sway::watch(|e| tx.send(Msg::Exposed(e)).is_ok() && w.write_all(&[1]).is_ok())
        });
    }

    let mut state = State {
        qh: qh.clone(),
        compositor,
        layer_shell,
        cursor_shape,
        _seat: seat,
        pointer: None,
        cursor_dev: None,
        pointer_at: None,
        outs: Vec::new(),
        exposed: None,
        ctx,
        r,
        g,
        scene,
        boards: HashMap::new(),
        generating: Vec::new(),
        tx,
        wake,
        start: Instant::now(),
        interval: Duration::from_secs_f64(1.0 / fps.clamp(1, 240) as f64),
    };
    globals.contents().with_list(|list| {
        for g in list.iter().filter(|g| g.interface == wl_output::WlOutput::interface().name) {
            state.add_output(globals.registry(), g.name, g.version);
        }
    });

    loop {
        queue.dispatch_pending(&mut state).map_err(|e| format!("wayland dispatch: {e}"))?;
        while let Ok(msg) = rx.try_recv() {
            state.handle(msg);
        }
        let timeout = state.render_due();
        queue.flush().map_err(|e| format!("wayland flush: {e}"))?;

        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [
            libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        // SAFETY: two valid pollfds that outlive the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
        if n > 0 && fds[0].revents != 0 {
            guard.read().map_err(|e| format!("wayland read: {e}"))?;
        } else {
            drop(guard);
        }
        if fds[1].revents != 0 {
            let mut buf = [0u8; 64];
            while matches!(wake_r.read(&mut buf), Ok(n) if n > 0) {}
        }
    }
}

impl<S: Scene> State<S> {
    fn add_output(&mut self, registry: &wl_registry::WlRegistry, global: u32, version: u32) {
        let output = registry.bind::<wl_output::WlOutput, _, _>(global, version.min(4), &self.qh, global);
        self.outs.push(Out { global, output, name: None, scale: 1, surf: None, state: S::Out::default() });
    }

    fn out_mut(&mut self, global: u32) -> Option<&mut Out<S>> {
        self.outs.iter_mut().find(|o| o.global == global)
    }

    fn create_surface(&mut self, global: u32) {
        let qh = self.qh.clone();
        let wl = self.compositor.create_surface(&qh, ());
        let Some(out) = self.outs.iter_mut().find(|o| o.global == global) else { return };
        use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};
        let layer = self.layer_shell.get_layer_surface(
            &wl,
            Some(&out.output),
            zwlr_layer_shell_v1::Layer::Background,
            "og-wallpaper".into(),
            &qh,
            global,
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        wl.commit();
        out.surf = Some(Surf {
            wl,
            layer,
            logical: (0, 0),
            size: (0, 0),
            egl: None,
            pending: None,
            drawn: false,
            any_drawn: false,
            next_due: Instant::now(),
        });
    }

    fn destroy_surface(&mut self, global: u32) {
        let Some(out) = self.out_mut(global) else { return };
        let Some(s) = out.surf.take() else { return };
        let size = s.size;
        self.drop_surf(s);
        self.free_unused(size);
    }

    fn drop_surf(&self, s: Surf) {
        if let Some((_win, es, _)) = s.egl {
            let _ = self.ctx.egl.make_current(self.ctx.display, None, None, Some(self.ctx.context));
            let _ = self.ctx.egl.destroy_surface(self.ctx.display, es);
        }
        s.layer.destroy();
        s.wl.destroy();
    }

    /// Free a board no output uses any more.
    fn free_unused(&mut self, size: (u32, u32)) {
        if self.outs.iter().any(|o| o.surf.as_ref().is_some_and(|s| s.size == size)) {
            return;
        }
        if let Some(b) = self.boards.remove(&size) {
            let _ = self.ctx.egl.make_current(self.ctx.display, None, None, Some(self.ctx.context));
            self.r.free(b);
        }
    }

    /// (Re)size the EGL window after a configure or scale change.
    fn resize(&mut self, global: u32) {
        let Some(out) = self.outs.iter_mut().find(|o| o.global == global) else { return };
        let scale = out.scale.max(1);
        let Some(s) = out.surf.as_mut() else { return };
        if s.logical.0 == 0 || s.logical.1 == 0 {
            return;
        }
        let size = (s.logical.0 * scale as u32, s.logical.1 * scale as u32);
        let old = s.size;
        s.wl.set_buffer_scale(scale);
        match &s.egl {
            Some((win, _, _)) => win.resize(size.0 as i32, size.1 as i32, 0, 0),
            None => match WlEglSurface::new(s.wl.id(), size.0 as i32, size.1 as i32) {
                Ok(win) => match self.ctx.window_surface(win.ptr() as *mut _) {
                    Ok((es, encode)) => {
                        let _ = self.ctx.egl.make_current(self.ctx.display, Some(es), Some(es), Some(self.ctx.context));
                        // Never block in eglSwapBuffers: pacing is ours (and
                        // a covered surface gets no frame callbacks at all).
                        let _ = self.ctx.egl.swap_interval(self.ctx.display, 0);
                        s.egl = Some((win, es, encode));
                    }
                    Err(e) => eprintln!("og-wallpaper: {e}"),
                },
                Err(e) => eprintln!("og-wallpaper: wl_egl_window: {e}"),
            },
        }
        s.size = size;
        s.drawn = false;
        s.any_drawn = false;
        if old != size && old != (0, 0) {
            self.free_unused(old);
        }
        self.ensure_board(size);
    }

    fn ensure_board(&mut self, size: (u32, u32)) {
        if self.boards.contains_key(&size) || self.generating.contains(&size) {
            return;
        }
        self.generating.push(size);
        let scene = self.scene.clone();
        let tx = self.tx.clone();
        let Ok(mut wake) = self.wake.try_clone() else { return };
        std::thread::spawn(move || {
            let g: Generated<S::Info> = scene.generate(size.0, size.1);
            let _ = tx.send(Msg::Board(size, Box::new(g)));
            let _ = wake.write_all(&[1]);
        });
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Exposed(e) => {
                if e != self.exposed {
                    match &e {
                        Some(list) => eprintln!("og-wallpaper: animating {list:?} (others frozen)"),
                        None => eprintln!("og-wallpaper: animating all outputs"),
                    }
                }
                self.exposed = e;
            }
            Msg::Board(size, g) => {
                self.generating.retain(|s| *s != size);
                let Ok(g) = g.downcast::<Generated<S::Info>>() else { return };
                let in_use = self.outs.iter().any(|o| o.surf.as_ref().is_some_and(|s| s.size == size));
                if !in_use {
                    return;
                }
                let _ = self.ctx.egl.make_current(self.ctx.display, None, None, Some(self.ctx.context));
                let board = self.r.upload(*g);
                self.boards.insert(size, board);
                for s in self.outs.iter_mut().filter_map(|o| o.surf.as_mut()) {
                    if s.size == size {
                        s.drawn = false;
                    }
                }
            }
        }
    }

    fn animating(&self, out: &Out<S>) -> bool {
        match (&self.exposed, &out.name) {
            (Some(list), Some(name)) => list.contains(name),
            _ => true,
        }
    }

    /// Render every surface that's due; returns how long poll may sleep.
    fn render_due(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let mut due = Vec::new();
        let mut wait: Option<Duration> = None;
        for (i, out) in self.outs.iter().enumerate() {
            let Some(s) = &out.surf else { continue };
            if s.egl.is_none() {
                continue;
            }
            let has_board = self.boards.contains_key(&s.size);
            let first = !s.any_drawn || (has_board && !s.drawn);
            let animate = has_board && self.animating(out);
            if !first && !animate {
                continue;
            }
            if let Some(at) = s.pending {
                // Waiting on the compositor. A lost callback (rare) is
                // retried after a second.
                if now.duration_since(at) < CALLBACK_TIMEOUT {
                    continue;
                }
            }
            if first || now >= s.next_due {
                due.push(i);
            } else {
                let d = s.next_due - now;
                wait = Some(wait.map_or(d, |w| w.min(d)));
            }
        }
        if !due.is_empty() {
            self.scene.tick();
            for i in due {
                self.render(i, now);
            }
            // Frame callbacks wake us next; the timeout only covers capped
            // frame rates and lost callbacks.
            wait = Some(wait.map_or(CALLBACK_TIMEOUT, |w| w.min(CALLBACK_TIMEOUT)));
        }
        wait
    }

    fn render(&mut self, i: usize, now: Instant) {
        let t = (self.start.elapsed().as_secs_f64() % 3600.0) as f32;
        let mouse = self.pointer_at.clone();
        let out = &mut self.outs[i];
        let scale = out.scale.max(1) as f32;
        let Some(s) = out.surf.as_mut() else { return };
        let Some((_, es, encode)) = &s.egl else { return };
        if self.ctx.egl.make_current(self.ctx.display, Some(*es), Some(*es), Some(self.ctx.context)).is_err() {
            return;
        }
        let (w, h) = s.size;
        self.r.bind_target(None, w, h);
        match self.boards.get_mut(&s.size) {
            Some(board) => {
                let mouse = mouse
                    .filter(|(id, _, _)| *id == s.wl.id())
                    .map(|(_, x, y)| (x as f32 * scale, y as f32 * scale));
                let f = Frame { w: w as f32, h: h as f32, t, mouse, encode: *encode, target: None };
                self.scene.draw(&self.r, &self.g, board, &mut out.state, &f);
                s.drawn = true;
            }
            None => self.r.clear(self.scene.clear_color(), *encode),
        }
        s.wl.frame(&self.qh, out.global);
        s.pending = Some(now);
        // A little slack so a cap equal to the refresh rate renders on every
        // frame callback instead of every other one.
        s.next_due = now + self.interval.saturating_sub(Duration::from_millis(2));
        if let Err(e) = self.ctx.egl.swap_buffers(self.ctx.display, *es) {
            eprintln!("og-wallpaper: eglSwapBuffers: {e}");
        }
        s.any_drawn = true;
    }
}

impl<S: Scene> Dispatch<wl_registry::WlRegistry, GlobalListContents> for State<S> {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } if interface == wl_output::WlOutput::interface().name => {
                state.add_output(registry, name, version);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if state.outs.iter().any(|o| o.global == name) {
                    state.destroy_surface(name);
                    if let Some(i) = state.outs.iter().position(|o| o.global == name) {
                        let out = state.outs.remove(i);
                        if out.output.version() >= 3 {
                            out.output.release();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl<S: Scene> Dispatch<wl_output::WlOutput, u32> for State<S> {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event, global: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let global = *global;
        let Some(out) = state.out_mut(global) else { return };
        match event {
            wl_output::Event::Name { name } => out.name = Some(name),
            wl_output::Event::Scale { factor } => out.scale = factor,
            wl_output::Event::Done => {
                if out.surf.is_none() {
                    state.create_surface(global);
                } else {
                    state.resize(global);
                }
            }
            _ => {}
        }
    }
}

impl<S: Scene> Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, u32> for State<S> {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                if let Some(s) = state.out_mut(*global).and_then(|o| o.surf.as_mut()) {
                    s.logical = (width, height);
                }
                state.resize(*global);
            }
            zwlr_layer_surface_v1::Event::Closed => state.destroy_surface(*global),
            _ => {}
        }
    }
}

impl<S: Scene> Dispatch<wl_callback::WlCallback, u32> for State<S> {
    fn event(state: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event, global: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(s) = state.out_mut(*global).and_then(|o| o.surf.as_mut()) {
                s.pending = None;
            }
        }
    }
}

impl<S: Scene> Dispatch<wl_seat::WlSeat, ()> for State<S> {
    fn event(state: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let has = caps.contains(wl_seat::Capability::Pointer);
            if has && state.pointer.is_none() {
                let p = seat.get_pointer(qh, ());
                state.cursor_dev = state.cursor_shape.as_ref().map(|m| m.get_pointer(&p, qh, ()));
                state.pointer = Some(p);
            } else if !has {
                if let Some(d) = state.cursor_dev.take() {
                    d.destroy();
                }
                if let Some(p) = state.pointer.take() {
                    if p.version() >= 3 {
                        p.release();
                    }
                }
                state.pointer_at = None;
            }
        }
    }
}

impl<S: Scene> Dispatch<wl_pointer::WlPointer, ()> for State<S> {
    fn event(state: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                // We own the cursor image while over the desktop.
                if let Some(d) = &state.cursor_dev {
                    d.set_shape(serial, wp_cursor_shape_device_v1::Shape::Default);
                }
                state.pointer_at = Some((surface.id(), surface_x, surface_y));
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(p) = state.pointer_at.as_mut() {
                    p.1 = surface_x;
                    p.2 = surface_y;
                }
            }
            wl_pointer::Event::Leave { .. } => state.pointer_at = None,
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(@<S: Scene> State<S>: ignore wl_compositor::WlCompositor);
wayland_client::delegate_noop!(@<S: Scene> State<S>: ignore wl_surface::WlSurface);
wayland_client::delegate_noop!(@<S: Scene> State<S>: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
wayland_client::delegate_noop!(@<S: Scene> State<S>: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
wayland_client::delegate_noop!(@<S: Scene> State<S>: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);
