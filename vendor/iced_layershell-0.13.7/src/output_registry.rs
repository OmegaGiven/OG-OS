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
