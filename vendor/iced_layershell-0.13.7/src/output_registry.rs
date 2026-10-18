//! `layershellev`'s `LayerEvent::XdgInfoChanged` (the wl_output name
//! arriving via zxdg_output) never reached the app in stock
//! iced_layershell — `multi_window::run`'s event closure had no arm for
//! it, and AllScreens apps had no way to tell which of their surfaces was
//! on which compositor output.
//!
//! The name arrives keyed by `layershellev::id::Id` (the wayland-side
//! surface id), in the outer event closure — before iced_layershell has
//! even minted an `iced::window::Id` for that surface (that only happens
//! later, in `run_instance`, on the surface's first `RequestRefresh`). So
//! this keeps the name and the raw-id/iced-id alias in two independent
//! maps, joined at query time — order between the two writers doesn't
//! matter.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use iced_core::window::Id as IcedId;
use layershellev::id::Id as RawId;

struct State {
    names: HashMap<RawId, String>,
    aliases: HashMap<IcedId, RawId>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            names: HashMap::new(),
            aliases: HashMap::new(),
        })
    })
}

pub(crate) fn set_name(raw: RawId, name: String) {
    state().lock().unwrap().names.insert(raw, name);
}

pub(crate) fn set_alias(iced_id: IcedId, raw: RawId) {
    state().lock().unwrap().aliases.insert(iced_id, raw);
}

/// Reverse alias lookup: the iced window id minted for a wayland-side
/// surface id, if that surface ever got one.
pub(crate) fn iced_id_for(raw: RawId) -> Option<IcedId> {
    state()
        .lock()
        .unwrap()
        .aliases
        .iter()
        .find(|(_, r)| **r == raw)
        .map(|(id, _)| *id)
}

pub(crate) fn remove(iced_id: IcedId) {
    let mut state = state().lock().unwrap();
    // Drop the surface's name too — otherwise every surface ever created
    // (one per output per DPMS wake/hotplug) leaves its entry behind.
    if let Some(raw) = state.aliases.remove(&iced_id) {
        state.names.remove(&raw);
    }
    drop(state);
    frozen().lock().unwrap().remove(&iced_id);
    drawn().lock().unwrap().remove(&iced_id);
}

/// The compositor's output name (e.g. "DP-3") a surface is on, if known.
pub fn output_name(iced_id: IcedId) -> Option<String> {
    let state = state().lock().unwrap();
    let raw = state.aliases.get(&iced_id)?;
    state.names.get(raw).cloned()
}

/// Every surface iced_layershell has minted an id for so far, paired with
/// its output name if known yet — lets an AllScreens app react per-output
/// (e.g. re-layer a specific surface) without tracking its own window list.
pub fn known_ids() -> Vec<(IcedId, Option<String>)> {
    let state = state().lock().unwrap();
    state
        .aliases
        .iter()
        .map(|(id, raw)| (*id, state.names.get(raw).cloned()))
        .collect()
}

/// Surfaces whose app-driven redraws are paused (og-wallpaper freezes the
/// circuit theme on outputs whose visible workspace is covered by windows).
/// Only redraws triggered by app messages/input/NextFrame are skipped; the
/// compositor's own configure/refresh still redraws, so a frozen surface
/// never comes up blank after a resize or DPMS wake.
fn frozen() -> &'static Mutex<std::collections::HashSet<IcedId>> {
    static FROZEN: OnceLock<Mutex<std::collections::HashSet<IcedId>>> = OnceLock::new();
    FROZEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub fn set_frozen(iced_id: IcedId, is_frozen: bool) {
    let mut set = frozen().lock().unwrap();
    if is_frozen {
        set.insert(iced_id);
    } else {
        set.remove(&iced_id);
    }
}

/// Surfaces that have presented at least one frame. A surface is never
/// treated as frozen before that: a new surface's first refresh doesn't
/// present (it waits for a follow-up redraw), so freezing it early would
/// leave the output with no buffer at all.
fn drawn() -> &'static Mutex<std::collections::HashSet<IcedId>> {
    static DRAWN: OnceLock<Mutex<std::collections::HashSet<IcedId>>> = OnceLock::new();
    DRAWN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

pub(crate) fn mark_drawn(iced_id: IcedId) {
    drawn().lock().unwrap().insert(iced_id);
}

pub(crate) fn is_frozen(iced_id: IcedId) -> bool {
    frozen().lock().unwrap().contains(&iced_id) && drawn().lock().unwrap().contains(&iced_id)
}
