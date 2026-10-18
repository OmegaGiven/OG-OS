//! Live checks against the real system bus — read-only apart from a short
//! discovery and a non-default agent that is unregistered again. Ignored by
//! default; run with `cargo test --release -- --ignored live_` on a
//! machine with BlueZ.

use super::*;

/// One runtime for all live tests: the shared `system_bus()` connection
/// lives on the runtime that created it.
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    })
}

#[test]
#[ignore]
fn live_load_state() {
    rt().block_on(async {
        let conn = system_bus().await.expect("system bus");
        let state = load_state(conn).await.expect("GetManagedObjects");
        let a = state.adapter().expect("adapter");
        println!("adapter {} {:?} powered={} pairable={}", a.path, a.alias, a.powered, a.pairable);
        let l = state.grouped();
        for d in l.mine.iter().chain(&l.available).chain(&l.unnamed) {
            println!(
                "  {} {:?} icon={:?} paired={} bonded={} trusted={} connected={} half={} rssi={:?} batt={:?}",
                d.address,
                d.display_name(),
                d.icon,
                d.paired,
                d.bonded,
                d.trusted,
                d.connected,
                d.half_paired(),
                d.rssi,
                d.battery
            );
        }
    });
}

#[test]
#[ignore]
fn live_event_stream_with_short_discovery() {
    std::env::set_var("OG_SETTINGS_BT_NO_AGENT", "1");
    rt().block_on(async {
        let mut events = Box::pin(events());
        let adapter = loop {
            match events.next().await.expect("event") {
                BtMsg::State(s) => break s.adapter().expect("adapter").path.clone(),
                BtMsg::AgentRegistered(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        };
        start_discovery(adapter.clone())
            .await
            .expect("StartDiscovery");
        let mut saw_discovering = false;
        let mut updates = 0;
        let mut seen = std::collections::BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, events.next()).await {
            if let BtMsg::State(s) = msg {
                updates += 1;
                saw_discovering |= s.adapter().is_some_and(|a| a.discovering);
                for d in s.devices.values().filter(|d| d.rssi.is_some()) {
                    seen.insert(format!("{} {:?} {:?}", d.address, d.display_name(), d.rssi));
                }
            }
        }
        stop_discovery(adapter).await.expect("StopDiscovery");
        println!("state updates during scan: {updates}");
        for s in &seen {
            println!("  in range: {s}");
        }
        assert!(
            saw_discovering,
            "Discovering=true should arrive via PropertiesChanged"
        );
    });
}

#[test]
#[ignore]
fn live_agent_register_unregister_non_default() {
    rt().block_on(async {
        let conn = system_bus().await.expect("system bus");
        let (broker, _rx) = Broker::new();
        let path = "/org/og_os/settings/test_agent";
        conn.object_server()
            .at(
                path,
                Agent {
                    broker,
                    conn: conn.clone(),
                },
            )
            .await
            .unwrap();
        let p = ObjectPath::try_from(path).unwrap();
        call(
            conn,
            "/org/bluez",
            "org.bluez.AgentManager1",
            "RegisterAgent",
            &(&p, AGENT_CAPABILITY),
        )
        .await
        .expect("RegisterAgent");
        call(
            conn,
            "/org/bluez",
            "org.bluez.AgentManager1",
            "UnregisterAgent",
            &(&p,),
        )
        .await
        .expect("UnregisterAgent");
        conn.object_server().remove::<Agent, _>(path).await.unwrap();
    });
}
