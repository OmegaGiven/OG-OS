//! Triggers the overlay every time anything on the session bus calls
//! `org.freedesktop.Notifications.Notify` — i.e. every time a
//! notification is about to be shown, regardless of which daemon (mako)
//! actually owns that name.
//!
//! Plain `AddMatch` can't see this: `Notify` is a method call addressed
//! directly at mako's unique name, not a broadcast signal, so normal
//! matching never routes a copy to a third party. The bus's own
//! `BecomeMonitor` mechanism (the same one `busctl monitor`/`dbus-monitor`
//! use) is what actually lets an unrelated client eavesdrop on it.

use futures_util::StreamExt;
use og_wayland::overlay::OverlayHandle;
use zbus::MessageStream;

/// A `Notify` call arrived on the bus — (re)loads the config fresh (so a
/// setting change takes effect without restarting this daemon) and, if
/// enabled, triggers the overlay on every output.
fn trigger(handle: &OverlayHandle) {
    let Some(cfg) = crate::config::load() else { return };
    if !cfg.enabled {
        return;
    }
    handle.trigger(move |width, height| Box::new(crate::effect::Effect::new(&cfg, width, height)));
}

pub fn spawn(handle: OverlayHandle) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(_) => return,
        };
        rt.block_on(async move {
            let Ok(conn) = zbus::Connection::session().await else { return };
            let match_rule = "interface='org.freedesktop.Notifications',member='Notify'";
            let become_monitor = conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus.Monitoring"),
                    "BecomeMonitor",
                    &(vec![match_rule], 0u32),
                )
                .await;
            if become_monitor.is_err() {
                return;
            }

            let mut stream = MessageStream::from(&conn);
            while let Some(Ok(msg)) = stream.next().await {
                let header = msg.header();
                if header.member().map(|m| m.as_str()) == Some("Notify") {
                    trigger(&handle);
                }
            }
        });
    });
}
