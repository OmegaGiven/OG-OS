//! Step 8 spike (PLAN.md section 9/13): does anything give us a
//! StatusNotifierItem host for free? Waybar wraps an existing SNI-watcher;
//! nothing under iced_layershell does. `system-tray` (used by ironbar, a
//! similar wlr-layer-shell bar) claims to be exactly that: an async
//! client for consuming other apps' tray icons. This binary is deliberately
//! NOT wired into og-bar's own bar layout yet — it's isolated on purpose so
//! a dead end here doesn't block anything else already built.
//!
//! Run standalone: `cargo run --release --bin tray-spike`

#[tokio::main]
async fn main() {
    println!("connecting to StatusNotifierWatcher...");
    let client = match system_tray::client::Client::new().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to connect: {e}");
            std::process::exit(1);
        }
    };

    let initial = client.items();
    let items = initial.lock().unwrap();
    println!("{} item(s) already registered:", items.len());
    for (id, (item, _menu)) in items.iter() {
        println!(
            "  id={id} title={:?} status={:?} icon_name={:?} has_pixmap={}",
            item.title,
            item.status,
            item.icon_name,
            item.icon_pixmap.as_ref().is_some_and(|p| !p.is_empty()),
        );
    }
    drop(items);

    println!("watching for changes for 10s (Ctrl-C to stop earlier)...");
    let mut rx = client.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            ev = rx.recv() => match ev {
                Ok(event) => println!("event: {event:?}"),
                Err(e) => { eprintln!("subscription closed: {e}"); break; }
            }
        }
    }
    println!("spike done.");
}
