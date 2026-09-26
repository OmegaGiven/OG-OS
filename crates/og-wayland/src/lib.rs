//! Real OS-level (cross-application) drag-and-drop of files out of an
//! iced window — e.g. dragging a file from og-files onto Brave's upload
//! drop zone.
//!
//! iced/winit have no Wayland drag-and-drop support at all (checked: no
//! `wl_data_source`/`start_drag` anywhere in either crate). The trick,
//! copied from how `smithay-clipboard` implements copy/paste (which iced
//! already depends on transitively): open a *second*, independent
//! `wayland-client` connection wrapping the *same real* Wayland socket
//! winit is already using (`Backend::from_foreign_display`), bind our own
//! `wl_seat`/`wl_pointer`/`wl_data_device_manager` on it, and use
//! smithay-client-toolkit's `DragSource` to start a real drag. Our own
//! pointer gets real `enter`/`press` events (with real serials and a real
//! reference to winit's surface) because the compositor broadcasts input
//! events to every `wl_pointer` resource a client has bound for a seat —
//! not just the one the toolkit itself created.
//!
//! Only handles being the drag *source* here (files leaving the window).
//! Internal same-window drag-to-folder is each app's own business, via
//! plain iced mouse events — this crate doesn't need to know about it.

pub mod overlay;

use std::ffi::{c_void, CString};
use std::path::PathBuf;

use calloop::channel::{self, Sender};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::data_device_manager::data_source::DragSource;
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::{data_device::DataDeviceHandler, DataDeviceManagerState};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shm::{slot::SlotPool, Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use wayland_client::backend::Backend;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{
    wl_data_device_manager::DndAction, wl_output, wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Proxy, QueueHandle};

pub struct DragRequest {
    pub paths: Vec<PathBuf>,
}

/// Something being dragged *into* our window from another client (a
/// browser, Dolphin, another og-files window...). Coordinates are
/// surface-local logical pixels, same space as iced's cursor position.
#[derive(Debug, Clone)]
pub enum DropEvent {
    Enter { x: f64, y: f64 },
    Motion { x: f64, y: f64 },
    Leave,
    /// The drop happened and the `text/uri-list` payload was read.
    Dropped { paths: Vec<PathBuf>, x: f64, y: f64 },
}

/// Fixed drag-icon canvas size — big enough to read as an icon, small
/// enough to stay a cursor accessory rather than a window of its own.
const ICON_SIZE: i32 = 56;

/// Reads the real `wl_display*` behind a raw Wayland `wl_surface*` (as
/// handed to us by `iced::window::run_with_handle`) via libwayland-client's
/// own `wl_proxy_get_display`. `RTLD_NOLOAD` ensures we attach to the
/// *already-loaded* copy of the library (the one winit itself is using),
/// not load a second instance with separate internal state.
///
/// # Safety
/// `surface` must be a live `*mut wl_proxy` for as long as this call runs
/// (true here — it's read synchronously inside iced's own event loop via
/// `run_with_handle`, before the window can have closed).
unsafe fn display_from_surface(surface: *mut c_void) -> Option<*mut c_void> {
    let lib_name = CString::new("libwayland-client.so.0").ok()?;
    let handle = libc::dlopen(lib_name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
    if handle.is_null() {
        return None;
    }
    let sym_name = CString::new("wl_proxy_get_display").ok()?;
    let sym = libc::dlsym(handle, sym_name.as_ptr());
    if sym.is_null() {
        return None;
    }
    let func: extern "C" fn(*mut c_void) -> *mut c_void = std::mem::transmute(sym);
    let display = func(surface);
    if display.is_null() {
        None
    } else {
        Some(display)
    }
}

/// Extracts the raw Wayland surface pointer from an iced `WindowHandle`,
/// then resolves it to the real `wl_display*`. Returns `None` on any
/// non-Wayland backend (e.g. if this ever runs under X11) or FFI failure.
pub fn display_ptr_from_window_handle(handle: iced::window::raw_window_handle::WindowHandle<'_>) -> Option<usize> {
    use iced::window::raw_window_handle::RawWindowHandle;
    let RawWindowHandle::Wayland(wh) = handle.as_raw() else { return None };
    let surface_ptr = wh.surface.as_ptr();
    let display_ptr = unsafe { display_from_surface(surface_ptr) }?;
    Some(display_ptr as usize)
}

/// Spawns the worker thread and returns a channel to request drags on it.
/// `display_ptr` is the real `wl_display*` (as a plain integer so it can
/// cross the thread boundary — the pointer itself is reconstructed, and
/// wrapped in a `Connection`, inside the new thread).
pub fn spawn(display_ptr: usize) -> Sender<DragRequest> {
    spawn_with_drops(display_ptr).0
}

/// Like `spawn`, but also reports drags coming *into* the window on the
/// returned receiver. winit has no Wayland drop-target support at all, so
/// this is the only way an iced window learns about a drop.
pub fn spawn_with_drops(display_ptr: usize) -> (Sender<DragRequest>, std::sync::mpsc::Receiver<DropEvent>) {
    let (tx, rx) = channel::channel();
    let (drop_tx, drop_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("og-wayland-drag".into())
        .spawn(move || {
            // Safety: `display_ptr` came from `wl_proxy_get_display` on a
            // surface belonging to our own still-running window; the real
            // connection stays alive for the process's whole lifetime.
            let backend = unsafe { Backend::from_foreign_display(display_ptr as *mut _) };
            let connection = Connection::from_backend(backend);
            worker(connection, rx, drop_tx);
        })
        .expect("spawn og-wayland-drag thread");
    (tx, drop_rx)
}

fn worker(connection: Connection, rx: channel::Channel<DragRequest>, drop_tx: std::sync::mpsc::Sender<DropEvent>) {
    let (globals, event_queue) = match registry_queue_init::<State>(&connection) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("og-wayland: registry_queue_init failed: {e:?}");
            return;
        }
    };
    let qh = event_queue.handle();

    let data_device_manager_state = match DataDeviceManagerState::bind(&globals, &qh) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("og-wayland: DataDeviceManagerState::bind failed (compositor has no wl_data_device_manager?): {e:?}");
            return;
        }
    };

    // Both optional (unlike data_device_manager_state above) — they only
    // back the drag *icon*. A bind failure here means drags still work,
    // just without a cursor icon (today's behavior), rather than losing
    // drag-and-drop entirely over what's a purely cosmetic addition.
    let compositor = CompositorState::bind(&globals, &qh)
        .inspect_err(|e| eprintln!("og-wayland: CompositorState::bind failed, drag icon disabled: {e:?}"))
        .ok();
    let shm = Shm::bind(&globals, &qh)
        .inspect_err(|e| eprintln!("og-wayland: Shm::bind failed, drag icon disabled: {e:?}"))
        .ok();

    let mut event_loop: EventLoop<State> = EventLoop::try_new().expect("calloop init");
    let mut state = State {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_device_manager_state,
        compositor,
        shm,
        seat: None,
        pointer: None,
        data_device: None,
        latest_serial: 0,
        latest_surface: None,
        active_drag: None,
        drop_tx,
        drop_pos: (0.0, 0.0),
        incoming_offer: None,
    };

    WaylandSource::new(connection, event_queue).insert(event_loop.handle()).expect("insert wayland source");

    event_loop
        .handle()
        .insert_source(rx, move |event, _, state: &mut State| {
            if let channel::Event::Msg(DragRequest { paths }) = event {
                state.start_drag(&qh, paths);
            }
        })
        .expect("insert drag-request channel");

    loop {
        if event_loop.dispatch(None, &mut state).is_err() {
            break;
        }
    }
}

struct State {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    data_device_manager_state: DataDeviceManagerState,
    compositor: Option<CompositorState>,
    shm: Option<Shm>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    data_device: Option<DataDevice>,
    latest_serial: u32,
    latest_surface: Option<WlSurface>,
    drop_tx: std::sync::mpsc::Sender<DropEvent>,
    /// Last surface-local position of an incoming drag.
    drop_pos: (f64, f64),
    /// The offer currently hovering us, if it carries a URI list.
    incoming_offer: Option<DragOffer>,
    /// Kept alive for the duration of the drag — dropping a `DragSource`
    /// cancels it. Also holds the URI-list bytes to hand back on `send`,
    /// and (if compositor/shm bound OK) the icon surface + its backing
    /// pool, which likewise must outlive the drag or the compositor loses
    /// its buffer.
    active_drag: Option<(DragSource, Vec<u8>, Option<(WlSurface, SlotPool)>)>,
}

impl State {
    /// Builds a small drag-icon surface: one "page" rectangle for a single
    /// file, three fanned-out ones for a multi-item drag — distinguishing
    /// "one thing" from "a bunch of things" without needing to rasterize
    /// any text/count badge. `None` on any bind/allocation failure (no
    /// compositor/shm bound, or the compositor rejected the surface) —
    /// callers fall back to dragging with no icon, same as before this
    /// existed, rather than failing the whole drag over a cosmetic extra.
    fn build_icon_surface(&mut self, qh: &QueueHandle<Self>, item_count: usize) -> Option<(WlSurface, SlotPool)> {
        let compositor = self.compositor.as_ref()?;
        let shm = self.shm.as_ref()?;

        let surface = compositor.create_surface(qh);
        let mut pool = SlotPool::new((ICON_SIZE * ICON_SIZE * 4) as usize, shm).ok()?;
        let stride = ICON_SIZE * 4;
        let (buffer, canvas) = pool.create_buffer(ICON_SIZE, ICON_SIZE, stride, wl_shm::Format::Argb8888).ok()?;

        draw_drag_icon(canvas, item_count);

        buffer.attach_to(&surface).ok()?;
        surface.damage_buffer(0, 0, ICON_SIZE, ICON_SIZE);
        surface.commit();

        Some((surface, pool))
    }

    fn start_drag(&mut self, qh: &QueueHandle<Self>, paths: Vec<PathBuf>) {
        let Some(surface) = self.latest_surface.clone() else { return };
        let Some(seat) = self.seat.clone() else { return };

        if self.data_device.is_none() {
            self.data_device = Some(self.data_device_manager_state.get_data_device(qh, &seat));
        }

        let uri_list = paths
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .map(|p| format!("file://{}", p.display()))
            .collect::<Vec<_>>()
            .join("\r\n");

        // Built before borrowing `data_device` below — needs `&mut self`,
        // which can't coexist with the immutable borrow `device` holds.
        let icon = self.build_icon_surface(qh, paths.len());
        let icon_surface_ref = icon.as_ref().map(|(s, _)| s);

        let Some(device) = &self.data_device else { return };
        let source = self.data_device_manager_state.create_drag_and_drop_source(
            qh,
            ["text/uri-list"],
            DndAction::Copy,
        );
        source.start_drag(device, &surface, icon_surface_ref, self.latest_serial);
        self.active_drag = Some((source, uri_list.into_bytes(), icon));
    }
}

/// Draws into a tightly-packed BGRA8888-little-endian / ARGB8888
/// premultiplied canvas (`width*height*4` bytes) — same convention as
/// og-notify's `effect::render` (see that crate for the reasoning on byte
/// order). A plain opaque rectangle with a darker 2px border reads clearly
/// as "a page" at this size without needing any text/font rendering.
fn draw_drag_icon(canvas: &mut [u8], item_count: usize) {
    canvas.fill(0);

    // Furthest-back layer drawn first so nearer ones overlap it, same as
    // a real fanned-out stack of pages.
    let layers: i32 = if item_count > 1 { 3 } else { 1 };
    let (base_x0, base_y0, base_x1, base_y1) = (8, 6, ICON_SIZE - 14, ICON_SIZE - 16);
    for i in 0..layers {
        let offset = (layers - 1 - i) * 6;
        let x0 = (base_x0 + offset).clamp(0, ICON_SIZE - 1);
        let y0 = (base_y0 + offset).clamp(0, ICON_SIZE - 1);
        let x1 = (base_x1 + offset).clamp(0, ICON_SIZE - 1);
        let y1 = (base_y1 + offset).clamp(0, ICON_SIZE - 1);
        fill_rect(canvas, ICON_SIZE, x0, y0, x1, y1, (245, 245, 245), 235);
        stroke_rect(canvas, ICON_SIZE, x0, y0, x1, y1, (90, 90, 90), 255);
    }
}

fn put_pixel(canvas: &mut [u8], width: i32, x: i32, y: i32, rgb: (u8, u8, u8), alpha: u8) {
    if x < 0 || y < 0 || x >= width || alpha == 0 {
        return;
    }
    let idx = (y as usize * width as usize + x as usize) * 4;
    let Some(px) = canvas.get_mut(idx..idx + 4) else { return };
    let a = alpha as u32;
    px[0] = ((rgb.2 as u32 * a) / 255) as u8;
    px[1] = ((rgb.1 as u32 * a) / 255) as u8;
    px[2] = ((rgb.0 as u32 * a) / 255) as u8;
    px[3] = alpha;
}

fn fill_rect(canvas: &mut [u8], width: i32, x0: i32, y0: i32, x1: i32, y1: i32, rgb: (u8, u8, u8), alpha: u8) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            put_pixel(canvas, width, x, y, rgb, alpha);
        }
    }
}

fn stroke_rect(canvas: &mut [u8], width: i32, x0: i32, y0: i32, x1: i32, y1: i32, rgb: (u8, u8, u8), alpha: u8) {
    for x in x0..=x1 {
        put_pixel(canvas, width, x, y0, rgb, alpha);
        put_pixel(canvas, width, x, y1, rgb, alpha);
    }
    for y in y0..=y1 {
        put_pixel(canvas, width, x0, y, rgb, alpha);
        put_pixel(canvas, width, x1, y, rgb, alpha);
    }
}

impl PointerHandler for State {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[PointerEvent]) {
        for event in events {
            // Any event's surface is a live reference to whatever real
            // surface currently has pointer focus — that's winit's window.
            self.latest_surface = Some(event.surface.clone());
            if let PointerEventKind::Press { serial, .. } | PointerEventKind::Release { serial, .. } = event.kind {
                self.latest_serial = serial;
            }
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                if pointer.version() >= 3 {
                    pointer.release();
                }
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl DataSourceHandler for State {
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource, mime: String, write_pipe: WritePipe) {
        if mime != "text/uri-list" {
            return;
        }
        let Some((_, content, _)) = &self.active_drag else { return };
        use std::io::Write;
        let mut pipe = write_pipe;
        let _ = pipe.write_all(content);
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource) {
        self.active_drag = None;
    }

    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource, _mime: Option<String>) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource) {
        self.active_drag = None;
    }

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &wayland_client::protocol::wl_data_source::WlDataSource, _action: DndAction) {}
}

const URI_LIST: &str = "text/uri-list";

/// `file:///home/me/a%20b.txt` lines → paths. Skips comments and non-file
/// URIs (a browser dragging a link gives `https://...`, not a file).
pub fn parse_uri_list(data: &str) -> Vec<PathBuf> {
    data.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let rest = l.strip_prefix("file://")?;
            // Strip an optional host part ("file://localhost/x").
            let path = if rest.starts_with('/') { rest } else { rest.find('/').map(|i| &rest[i..])? };
            Some(PathBuf::from(percent_decode(path)))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

impl State {
    fn current_offer(&self) -> Option<DragOffer> {
        self.data_device.as_ref().and_then(|d| d.data().drag_offer())
    }
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64, _surface: &WlSurface) {
        // Ignore our own outgoing drag re-entering the window — the app
        // handles in-window drops itself via plain mouse events.
        if self.active_drag.is_some() {
            return;
        }
        let Some(offer) = self.current_offer() else { return };
        let has_uris = offer.with_mime_types(|m| m.iter().any(|t| t == URI_LIST));
        if !has_uris {
            self.incoming_offer = None;
            return;
        }
        offer.accept_mime_type(offer.serial, Some(URI_LIST.to_string()));
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        self.incoming_offer = Some(offer);
        self.drop_pos = (x, y);
        let _ = self.drop_tx.send(DropEvent::Enter { x, y });
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice) {
        if self.incoming_offer.take().is_some() {
            let _ = self.drop_tx.send(DropEvent::Leave);
        }
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64) {
        if self.incoming_offer.is_some() {
            self.drop_pos = (x, y);
            let _ = self.drop_tx.send(DropEvent::Motion { x, y });
        }
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice) {
        let Some(offer) = self.incoming_offer.take() else { return };
        let (x, y) = self.drop_pos;
        match offer.receive(URI_LIST.to_string()) {
            Ok(pipe) => {
                // The source writes once we've returned to the event loop
                // (it needs to see our request first), so read on a side
                // thread rather than blocking dispatch here.
                let tx = self.drop_tx.clone();
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut pipe = pipe;
                    let mut buf = Vec::new();
                    let _ = pipe.read_to_end(&mut buf);
                    let paths = parse_uri_list(&String::from_utf8_lossy(&buf));
                    let _ = tx.send(DropEvent::Dropped { paths, x, y });
                });
                offer.finish();
            }
            Err(e) => {
                eprintln!("og-wayland: receive on drop failed: {e:?}");
                let _ = self.drop_tx.send(DropEvent::Leave);
            }
        }
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice) {}
}

impl DataOfferHandler for State {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _offer: &mut DragOffer, _actions: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _offer: &mut DragOffer, _actions: DndAction) {}
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![SeatState, OutputState];
}

// Never needs to actually do anything with an output (the icon surface
// isn't tied to one) — `delegate_compositor!`'s generated dispatch just
// requires `OutputHandler` to exist.
impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

// Icon surfaces are plain `wl_surface`s committed once and never
// reconfigured (no xdg-shell/layer-shell role), so every compositor
// callback here is a no-op — there's nothing to react to.
impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        self.shm.as_mut().expect("ShmHandler called without a bound Shm")
    }
}

delegate_seat!(State);
delegate_pointer!(State);
delegate_data_device!(State);
delegate_registry!(State);
delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
