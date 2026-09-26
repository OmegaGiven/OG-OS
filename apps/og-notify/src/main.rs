//! Small standalone daemon: watches the session bus for
//! `org.freedesktop.Notifications.Notify` calls (the same message mako
//! receives) and, when enabled in og-settings, flashes a rain/glow/wind/
//! sparkle effect on a transparent wlr-layer-shell overlay covering every
//! output. Independent process from og-settings itself — the GUI only
//! edits `notif_fx_*` in the shared config file and toggles this binary's
//! sway autostart line.
//!
//! The actual layer-shell/compositor/output/shm plumbing lives in the
//! shared `og-wayland` crate (`overlay` module) — this binary only
//! supplies the two things specific to it: how to load its config, and
//! how to render one frame of whichever effect is configured.

mod config;
mod effect;
mod notify_watch;

use config::EffectKind as ConfiguredKind;

fn main() {
    if std::env::args().any(|a| a == "--preview") {
        // og-settings' "Send test notification" button — ignores the
        // `enabled` toggle (so you can preview before turning it on) and
        // exits once every window's effect has actually finished (handled
        // inside `og_wayland::overlay::run_preview`), not a flat timer —
        // a streak's fall time varies, so a flat cutoff risked freezing
        // the animation mid-fall the same way it used to.
        og_wayland::overlay::run_preview(|width, height| {
            let cfg = config::load().unwrap_or(config::FxConfig {
                enabled: true,
                effect: ConfiguredKind::Rain,
                color: (255, 120, 0),
                duration_ms: 1600,
            });
            Box::new(effect::Effect::new(&cfg, width, height))
        });
    } else {
        og_wayland::overlay::run_daemon(|handle| {
            notify_watch::spawn(handle);
        });
    }
}
