mod bar;
mod icon_font;
mod message;
mod module;
mod modules;
mod popup;
mod power;
mod settings;
mod watchdog;
mod window_menu;

use iced_layershell::build_pattern::daemon;
use iced_layershell::reexport::{Anchor, Layer};
use iced_layershell::settings::{LayerShellSettings, StartMode};

use bar::Bar;
use og_config::{BarConfig, BarOutputMode, Edge};

fn main() -> iced_layershell::Result {
    watchdog::ready();
    let bar_cfg = BarConfig::load();
    let thickness = bar_cfg.thickness;

    let start_mode = match &bar_cfg.output_mode {
        BarOutputMode::AllScreens => StartMode::AllScreens,
        BarOutputMode::SingleOutput(name) => StartMode::TargetScreen(name.clone()),
    };

    // Auto-hide starts collapsed to the reveal strip (bar::REVEAL_STRIP) —
    // exclusive_zone follows it down too, so windows only lose that thin
    // strip of screen space rather than the bar's full thickness the
    // whole time it's sitting collapsed. bar.rs's SizeChange handles
    // resizing the surface itself on hover; exclusive_zone can't be
    // changed at runtime (no such action exists in iced_layershell), so
    // it's fixed at this startup value for the process's whole lifetime.
    let cross = if bar_cfg.auto_hide { bar::REVEAL_STRIP } else { thickness };
    let (size, anchor) = match bar_cfg.position {
        Edge::Top => (Some((0, cross)), Anchor::Top | Anchor::Left | Anchor::Right),
        Edge::Bottom => (Some((0, cross)), Anchor::Bottom | Anchor::Left | Anchor::Right),
        Edge::Left => (Some((cross, 0)), Anchor::Left | Anchor::Top | Anchor::Bottom),
        Edge::Right => (Some((cross, 0)), Anchor::Right | Anchor::Top | Anchor::Bottom),
    };

    let app = daemon("og-bar", bar::update, bar::view, bar::remove_id);
    let app = match icon_font::load_font_bytes() {
        Some(bytes) => app.font(bytes),
        None => app,
    };

    app.layer_settings(LayerShellSettings {
            size,
            anchor,
            exclusive_zone: cross as i32,
            // AllScreens: one surface per connected output (waybar's own
            // default behavior); layershellev handles this itself, no
            // manual output-tracking needed on our side (see PLAN.md
            // section 10 / step 9 spike). SingleOutput pins to one output
            // by name — user-configurable via og-settings' Bar tab.
            start_mode,
            // Top-layer surfaces stop receiving pointer input on other
            // outputs once any output has a fullscreen surface — a known
            // wlroots/sway quirk (same reason swaylock/waybar use overlay).
            layer: Layer::Overlay,
            ..Default::default()
        })
        .style(bar::style)
        .subscription(bar::subscription)
        .run_with(Bar::new)
}
