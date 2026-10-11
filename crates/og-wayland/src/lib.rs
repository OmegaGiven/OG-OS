//! Real OS-level (cross-application) drag-and-drop of files out of and
//! into an iced window — e.g. dragging a file from og-files onto Brave's
//! upload drop zone, or a file from a browser download bar into og-files.
//!
//! iced/winit have no Wayland drag-and-drop support at all (checked: no
//! `wl_data_source`/`start_drag` anywhere in either crate). The trick,
//! copied from how `smithay-clipboard` implements copy/paste (which iced
//! already depends on transitively): open a *second*, independent
//! `wayland-client` connection wrapping the *same real* Wayland socket
//! winit is already using (`Backend::from_foreign_display`), bind our own
//! `wl_seat`/`wl_pointer`/`wl_keyboard`/`wl_data_device_manager` on it,
//! and use smithay-client-toolkit's `DragSource` to start a real drag.
//!
//! ## Surface and serial tracking
//!
//! `wl_data_device.start_drag` needs (a) the origin surface — winit's
//! window `wl_surface` — and (b) the serial of the button press that
//! started the implicit pointer grab.
//!
//! * Surface: [`spawn_for_window`] is given the raw `wl_surface*` from the
//!   iced window handle and wraps it as a proxy on our connection
//!   (`ObjectId::from_ptr` — same libwayland display, so the object is
//!   valid in requests we send). That works no matter where the pointer
//!   was when we bound. Only the legacy display-only [`spawn_with_drops`]
//!   falls back to the surface seen in our own `wl_pointer`/`wl_keyboard`
//!   enter events.
//! * Serial: the compositor broadcasts input events to every `wl_pointer`
//!   resource a client has bound for a seat, so our own raw `wl_pointer`
//!   sees every button event winit sees, with the same serials. We record
//!   the serial of the press that took the held-button count from 0 to 1
//!   (that is the compositor's grab serial) and forget it on the release
//!   that brings the count back to 0, so a drag is only ever started with
//!   the current button-down's serial — never a stale one. Unlike sctk's
//!   `PointerHandler`, the raw dispatch does not drop button events that
//!   arrive without a preceding `enter`.
//!
//! Drags coming *in* (from other clients, or our own drag coming back)
//! are reported as [`DropEvent`]s with surface-local logical coordinates.

pub mod overlay;

use std::collections::BTreeSet;
use std::ffi::{c_void, CString, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use calloop::channel::{self, Sender};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::data_device_manager::data_device::DataDevice;
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::data_device_manager::data_source::DragSource;
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::data_device_manager::{data_device::DataDeviceHandler, DataDeviceManagerState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shm::{slot::SlotPool, Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_registry, delegate_seat,
    delegate_shm, registry_handlers,
};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{
    wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_keyboard, wl_keyboard::WlKeyboard,
    wl_output, wl_pointer, wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

pub struct DragRequest {
    pub paths: Vec<PathBuf>,
}

/// Sending half for drag requests (so callers needn't depend on calloop).
pub type DragSender = Sender<DragRequest>;

/// What became of a drag we started (reported on the drop receiver).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragOutcome {
    /// The target accepted the drop and finished with the copy action.
    Copied,
    /// The target finished with the move action. Per the protocol the
    /// source "should" now delete the originals — callers decide.
    Moved,
    /// The target finished with the "ask" action (or none reported).
    Finished,
    /// Dropped nowhere / rejected / replaced by another drag.
    Cancelled,
    /// The drag could not be started (no surface, no button held, ...).
    Failed(String),
}

/// Something being dragged *into* our window from another client (a
/// browser, Dolphin, another og-files window...). Coordinates are
/// surface-local logical pixels, same space as iced's cursor position.
#[derive(Debug, Clone)]
pub enum DropEvent {
    Enter { x: f64, y: f64 },
    Motion { x: f64, y: f64 },
    Leave,
    /// The drop happened and the payload (`text/uri-list`, or a
    /// `text/plain` path list as a fallback) was read.
    Dropped { paths: Vec<PathBuf>, x: f64, y: f64 },
    /// Feedback about a drag *out* of the window that we started.
    DragOut(DragOutcome),
}

/// Fixed drag-icon canvas size — big enough to read as an icon, small
/// enough to stay a cursor accessory rather than a window of its own.
const ICON_SIZE: i32 = 56;

pub const URI_LIST: &str = "text/uri-list";
pub const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
pub const TEXT_PLAIN: &str = "text/plain";

/// MIME types offered for a drag out, in preference order.
pub const OFFER_MIME_TYPES: [&str; 3] = [URI_LIST, TEXT_PLAIN_UTF8, TEXT_PLAIN];

/// Picks the MIME type to request from an incoming offer: `text/uri-list`
/// preferred, then plain-text path lists.
pub fn pick_drop_mime(offered: &[String]) -> Option<&'static str> {
    OFFER_MIME_TYPES.into_iter().find(|want| offered.iter().any(|m| m.eq_ignore_ascii_case(want)))
}

// ---------------------------------------------------------------------
// Pure helpers: percent-encoding, URI lists.
// ---------------------------------------------------------------------

/// Percent-encodes a path for the path component of a `file://` URI
/// (RFC 8089 / RFC 3986): keeps unreserved characters and `/`, encodes
/// every other byte (including non-ASCII UTF-8 and non-UTF-8 bytes).
pub fn percent_encode_path(path: &Path) -> String {
    let mut out = String::with_capacity(path.as_os_str().len());
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes to raw bytes; malformed escapes are kept literally.
pub fn percent_decode(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `/home/me/a b.txt` → `file:///home/me/a%20b.txt`.
pub fn file_uri(path: &Path) -> String {
    format!("file://{}", percent_encode_path(path))
}

/// RFC 2483 `text/uri-list`: one URI per line, CRLF-terminated.
pub fn build_uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| file_uri(p) + "\r\n").collect()
}

/// Plain newline-separated paths for `text/plain` targets (terminals,
/// editors, chat boxes).
pub fn build_plain_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n")
}

/// One `file:` URI → local path. Accepts `file:///p`, `file://localhost/p`
/// and the minimal `file:/p`; rejects other schemes and remote hosts.
pub fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    let scheme_end = uri.find(':')?;
    if !uri[..scheme_end].eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = &uri[scheme_end + 1..];
    let path = if let Some(after) = rest.strip_prefix("//") {
        let slash = after.find('/')?;
        let host = &after[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        &after[slash..]
    } else if rest.starts_with('/') {
        rest
    } else {
        return None;
    };
    Some(PathBuf::from(OsString::from_vec(percent_decode(path))))
}

/// `file:///home/me/a%20b.txt` lines → paths. Skips blank lines, `#`
/// comments and non-file URIs (a browser dragging a link gives
/// `https://...`, not a file).
pub fn parse_uri_list(data: &str) -> Vec<PathBuf> {
    data.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(parse_file_uri)
        .collect()
}

/// `text/plain` fallback: each line is either a `file:` URI or an
/// absolute path; anything else (prose, relative paths, URLs) is skipped.
pub fn parse_plain_list(data: &str) -> Vec<PathBuf> {
    data.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            if l.starts_with('/') {
                Some(PathBuf::from(l))
            } else {
                parse_file_uri(l.trim())
            }
        })
        .collect()
}

/// Parses a received payload according to the MIME type it was read as.
pub fn parse_payload(mime: &str, data: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(data);
    if mime.eq_ignore_ascii_case(URI_LIST) {
        parse_uri_list(&text)
    } else {
        parse_plain_list(&text)
    }
}

// ---------------------------------------------------------------------
// Window handle plumbing.
// ---------------------------------------------------------------------

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

/// Raw `wl_display*` and `wl_surface*` of an iced window, as plain
/// integers so they can cross thread boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPtrs {
    pub display: usize,
    pub surface: usize,
}

/// Extracts the raw Wayland surface and display pointers from an iced
/// `WindowHandle`. `None` on non-Wayland backends or FFI failure.
pub fn window_ptrs_from_handle(handle: iced::window::raw_window_handle::WindowHandle<'_>) -> Option<WindowPtrs> {
    use iced::window::raw_window_handle::RawWindowHandle;
    let RawWindowHandle::Wayland(wh) = handle.as_raw() else { return None };
    let surface_ptr = wh.surface.as_ptr();
    let display_ptr = unsafe { display_from_surface(surface_ptr) }?;
    Some(WindowPtrs { display: display_ptr as usize, surface: surface_ptr as usize })
}

/// Extracts the raw Wayland surface pointer from an iced `WindowHandle`,
/// then resolves it to the real `wl_display*`. Returns `None` on any
/// non-Wayland backend (e.g. if this ever runs under X11) or FFI failure.
pub fn display_ptr_from_window_handle(handle: iced::window::raw_window_handle::WindowHandle<'_>) -> Option<usize> {
    window_ptrs_from_handle(handle).map(|p| p.display)
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
///
/// Legacy entry point: without the window's surface pointer the drag
/// origin falls back to pointer/keyboard focus seen on our own seat
/// binding. Prefer [`spawn_for_window`]. Our own drag coming back into
/// the window is ignored here (the old behaviour).
pub fn spawn_with_drops(display_ptr: usize) -> (Sender<DragRequest>, std::sync::mpsc::Receiver<DropEvent>) {
    spawn_inner(display_ptr, None, false)
}

/// Spawns the worker for a specific window (see [`window_ptrs_from_handle`]).
/// The window's own surface is used as drag origin, so drags work even if
/// the pointer was already over the window when we connected. A drag we
/// started that comes back into the window is reported as a regular
/// Enter/Motion/Dropped — while our drag is active the compositor holds
/// the pointer grab and the toolkit never sees the button release.
///
/// The window must outlive the connection (true for an app's main window).
pub fn spawn_for_window(ptrs: WindowPtrs) -> (Sender<DragRequest>, std::sync::mpsc::Receiver<DropEvent>) {
    spawn_inner(ptrs.display, Some(ptrs.surface), true)
}

fn spawn_inner(
    display_ptr: usize,
    surface_ptr: Option<usize>,
    report_own_drags: bool,
) -> (Sender<DragRequest>, std::sync::mpsc::Receiver<DropEvent>) {
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
            // Safety: the pointer is winit's live `wl_surface` proxy on this
            // same display; `from_ptr` checks the interface name, and for a
            // wayland-rs managed proxy (winit's) tracks its liveness, so a
            // destroyed window makes requests using it fail, not crash.
            let window_surface = surface_ptr.and_then(|p| {
                let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), p as *mut _) }.ok()?;
                WlSurface::from_id(&connection, id).ok()
            });
            worker(connection, rx, drop_tx, window_surface, report_own_drags);
        })
        .expect("spawn og-wayland-drag thread");
    (tx, drop_rx)
}

fn worker(
    connection: Connection,
    rx: channel::Channel<DragRequest>,
    drop_tx: std::sync::mpsc::Sender<DropEvent>,
    window_surface: Option<WlSurface>,
    report_own_drags: bool,
) {
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
    // just without a cursor icon, rather than losing drag-and-drop
    // entirely over what's a purely cosmetic addition.
    let compositor = CompositorState::bind(&globals, &qh)
        .inspect_err(|e| eprintln!("og-wayland: CompositorState::bind failed, drag icon disabled: {e:?}"))
        .ok();
    let shm = Shm::bind(&globals, &qh)
        .inspect_err(|e| eprintln!("og-wayland: Shm::bind failed, drag icon disabled: {e:?}"))
        .ok();

    let mut event_loop: EventLoop<State> = EventLoop::try_new().expect("calloop init");
    let (finish_tx, finish_rx) = channel::channel::<(DragOffer, DndAction)>();
    event_loop
        .handle()
        .insert_source(finish_rx, |event, _, _: &mut State| {
            if let channel::Event::Msg((offer, selected)) = event {
                // `finish` is a protocol error (fatal for the whole shared
                // connection, i.e. the app) unless an action was selected.
                if !selected.is_empty() && selected != DndAction::Ask {
                    offer.finish();
                }
                offer.destroy();
            }
        })
        .expect("insert finish channel");
    let mut state = State {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        data_device_manager_state,
        compositor,
        shm,
        seat: None,
        pointer: None,
        keyboard: None,
        data_device: None,
        window_surface,
        report_own_drags,
        pointer_focus: None,
        keyboard_focus: None,
        buttons_held: BTreeSet::new(),
        press_serial: None,
        active_drag: None,
        drop_tx,
        drop_pos: (0.0, 0.0),
        incoming_offer: None,
        finish_tx,
    };

    WaylandSource::new(connection, event_queue).insert(event_loop.handle()).expect("insert wayland source");

    event_loop
        .handle()
        .insert_source(rx, move |event, _, state: &mut State| {
            if let channel::Event::Msg(DragRequest { paths }) = event {
                if let Err(reason) = state.start_drag(&qh, paths) {
                    eprintln!("og-wayland: drag not started: {reason}");
                    let _ = state.drop_tx.send(DropEvent::DragOut(DragOutcome::Failed(reason)));
                }
            }
        })
        .expect("insert drag-request channel");

    loop {
        if event_loop.dispatch(None, &mut state).is_err() {
            break;
        }
    }
}

/// A drag we started. Dropping the `DragSource` cancels it, so it lives
/// here for the drag's duration together with the payloads to hand out
/// on `send`, the last action the compositor negotiated, and (if
/// compositor/shm bound OK) the icon surface + its backing pool, which
/// likewise must outlive the drag or the compositor loses its buffer.
struct ActiveDrag {
    source: DragSource,
    uri_list: Vec<u8>,
    plain: Vec<u8>,
    action: DndAction,
    _icon: Option<(WlSurface, SlotPool)>,
}

/// An offer currently hovering us, with the MIME type we'll read it as.
struct IncomingOffer {
    offer: DragOffer,
    mime: &'static str,
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
    keyboard: Option<WlKeyboard>,
    data_device: Option<DataDevice>,
    /// winit's window surface, if the caller gave us its pointer.
    window_surface: Option<WlSurface>,
    report_own_drags: bool,
    /// Fallback origin surfaces from our own seat binding.
    pointer_focus: Option<WlSurface>,
    keyboard_focus: Option<WlSurface>,
    /// Buttons currently held, per our own `wl_pointer`.
    buttons_held: BTreeSet<u32>,
    /// Serial of the press that started the current implicit grab.
    press_serial: Option<u32>,
    active_drag: Option<ActiveDrag>,
    drop_tx: std::sync::mpsc::Sender<DropEvent>,
    /// Last surface-local position of an incoming drag.
    drop_pos: (f64, f64),
    incoming_offer: Option<IncomingOffer>,
    /// Dropped offers come back here once their payload has been read,
    /// to be finished and destroyed on the worker thread.
    finish_tx: Sender<(DragOffer, DndAction)>,
}

impl State {
    /// Builds a small drag-icon surface: one "page" rectangle for a single
    /// file, three fanned-out ones for a multi-item drag. `None` on any
    /// bind/allocation failure — callers then drag with no icon.
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

    fn origin_surface(&self) -> Option<WlSurface> {
        self.window_surface
            .clone()
            .filter(|s| s.is_alive())
            .or_else(|| self.pointer_focus.clone().filter(|s| s.is_alive()))
            .or_else(|| self.keyboard_focus.clone().filter(|s| s.is_alive()))
    }

    fn ensure_data_device(&mut self, qh: &QueueHandle<Self>) {
        if self.data_device.is_none() {
            if let Some(seat) = &self.seat {
                self.data_device = Some(self.data_device_manager_state.get_data_device(qh, seat));
            }
        }
    }

    fn start_drag(&mut self, qh: &QueueHandle<Self>, paths: Vec<PathBuf>) -> Result<(), String> {
        let surface = self.origin_surface().ok_or("window surface unknown")?;
        let serial = self.press_serial.ok_or("no pointer button held (drag must start during a press)")?;
        self.ensure_data_device(qh);
        if self.data_device.is_none() {
            return Err("no seat".into());
        }

        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter_map(|p| p.canonicalize().ok().or_else(|| p.is_absolute().then_some(p)))
            .collect();
        if paths.is_empty() {
            return Err("no draggable paths".into());
        }

        // Replacing an older drag: report it as cancelled first.
        if self.active_drag.take().is_some() {
            let _ = self.drop_tx.send(DropEvent::DragOut(DragOutcome::Cancelled));
        }

        // Built before borrowing `data_device` below — needs `&mut self`.
        let icon = self.build_icon_surface(qh, paths.len());
        let icon_surface_ref = icon.as_ref().map(|(s, _)| s);

        let Some(device) = &self.data_device else { return Err("no data device".into()) };
        // A source can't express a preference — the destination picks via
        // `set_actions(.., preferred)`; we accept Copy (what everyone
        // prefers for files) or Move.
        let source = self.data_device_manager_state.create_drag_and_drop_source(
            qh,
            OFFER_MIME_TYPES,
            DndAction::Copy | DndAction::Move,
        );
        source.start_drag(device, &surface, icon_surface_ref, serial);
        // The compositor's drag grab swallows the matching release (no
        // button event reaches any client), so forget the press now or the
        // held-button set would stay non-empty and pin a stale serial.
        self.buttons_held.clear();
        self.press_serial = None;
        self.active_drag = Some(ActiveDrag {
            source,
            uri_list: build_uri_list(&paths).into_bytes(),
            plain: build_plain_list(&paths).into_bytes(),
            action: DndAction::empty(),
            _icon: icon,
        });
        Ok(())
    }

    fn is_our_source(&self, source: &WlDataSource) -> bool {
        self.active_drag.as_ref().is_some_and(|d| d.source.inner() == source)
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


impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface, .. } => state.pointer_focus = Some(surface),
            wl_pointer::Event::Leave { surface, .. } => {
                if state.pointer_focus.as_ref() == Some(&surface) {
                    state.pointer_focus = None;
                }
            }
            wl_pointer::Event::Button { serial, button, state: WEnum::Value(button_state), .. } => match button_state {
                wl_pointer::ButtonState::Pressed => {
                    // The press that takes the held count 0 → 1 starts the
                    // implicit grab; its serial is the one start_drag needs.
                    if state.buttons_held.is_empty() {
                        state.press_serial = Some(serial);
                    }
                    state.buttons_held.insert(button);
                }
                wl_pointer::ButtonState::Released => {
                    state.buttons_held.remove(&button);
                    if state.buttons_held.is_empty() {
                        state.press_serial = None;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // Only focus matters (fallback drag origin); the keymap fd is
        // closed when the event is dropped.
        match event {
            wl_keyboard::Event::Enter { surface, .. } => state.keyboard_focus = Some(surface),
            wl_keyboard::Event::Leave { surface, .. } => {
                if state.keyboard_focus.as_ref() == Some(&surface) {
                    state.keyboard_focus = None;
                }
            }
            _ => {}
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
            self.ensure_data_device(qh);
        }
    }

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, capability: Capability) {
        // Seats present at startup never go through `new_seat`.
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
        }
        if self.seat.as_ref() != Some(&seat) {
            return;
        }
        // Created eagerly so drops *into* the window work before any drag out.
        self.ensure_data_device(qh);
        match capability {
            Capability::Pointer if self.pointer.is_none() => self.pointer = Some(seat.get_pointer(qh, ())),
            Capability::Keyboard if self.keyboard.is_none() => self.keyboard = Some(seat.get_keyboard(qh, ())),
            _ => {}
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, capability: Capability) {
        match capability {
            Capability::Pointer => {
                if let Some(pointer) = self.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release();
                    }
                }
                self.pointer_focus = None;
                self.buttons_held.clear();
                self.press_serial = None;
            }
            Capability::Keyboard => {
                if let Some(keyboard) = self.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release();
                    }
                }
                self.keyboard_focus = None;
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl DataSourceHandler for State {
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, mime: String, write_pipe: WritePipe) {
        let Some(drag) = self.active_drag.as_ref().filter(|d| d.source.inner() == source) else { return };
        let content = if mime.eq_ignore_ascii_case(URI_LIST) {
            drag.uri_list.clone()
        } else if mime.eq_ignore_ascii_case(TEXT_PLAIN_UTF8) || mime.eq_ignore_ascii_case(TEXT_PLAIN) {
            drag.plain.clone()
        } else {
            return;
        };
        // Written off the event loop: a slow (or same-process) reader
        // must not stall dispatch.
        std::thread::spawn(move || {
            use std::io::Write;
            let mut pipe = write_pipe;
            let _ = pipe.write_all(&content);
        });
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.is_our_source(source) {
            self.active_drag = None;
            let _ = self.drop_tx.send(DropEvent::DragOut(DragOutcome::Cancelled));
        }
    }

    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &WlDataSource, _mime: Option<String>) {}

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _source: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if !self.is_our_source(source) {
            return;
        }
        let action = self.active_drag.take().map(|d| d.action).unwrap_or_else(DndAction::empty);
        let outcome = if action.contains(DndAction::Move) {
            DragOutcome::Moved
        } else if action.contains(DndAction::Copy) {
            DragOutcome::Copied
        } else {
            DragOutcome::Finished
        };
        let _ = self.drop_tx.send(DropEvent::DragOut(outcome));
    }

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, action: DndAction) {
        if let Some(drag) = self.active_drag.as_mut().filter(|d| d.source.inner() == source) {
            drag.action = action;
        }
    }
}

impl State {
    fn current_offer(&self) -> Option<DragOffer> {
        self.data_device.as_ref().and_then(|d| d.data().drag_offer())
    }
}

impl DataDeviceHandler for State {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _device: &wayland_client::protocol::wl_data_device::WlDataDevice, x: f64, y: f64, _surface: &WlSurface) {
        self.incoming_offer = None;
        // Legacy mode ignores our own outgoing drag re-entering the window.
        if self.active_drag.is_some() && !self.report_own_drags {
            return;
        }
        let Some(offer) = self.current_offer() else { return };
        let Some(mime) = offer.with_mime_types(pick_drop_mime) else {
            offer.accept_mime_type(offer.serial, None);
            return;
        };
        offer.accept_mime_type(offer.serial, Some(mime.to_string()));
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        self.incoming_offer = Some(IncomingOffer { offer, mime });
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
        let Some(IncomingOffer { offer, mime }) = self.incoming_offer.take() else { return };
        let (x, y) = self.drop_pos;
        // Fresh snapshot for the negotiated action (the stored clone is
        // from `enter`).
        let selected = self.current_offer().map(|o| o.selected_action).unwrap_or(offer.selected_action);
        match offer.receive(mime.to_string()) {
            Ok(pipe) => {
                // The source writes once our request reaches it, so read on a
                // side thread rather than blocking dispatch here; finish only
                // after the read so the source keeps its data until then.
                let tx = self.drop_tx.clone();
                let finish_tx = self.finish_tx.clone();
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut pipe = pipe;
                    let mut buf = Vec::new();
                    let _ = pipe.read_to_end(&mut buf);
                    let paths = parse_payload(mime, &buf);
                    let _ = tx.send(DropEvent::Dropped { paths, x, y });
                    let _ = finish_tx.send((offer, selected));
                });
            }
            Err(e) => {
                eprintln!("og-wayland: receive on drop failed: {e:?}");
                offer.destroy();
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
delegate_data_device!(State);
delegate_registry!(State);
delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_keeps_unreserved_and_slash() {
        assert_eq!(percent_encode_path(Path::new("/home/me/a-b_c.d~e")), "/home/me/a-b_c.d~e");
    }

    #[test]
    fn encode_spaces_unicode_and_reserved() {
        assert_eq!(percent_encode_path(Path::new("/tmp/a b.txt")), "/tmp/a%20b.txt");
        assert_eq!(percent_encode_path(Path::new("/tmp/é")), "/tmp/%C3%A9");
        assert_eq!(percent_encode_path(Path::new("/tmp/50%#?&+")), "/tmp/50%25%23%3F%26%2B");
    }

    #[test]
    fn encode_non_utf8_bytes() {
        let p = PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()));
        assert_eq!(percent_encode_path(&p), "/tmp/%FF");
    }

    #[test]
    fn decode_roundtrip_and_malformed() {
        assert_eq!(percent_decode("/a%20b%c3%A9"), "/a bé".as_bytes());
        assert_eq!(percent_decode("100%"), b"100%");
        assert_eq!(percent_decode("%zz%4"), b"%zz%4");
        let p = PathBuf::from(OsString::from_vec(b"/x y/\xfe\xc3\xa9#".to_vec()));
        assert_eq!(percent_decode(&percent_encode_path(&p)), p.as_os_str().as_bytes());
    }

    #[test]
    fn build_uri_list_is_crlf_terminated_and_encoded() {
        let paths = vec![PathBuf::from("/a b"), PathBuf::from("/c")];
        assert_eq!(build_uri_list(&paths), "file:///a%20b\r\nfile:///c\r\n");
        assert_eq!(build_plain_list(&paths), "/a b\n/c");
    }

    #[test]
    fn parse_uri_list_variants() {
        let data = "# comment\r\nfile:///a%20b\r\nfile://localhost/c/d\r\nhttps://x.org/y\r\n\r\nfile://otherhost/z\r\nFILE:/e%C3%A9\n  file:///trim  \n";
        assert_eq!(
            parse_uri_list(data),
            vec![
                PathBuf::from("/a b"),
                PathBuf::from("/c/d"),
                PathBuf::from("/eé"),
                PathBuf::from("/trim"),
            ]
        );
    }

    #[test]
    fn uri_list_roundtrip() {
        let paths = vec![PathBuf::from("/home/me/Ünïcode dir/file #1.txt"), PathBuf::from("/tmp/100%.png")];
        assert_eq!(parse_uri_list(&build_uri_list(&paths)), paths);
    }

    #[test]
    fn parse_plain_list_variants() {
        let data = "/home/me/a b.txt\r\nfile:///c%20d\nrelative/path\nhello world\n\n";
        assert_eq!(parse_plain_list(data), vec![PathBuf::from("/home/me/a b.txt"), PathBuf::from("/c d")]);
    }

    #[test]
    fn parse_payload_dispatches_on_mime() {
        assert_eq!(parse_payload(URI_LIST, b"file:///a%20b\r\n"), vec![PathBuf::from("/a b")]);
        assert_eq!(parse_payload(TEXT_PLAIN, b"/a%20b"), vec![PathBuf::from("/a%20b")]);
    }

    #[test]
    fn offer_mime_list_and_pick() {
        assert_eq!(OFFER_MIME_TYPES, ["text/uri-list", "text/plain;charset=utf-8", "text/plain"]);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_drop_mime(&s(&["text/plain", "text/uri-list"])), Some(URI_LIST));
        assert_eq!(pick_drop_mime(&s(&["TEXT/PLAIN;charset=UTF-8", "text/plain"])), Some(TEXT_PLAIN_UTF8));
        assert_eq!(pick_drop_mime(&s(&["text/plain"])), Some(TEXT_PLAIN));
        assert_eq!(pick_drop_mime(&s(&["image/png", "text/html"])), None);
    }
}
