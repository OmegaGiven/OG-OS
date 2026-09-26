//! StatusNotifierItem tray — the actual point of the step-8 spike
//! (src/bin/tray_spike.rs): apps like Steam/Discord that keep running with
//! just a tray icon after their window closes. This is the module that
//! actually renders those icons and lets you click them, not Taskbar
//! (which only shows windows that are still open).
//!
//! Right-click renders the item's real DBusMenu (com.canonical.dbusmenu)
//! tree as a popup, flattened with depth-based indentation rather than a
//! true expand/collapse submenu widget — most tray menus (Discord/Steam/
//! etc) are shallow enough that this reads fine, and it avoids building a
//! whole nested-popup state machine for what's usually 1-2 levels deep.

use iced::widget::{button, column, container, image, mouse_area, text};
use iced::{Background, Border, Color, Element, Length, Subscription};

use system_tray::client::{ActivateRequest, Client};
use system_tray::item::IconPixmap;
use system_tray::menu::{MenuItem, MenuType, TrayMenu};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

/// Icons carry a ready-made `image::Handle`, built once per tray update
/// rather than in `view()`: every new Handle gets a fresh id, so building
/// them per frame meant the renderer re-decoded every icon on every redraw
/// and never hit its raster cache.
#[derive(Debug, Clone)]
pub enum TrayIcon {
    /// From the item's own ARGB pixmap, or from an icon theme file (PNG
    /// bytes; iced::widget::image sniffs the format).
    Image(image::Handle),
    /// Nothing usable was found — fall back to a generic glyph rather than
    /// showing nothing at all.
    Glyph,
}

#[derive(Debug, Clone)]
pub struct TrayItem {
    pub address: String,
    pub icon: TrayIcon,
    /// DBus object path implementing com.canonical.dbusmenu — `None` means
    /// this item never advertised a context menu, so right-click has
    /// nothing to show (left-click Activate still works either way).
    pub menu_path: Option<String>,
    pub menu: Option<TrayMenu>,
}

/// Own type (not `system_tray::menu::MenuItem` directly) so `PopupKind`
/// can derive `PartialEq`/`Eq` — the real DBusMenu types don't, and
/// deriving those by hand across a recursive third-party struct isn't
/// worth it when the popup only ever needs these five fields anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuEntry {
    pub submenu_id: i32,
    pub label: String,
    pub enabled: bool,
    pub is_separator: bool,
    pub depth: u32,
}

/// Flattens a DBusMenu tree (recursive, arbitrary depth) into a flat list
/// with each item's original nesting depth preserved for indentation —
/// resolved once when the popup opens, same convention as
/// `WindowMenu`'s `other_workspaces` (a snapshot, not kept live).
fn flatten_menu(items: &[MenuItem], depth: u32, out: &mut Vec<TrayMenuEntry>) {
    for item in items {
        if !item.visible {
            continue;
        }
        out.push(TrayMenuEntry {
            submenu_id: item.id,
            label: item.label.clone().unwrap_or_default(),
            enabled: item.enabled,
            is_separator: item.menu_type == MenuType::Separator,
            depth,
        });
        if !item.submenu.is_empty() {
            flatten_menu(&item.submenu, depth + 1, out);
        }
    }
}

pub fn flatten_tray_menu(menu: &TrayMenu) -> Vec<TrayMenuEntry> {
    let mut out = Vec::new();
    flatten_menu(&menu.submenus, 0, &mut out);
    out
}

/// SNI icon pixmaps are ARGB32 in network byte order (A,R,G,B per pixel) —
/// iced's Handle::from_rgba wants R,G,B,A. Picks the largest available
/// size since a tray icon this small never needs more than that anyway.
fn pixmap_to_icon(pixmaps: &[IconPixmap]) -> Option<TrayIcon> {
    let biggest = pixmaps.iter().max_by_key(|p| p.width * p.height)?;
    if biggest.width <= 0 || biggest.height <= 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(biggest.pixels.len());
    for chunk in biggest.pixels.chunks_exact(4) {
        let [a, r, g, b] = [chunk[0], chunk[1], chunk[2], chunk[3]];
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Some(TrayIcon::Image(image::Handle::from_rgba(biggest.width as u32, biggest.height as u32, rgba)))
}

/// Shallow (2-level) search for `{icon_name}.png` under a directory —
/// icon themes nest by size/category (e.g. `48x48/apps/name.png`) and
/// there's no single fixed path, so this just looks rather than guessing
/// one exact layout.
fn find_icon_file(root: &std::path::Path, icon_name: &str, depth: u32) -> Option<std::path::PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.file_stem().and_then(|s| s.to_str()) == Some(icon_name) && path.extension().and_then(|e| e.to_str()) == Some("png") {
            return Some(path);
        }
    }
    for dir in subdirs {
        if let Some(found) = find_icon_file(&dir, icon_name, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// Theme-file lookups keyed by (icon name, theme path). The directory walk
/// in `find_icon_file` is far too slow to repeat on every tray event (apps
/// like Steam/Discord emit them constantly), and the answer never changes
/// for a given name. Caching the Handle also keeps its id stable, so the
/// renderer's image cache keeps hitting across updates.
type IconFileCache = std::collections::HashMap<(String, Option<String>), TrayIcon>;

fn resolve_icon(
    icon_name: Option<&str>,
    icon_theme_path: Option<&str>,
    pixmaps: Option<&[IconPixmap]>,
    file_cache: &mut IconFileCache,
) -> TrayIcon {
    if let Some(pixmaps) = pixmaps {
        if let Some(icon) = pixmap_to_icon(pixmaps) {
            return icon;
        }
    }
    let Some(name) = icon_name else { return TrayIcon::Glyph };
    file_cache
        .entry((name.to_string(), icon_theme_path.map(str::to_string)))
        .or_insert_with(|| lookup_icon_file(name, icon_theme_path))
        .clone()
}

fn lookup_icon_file(name: &str, icon_theme_path: Option<&str>) -> TrayIcon {
    let search_roots: Vec<std::path::PathBuf> = icon_theme_path
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(["/usr/share/icons/hicolor", "/usr/share/pixmaps"].map(std::path::PathBuf::from))
        .collect();
    for root in search_roots {
        if let Some(path) = find_icon_file(&root, name, 3) {
            if let Ok(bytes) = std::fs::read(&path) {
                return TrayIcon::Image(image::Handle::from_bytes(bytes));
            }
        }
    }
    TrayIcon::Glyph
}

/// Pure snapshot -> Vec<TrayItem>, given an already-connected client's own
/// item map — no dbus I/O here. The map key is the item's DBus bus address
/// (e.g. ":1.370"), needed for Activate; `item.id` is a separate, app-
/// chosen SNI id (e.g. "steam") that isn't usable for that.
fn snapshot_items(items: &system_tray::data::BaseMap, file_cache: &mut IconFileCache) -> Vec<TrayItem> {
    items
        .iter()
        .map(|(address, (item, menu))| TrayItem {
            address: address.clone(),
            icon: resolve_icon(
                item.icon_name.as_deref(),
                item.icon_theme_path.as_deref(),
                item.icon_pixmap.as_deref(),
                file_cache,
            ),
            menu_path: item.menu.clone(),
            menu: menu.clone(),
        })
        .collect()
}

pub struct Tray {
    items: Vec<TrayItem>,
    // `snapshot_items` walks `system_tray`'s own `BaseMap`, which is a
    // plain `HashMap` — its iteration order isn't stable and can silently
    // change on any update even with the same items present, which read as
    // every icon reshuffling position on each tray event instead of a new
    // one just joining the end. This is the fix: a stable append-order by
    // address, independent of whatever order the map happens to yield.
    order: Vec<String>,
    hovered_address: Option<String>,
}

impl Tray {
    pub fn new() -> Self {
        Self { items: Vec::new(), order: Vec::new(), hovered_address: None }
    }

    /// Items in `order` (stable), for any address still present in the
    /// latest snapshot — `order` itself is kept in sync in `update()`.
    fn ordered_items(&self) -> Vec<&TrayItem> {
        self.order.iter().filter_map(|addr| self.items.iter().find(|i| &i.address == addr)).collect()
    }
}

impl Module for Tray {
    fn cell_length(&self, size: u32, _orientation: Orientation) -> Length {
        // Fixed, not Shrink. Tray items arrive async over dbus — typically
        // *after* the first paint (hence "looks fine for a second, then
        // the rest of the section vanishes") — so a Shrink cell's size
        // actually changes at runtime the moment the first item lands.
        // That's the one thing every other module here avoids (they're
        // all a fixed size from the start) and it lines up exactly with a
        // real rendering corruption in this app's layer-shell surface:
        // the section's layout doesn't recover from that resize. A cell
        // that's the same fixed size for its entire lifetime sidesteps
        // the trigger entirely. Reserves room for a handful of icons —
        // more than that scrolls within this slot instead of growing it.
        Length::Fixed((size as f32 + 2.0) * 3.0)
    }

    fn view(&self, colors: AppColors, size: u32, orientation: Orientation) -> Element<'_, Message> {
        let cells: Vec<Element<Message>> = self
            .ordered_items()
            .into_iter()
            .map(|item| {
                let content: Element<Message> = match &item.icon {
                    TrayIcon::Image(handle) => image(handle.clone()).width(16).height(16).into(),
                    TrayIcon::Glyph => {
                        let fg = colors.text;
                        text("\u{f2d0}").size(14).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(fg) }).into()
                    }
                };
                let hovered = self.hovered_address.as_deref() == Some(item.address.as_str());
                let bg = if hovered { colors.header_btn_bg } else { Color::TRANSPARENT };
                let address = item.address.clone();
                let right_click_address = item.address.clone();
                let enter_address = item.address.clone();
                let exit_address = item.address.clone();
                let menu_entries = item.menu.as_ref().map(flatten_tray_menu);
                let menu_path = item.menu_path.clone();

                // mouse_area, not button — needs on_right_press (real
                // DBusMenu context menu) alongside left-click Activate,
                // same trade-off as the settings/pulseaudio modules: no
                // free hover styling from button::Style, tracked by hand
                // instead via on_enter/on_exit + `hovered_address`.
                let btn = mouse_area(
                    container(
                        container(content).width(size as u16).height(size as u16).center_x(Length::Fill).center_y(Length::Fill),
                    )
                    .style(move |_| container::Style {
                        background: Some(Background::Color(bg)),
                        border: Border { radius: colors.radius.into(), ..Default::default() },
                        ..Default::default()
                    }),
                )
                .on_press(Message::TrayActivate(address))
                .on_enter(Message::TrayHover(enter_address, true))
                .on_exit(Message::TrayHover(exit_address, false));

                let btn = match (menu_path, menu_entries) {
                    (Some(menu_path), Some(entries)) if !entries.is_empty() => {
                        btn.on_right_press(Message::TrayContextMenu(right_click_address, menu_path, entries))
                    }
                    _ => btn,
                };

                // Was wrapped in `iced::widget::tooltip` to show `item.title`
                // on hover — this app runs on iced_layershell (a layer-shell
                // surface, not a normal window), where tooltip's floating
                // overlay doesn't actually float: it rendered inline instead,
                // eating real layout space and shoving every module after
                // Tray in the same section out of place the instant any tray
                // item had a title (i.e. always). No other module in this
                // codebase uses `tooltip` — this was the only one, and it's
                // why. Plain icon, no tooltip, same as everything else.
                btn.into()
            })
            .collect();

        // Plain row/column, no scrollable — this is just a place for a
        // handful of minimized-to-tray app icons plus a right-click menu,
        // nothing fancier. `scrollable` here previously left icons with
        // valid pixel data (confirmed via tray-spike) that never actually
        // painted, and a stray accent-colored scrollbar-thumb sliver in
        // the corner instead — an ambiguous/collapsed viewport, not
        // something worth carrying the complexity of a scroll area to
        // work around. The fixed-size cell (see `cell_length`) is what
        // actually matters: it keeps this module from resizing when the
        // first tray item lands after the initial paint (dbus is async),
        // which is what previously corrupted this layer-shell surface's
        // rendering for the whole section.
        match orientation {
            Orientation::Horizontal => iced::widget::row(cells).spacing(2).into(),
            Orientation::Vertical => iced::widget::column(cells).spacing(2).into(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with_id("tray", tray_stream())
    }

    fn update(&mut self, message: &Message) {
        if let Message::TrayUpdated(items) = message {
            // Drop addresses no longer present, then append any new ones
            // at the end — existing icons keep their slot, new ones just
            // join the tail of the row instead of the whole row
            // re-sorting to whatever order the HashMap yielded this time.
            self.order.retain(|addr| items.iter().any(|i| &i.address == addr));
            for item in items {
                if !self.order.contains(&item.address) {
                    self.order.push(item.address.clone());
                }
            }
            self.items = items.clone();
        }
        if let Message::TrayHover(address, entering) = message {
            if *entering {
                self.hovered_address = Some(address.clone());
            } else if self.hovered_address.as_deref() == Some(address.as_str()) {
                self.hovered_address = None;
            }
        }
    }
}

/// One tray client for the whole process. `Client::new()` opens its own
/// D-Bus connection, registers a new StatusNotifierHost and spawns
/// background tasks that are *not* stopped when the Client is dropped — so
/// the old per-click `Client::new()` in activate()/activate_menu_item()
/// leaked a live connection + tasks on every tray click.
static CLIENT: tokio::sync::OnceCell<Client> = tokio::sync::OnceCell::const_new();

async fn client() -> Option<&'static Client> {
    CLIENT.get_or_try_init(Client::new).await.ok()
}

fn tray_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(16, |mut sender| async move {
        use iced::futures::SinkExt;

        let Some(client) = client().await else { return };
        let mut rx = client.subscribe();
        let items = client.items();
        let mut file_cache = IconFileCache::new();

        // Real bug found while testing this: creating a *second* fresh
        // Client (with its own short sleep-then-read) on every fetch,
        // instead of reusing this already-connected one, meant every
        // refresh raced a brand-new connection's own replay from the
        // watcher — usually losing that race, so nothing ever rendered.
        // Reading straight from this client's own `items()` handle avoids
        // that: it only ever reflects events *this* connection actually
        // received.
        let snapshot = { snapshot_items(&items.lock().unwrap(), &mut file_cache) };
        let _ = sender.send(Message::TrayUpdated(snapshot)).await;

        // `broadcast::Receiver::recv()` returns `Err(Lagged(n))` when this
        // receiver fell behind and the channel dropped messages for it —
        // recoverable, not the channel closing. The real bug: treating
        // every `Err` as "stop" (`while rx.recv().await.is_ok()`) meant a
        // single dropped broadcast — entirely plausible during the burst
        // of activity when several apps register tray icons around
        // startup — silently killed this loop forever. Everything after
        // that point (Steam, Discord, ...) would register fine with the
        // watcher but never reach the UI, because nothing was listening
        // anymore. `Closed` is the only case that should actually end the
        // stream; `Lagged` just means "re-sync and keep going."
        loop {
            match rx.recv().await {
                Ok(_) => {
                    let snapshot = { snapshot_items(&items.lock().unwrap(), &mut file_cache) };
                    let _ = sender.send(Message::TrayUpdated(snapshot)).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let snapshot = { snapshot_items(&items.lock().unwrap(), &mut file_cache) };
                    let _ = sender.send(Message::TrayUpdated(snapshot)).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

pub async fn activate(address: String) {
    if let Some(client) = client().await {
        let _ = client.activate(ActivateRequest::Default { address, x: 0, y: 0 }).await;
    }
}

pub async fn activate_menu_item(address: String, menu_path: String, submenu_id: i32) {
    if let Some(client) = client().await {
        let _ = client.activate(ActivateRequest::MenuItem { address, menu_path, submenu_id }).await;
    }
}

/// Flattened DBusMenu popup — a plain button per entry (disabled ones
/// still rendered, just dimmed and non-interactive, so the menu's shape
/// matches what the app actually sent), indented by nesting depth, with
/// separators as a thin rule instead of a row.
pub fn popup_view(colors: AppColors, address: &str, menu_path: &str, entries: &[TrayMenuEntry]) -> Element<'static, Message> {
    let rows: Vec<Element<'static, Message>> = entries
        .iter()
        .map(|entry| {
            if entry.is_separator {
                return container(iced::widget::horizontal_rule(1))
                    .padding([4, 8])
                    .into();
            }

            // Owned locals, not `entry` itself, captured by the style
            // closures below — capturing a `&TrayMenuEntry` would tie
            // this `Element` to `entries`' borrow instead of `'static`.
            let enabled = entry.enabled;
            let depth = entry.depth;
            let label = entry.label.clone();
            let submenu_id = entry.submenu_id;
            let fg = if enabled { colors.text } else { colors.dim_text };

            let label_row = button(
                text(label).size(13).style(move |_| text::Style { color: Some(fg) }),
            )
            .width(Length::Fill)
            .padding([6, 8 + depth as u16 * 14])
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if matches!(status, button::Status::Hovered) && enabled {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                text_color: fg,
                ..Default::default()
            });

            if enabled {
                label_row
                    .on_press(Message::TrayMenuItemActivate(address.to_string(), menu_path.to_string(), submenu_id))
                    .into()
            } else {
                label_row.into()
            }
        })
        .collect();

    container(column(rows).spacing(2).padding(6))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(colors.bg_fill),
            border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .into()
}
