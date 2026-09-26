//! One reusable anchored-popup mechanism (PLAN.md section 4). A popup is
//! just a second layer-shell surface, positioned by the compositor at the
//! point the user clicked (via `NewMenu`'s direction). Every menu the bar
//! ever needs — power, network, bluetooth, tray items, the customization
//! popout — is a consumer of this one `PopupState`, not its own surface
//! plumbing.

use iced::window;

use crate::modules::tray::TrayMenuEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupKind {
    Power,
    Settings,
    /// Right-click menu on a single app icon in the Workspaces module —
    /// close it, or move it to another workspace. `other_workspaces` is
    /// resolved once at open time (a quick synchronous `get_workspaces`,
    /// same convention as other sync shell-outs in this codebase) rather
    /// than kept live, since the menu itself is transient.
    WindowMenu {
        con_id: i64,
        other_workspaces: Vec<i32>,
    },
    /// Right-click on a tray icon's real DBusMenu, flattened at open time
    /// (see `tray::flatten_tray_menu`) — same "resolved once, not kept
    /// live" convention as `WindowMenu` above.
    Tray {
        address: String,
        menu_path: String,
        entries: Vec<TrayMenuEntry>,
    },
    /// Right-click on the Bluetooth module — power on/off, devices,
    /// adapter settings, send files. Same set waybar's old `menu-file` for
    /// this module offered.
    Bluetooth,
}

#[derive(Debug, Clone)]
pub struct PopupState {
    pub id: window::Id,
    pub kind: PopupKind,
}
