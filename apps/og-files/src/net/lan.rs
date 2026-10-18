//! LAN services via Avahi (mDNS / DNS-SD) over the system bus. Read-only:
//! browsers are created on a private bus connection, so Avahi frees them
//! as soon as the stream is dropped.

use super::{Discovered, LanEvent, Proto, Source};
use iced::futures::channel::mpsc as fmpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

const AVAHI: &str = "org.freedesktop.Avahi";
const IF_SERVER: &str = "org.freedesktop.Avahi.Server";
const IF_SERVER2: &str = "org.freedesktop.Avahi.Server2";
const IF_BROWSER: &str = "org.freedesktop.Avahi.ServiceBrowser";
const IF_UNSPEC: i32 = -1;
const PROTO_UNSPEC: i32 = -1;
const PROTO_INET: i32 = 0;
/// AVAHI_LOOKUP_RESULT_LOCAL: the service runs on this machine
const RESULT_LOCAL: u32 = 8;

pub(crate) const SERVICE_TYPES: [&str; 6] = [
    "_sftp-ssh._tcp",
    "_ssh._tcp",
    "_smb._tcp",
    "_webdav._tcp",
    "_webdavs._tcp",
    "_ftp._tcp",
];

pub(crate) fn proto_of(service_type: &str) -> Option<Proto> {
    Some(match service_type {
        "_sftp-ssh._tcp" | "_ssh._tcp" => Proto::Sftp,
        "_smb._tcp" => Proto::Smb,
        "_webdav._tcp" | "_webdavs._tcp" => Proto::WebDav,
        "_ftp._tcp" => Proto::Ftp,
        _ => return None,
    })
}

/// Port worth recording (None = the protocol's usual one). `_webdavs` on
/// 443 is kept explicitly: that's what selects https when mounting.
pub(crate) fn port_for(service_type: &str, port: u16) -> Option<u16> {
    let default = match service_type {
        "_webdav._tcp" => 80,
        "_webdavs._tcp" => 0,
        t => proto_of(t).map_or(0, Proto::default_port),
    };
    (port != default && port != 0).then_some(port)
}

/// One browse result: (interface, protocol, name, type, domain).
type Instance = (i32, i32, String, String, String);
/// (service name, index into `Proto::ALL`) — Proto isn't `Hash`.
type Key = (String, usize);

fn pidx(p: Proto) -> usize {
    Proto::ALL.iter().position(|x| *x == p).unwrap_or(0)
}

/// Item signals and resolver answers, merged into one loop.
enum Ev {
    New(Instance, u32),
    Remove(Instance),
    Resolved(Key, Option<Discovered>),
}

pub(crate) fn browse() -> impl Stream<Item = LanEvent> + Send + 'static {
    iced::stream::channel(32, |mut out| async move {
        // private connection: browsers die with it
        let Ok(conn) = Connection::system().await else {
            return;
        };
        let Ok(signals) = subscribe(&conn).await else {
            return;
        };
        let mut browsers: HashSet<OwnedObjectPath> = HashSet::new();
        for t in SERVICE_TYPES {
            if let Ok(p) = new_browser(&conn, t).await {
                browsers.insert(p);
            }
        }
        if browsers.is_empty() {
            return;
        }
        let use_names = mdns_names_resolve();
        let user = super::local_user();

        let (tx, rx) = fmpsc::unbounded::<Ev>();
        let sigs = signals.filter_map(|m| {
            let ev = m.ok().and_then(|m| {
                let h = m.header();
                let path = h.path()?;
                if !browsers.iter().any(|b| b.as_str() == path.as_str()) {
                    return None;
                }
                match h.member()?.as_str() {
                    "ItemNew" => {
                        let (i, p, n, t, d, f): (i32, i32, String, String, String, u32) =
                            m.body().deserialize().ok()?;
                        Some(Ev::New((i, p, n, t, d), f))
                    }
                    "ItemRemove" => {
                        let (i, p, n, t, d, _f): (i32, i32, String, String, String, u32) =
                            m.body().deserialize().ok()?;
                        Some(Ev::Remove((i, p, n, t, d)))
                    }
                    _ => None,
                }
            });
            std::future::ready(ev)
        });
        let mut events = iced::futures::stream::select(sigs, rx);

        let mut instances: HashMap<Key, HashSet<Instance>> = HashMap::new();
        let mut pending: HashSet<Key> = HashSet::new();
        let mut found: HashSet<Key> = HashSet::new();

        while let Some(ev) = events.next().await {
            match ev {
                Ev::New(inst, flags) => {
                    if flags & RESULT_LOCAL != 0 {
                        continue;
                    }
                    let Some(proto) = proto_of(&inst.3) else {
                        continue;
                    };
                    let key = (inst.2.clone(), pidx(proto));
                    instances
                        .entry(key.clone())
                        .or_default()
                        .insert(inst.clone());
                    if found.contains(&key) || !pending.insert(key.clone()) {
                        continue;
                    }
                    let (conn, mut tx, user) = (conn.clone(), tx.clone(), user.clone());
                    tokio::spawn(async move {
                        let d = resolve(&conn, &inst, use_names, &user).await;
                        let _ = tx.send(Ev::Resolved(key, d)).await;
                    });
                }
                Ev::Remove(inst) => {
                    let Some(proto) = proto_of(&inst.3) else {
                        continue;
                    };
                    let key = (inst.2.clone(), pidx(proto));
                    let gone = instances.get_mut(&key).is_some_and(|set| {
                        set.remove(&inst);
                        set.is_empty()
                    });
                    if gone {
                        instances.remove(&key);
                        if found.remove(&key) {
                            let lost = LanEvent::Lost {
                                name: key.0,
                                proto: Proto::ALL[key.1],
                            };
                            if out.send(lost).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                Ev::Resolved(key, d) => {
                    pending.remove(&key);
                    let Some(d) = d else { continue };
                    if instances.contains_key(&key)
                        && found.insert(key)
                        && out.send(LanEvent::Found(d)).await.is_err()
                    {
                        return;
                    }
                }
            }
        }
    })
}

async fn subscribe(conn: &Connection) -> Result<zbus::MessageStream, zbus::Error> {
    use zbus::message::Type;
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .sender(AVAHI)?
        .interface(IF_BROWSER)?
        .build();
    zbus::MessageStream::for_match_rule(rule, conn, Some(512)).await
}

/// Avahi ≥ 0.8: ServiceBrowserPrepare + Start (no signal race); older:
/// ServiceBrowserNew (we subscribed before creating, so nothing is lost).
async fn new_browser(
    conn: &Connection,
    service_type: &str,
) -> Result<OwnedObjectPath, zbus::Error> {
    let args = (IF_UNSPEC, PROTO_UNSPEC, service_type, "", 0u32);
    match conn
        .call_method(
            Some(AVAHI),
            "/",
            Some(IF_SERVER2),
            "ServiceBrowserPrepare",
            &args,
        )
        .await
    {
        Ok(m) => {
            let path: OwnedObjectPath = m.body().deserialize()?;
            conn.call_method(Some(AVAHI), path.as_str(), Some(IF_BROWSER), "Start", &())
                .await?;
            Ok(path)
        }
        Err(_) => {
            let m = conn
                .call_method(
                    Some(AVAHI),
                    "/",
                    Some(IF_SERVER),
                    "ServiceBrowserNew",
                    &args,
                )
                .await?;
            Ok(m.body().deserialize()?)
        }
    }
}

type Resolved = (
    i32,
    i32,
    String,
    String,
    String,
    String,
    i32,
    String,
    u16,
    Vec<Vec<u8>>,
    u32,
);

async fn resolve(
    conn: &Connection,
    inst: &Instance,
    use_names: bool,
    user: &str,
) -> Option<Discovered> {
    let (iface, proto, name, ty, domain) = inst;
    let mut answer = None;
    for aproto in [PROTO_INET, PROTO_UNSPEC] {
        let args = (
            *iface,
            *proto,
            name.as_str(),
            ty.as_str(),
            domain.as_str(),
            aproto,
            0u32,
        );
        let call = conn.call_method(Some(AVAHI), "/", Some(IF_SERVER), "ResolveService", &args);
        if let Ok(Ok(m)) = tokio::time::timeout(Duration::from_secs(5), call).await {
            if let Ok(r) = m.body().deserialize::<Resolved>() {
                answer = Some(r);
                break;
            }
        }
    }
    let (.., host, _aproto, address, port, _txt, _flags) = answer?;
    to_discovered(name, ty, &host, &address, port, use_names, user)
}

pub(crate) fn to_discovered(
    name: &str,
    ty: &str,
    host: &str,
    address: &str,
    port: u16,
    use_names: bool,
    user: &str,
) -> Option<Discovered> {
    let proto = proto_of(ty)?;
    let host = if use_names && !host.is_empty() {
        host.trim_end_matches('.').to_string()
    } else if !address.is_empty() && !address.starts_with("fe80:") {
        address.to_string()
    } else if !host.is_empty() {
        host.trim_end_matches('.').to_string()
    } else {
        return None;
    };
    Some(Discovered {
        name: name.to_string(),
        proto,
        host,
        port: port_for(ty, port),
        user: if proto == Proto::Sftp {
            user.to_string()
        } else {
            String::new()
        },
        source: Source::Lan,
    })
}

/// Does the system resolve `*.local` names (nss-mdns)? Else use addresses.
fn mdns_names_resolve() -> bool {
    std::fs::read_to_string("/etc/nsswitch.conf")
        .map(|s| {
            s.lines()
                .filter(|l| l.trim_start().starts_with("hosts:"))
                .any(|l| l.contains("mdns"))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_lan_mapping() {
        assert_eq!(proto_of("_ssh._tcp"), Some(Proto::Sftp));
        assert_eq!(proto_of("_sftp-ssh._tcp"), Some(Proto::Sftp));
        assert_eq!(proto_of("_webdavs._tcp"), Some(Proto::WebDav));
        assert_eq!(proto_of("_http._tcp"), None);
        assert_eq!(port_for("_ssh._tcp", 22), None);
        assert_eq!(port_for("_ssh._tcp", 2222), Some(2222));
        assert_eq!(port_for("_smb._tcp", 445), None);
        assert_eq!(port_for("_webdav._tcp", 80), None);
        assert_eq!(port_for("_webdav._tcp", 443), Some(443));
        assert_eq!(port_for("_webdavs._tcp", 443), Some(443));

        let d = to_discovered(
            "My NAS",
            "_smb._tcp",
            "nas.local.",
            "192.168.1.5",
            445,
            false,
            "me",
        )
        .unwrap();
        assert_eq!(
            (d.host.as_str(), d.port, d.user.as_str()),
            ("192.168.1.5", None, "")
        );
        assert_eq!(d.source, Source::Lan);
        let d = to_discovered(
            "box",
            "_ssh._tcp",
            "box.local.",
            "192.168.1.6",
            22,
            true,
            "me",
        )
        .unwrap();
        assert_eq!((d.host.as_str(), d.user.as_str()), ("box.local", "me"));
        let d = to_discovered("v6", "_ssh._tcp", "v6.local", "fe80::1", 22, false, "me").unwrap();
        assert_eq!(d.host, "v6.local");
        assert!(to_discovered("x", "_ssh._tcp", "", "", 22, false, "me").is_none());
    }

    /// Live, read-only: browse the LAN for 4 s, print service types found.
    #[tokio::test]
    #[ignore]
    async fn net_lan_live() {
        let mut s = Box::pin(browse());
        let mut seen: HashMap<String, usize> = HashMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, s.next()).await {
            if let LanEvent::Found(d) = ev {
                *seen.entry(format!("{:?}", d.proto)).or_default() += 1;
            }
        }
        println!("LIVE lan: {seen:?}");
    }
}
