//! Network devices as players.  Each protocol's discovery ([`super::upnp`],
//! [`super::chromecast`], [`super::airplay`]) reports what it finds to [`found`], which runs
//! one bridge per device: the bridge is the player the server sees, and the protocol's
//! *driver* plays what the bridge asks for on the device and reports what the device does.
//!
//! A driver ends when its device can't be reached any more; the player then disconnects,
//! and the device comes back as the same player (its id is stable) when it is found again.
//! Turning a protocol off in the settings ends its players at once.
//!
//! A device found in several ways (a WiiM is a Squeezebox player, a UPnP renderer and a
//! Chromecast) is one player: the best way wins (Squeezebox, then UPnP, Chromecast,
//! AirPlay), by the device's IP address.  The others are not bridged while it is
//! connected, and come back (on their next discovery) when it isn't.

use super::{FromRenderer, Identity, ToRenderer};
use crate::server::Server;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// The protocols, with the pref that turns each on and the pref listing devices to use
/// without discovery (for networks where multicast doesn't reach).
pub const PROTOCOLS: &[Protocol] = &[
    Protocol {
        prefix: "upnp",
        enabled_pref: "bridgeUpnp",
        devices_pref: "upnpRenderers",
    },
    Protocol {
        prefix: "chromecast",
        enabled_pref: "bridgeChromecast",
        devices_pref: "chromecastHosts",
    },
    Protocol {
        prefix: "airplay",
        enabled_pref: "bridgeAirplay",
        devices_pref: "airplayHosts",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct Protocol {
    /// Prefix of the keys of this protocol's devices (`upnp:<udn>`).
    pub prefix: &'static str,
    pub enabled_pref: &'static str,
    pub devices_pref: &'static str,
}

impl Protocol {
    pub fn by_prefix(prefix: &str) -> &'static Protocol {
        PROTOCOLS
            .iter()
            .find(|p| p.prefix == prefix)
            .expect("known protocol")
    }

    pub fn enabled(&self, server: &Server) -> bool {
        server.prefs.server.get_bool(self.enabled_pref)
    }

    /// Devices given by hand (description URLs, `host[:port]`).
    pub fn configured(&self, server: &Server) -> Vec<String> {
        server.prefs.server.get_string_list(self.devices_pref)
    }
}

/// A device that was found.
pub struct Device {
    /// `<protocol prefix>:<the device's own id>`, unique and stable.
    pub key: String,
    pub identity: Identity,
    /// Its address, to tell when another player is the same device.
    pub ip: Option<IpAddr>,
}

/// How a device is best played to: lower wins when one device is found several ways.
/// Squeezebox players (slimproto, not bridged) are 0.
fn rank(key: &str) -> u8 {
    match key.split(':').next() {
        Some("upnp") => 1,
        Some("chromecast") => 2,
        _ => 3,
    }
}

/// A bridged player that is running.
struct Running {
    task: tokio::task::AbortHandle,
    ip: Option<IpAddr>,
    name: String,
}

/// Another player is the same device as a bridge of `key` at `ip`: its name.
fn same_device(
    server: &Server,
    running: &HashMap<String, Running>,
    key: &str,
    ip: IpAddr,
) -> Option<String> {
    let mine = rank(key);
    // a Squeezebox player connected from that address
    let squeezebox = server
        .connected_players()
        .into_iter()
        .find(|p| !p.is_bridged() && p.state.lock().addr.is_some_and(|a| a.ip() == ip));
    if let Some(p) = squeezebox {
        return Some(p.name());
    }
    running
        .iter()
        .find(|(k, r)| *k != key && r.ip == Some(ip) && rank(k) < mine)
        .map(|(_, r)| r.name.clone())
}

/// What a driver gets: the bridge's requests, the channel for its reports, and the
/// server's address as devices on the network reach it (`http://192.168.1.10:9000`), to
/// which the requests' URLs are relative.
pub struct DriverIo {
    pub commands: mpsc::Receiver<ToRenderer>,
    pub reports: mpsc::Sender<FromRenderer>,
    pub base_url: String,
}

/// Starts a device's driver.
pub type Start = Box<dyn FnOnce(DriverIo) -> BoxFuture<'static, ()> + Send>;

static RUNNING: Mutex<Option<HashMap<String, Running>>> = Mutex::new(None);

pub fn is_running(key: &str) -> bool {
    RUNNING.lock().as_ref().is_some_and(|m| m.contains_key(key))
}

/// Run a player for `device` (unless one runs already), driven by `start`'s driver.
pub fn found(server: &Arc<Server>, device: Device, start: Start) {
    let key = device.key.clone();
    let mut running = RUNNING.lock();
    let map = running.get_or_insert_with(HashMap::new);
    if map.contains_key(&key) {
        return;
    }
    if let Some(other) = device.ip.and_then(|ip| same_device(server, map, &key, ip)) {
        debug!(
            "{} ({key}) is the same device as player \"{other}\": not added",
            device.identity.name
        );
        return;
    }
    info!(
        "found {} ({}): {}",
        device.identity.name, device.identity.model_name, key
    );
    let (to_tx, to_rx) = mpsc::channel(32);
    let (from_tx, from_rx) = mpsc::channel(32);
    let io = DriverIo {
        commands: to_rx,
        reports: from_tx,
        base_url: base_url(server),
    };
    let server2 = server.clone();
    let key2 = key.clone();
    let (ip, name) = (device.ip, device.identity.name.clone());
    let task = crate::util::spawn(async move {
        let driver = crate::util::spawn(start(io));
        let _ = super::run(server2, device.identity, to_tx, from_rx).await;
        driver.abort();
        if let Some(m) = RUNNING.lock().as_mut() {
            m.remove(&key2);
        }
    });
    map.insert(
        key,
        Running {
            task: task.abort_handle(),
            ip,
            name,
        },
    );
    drop(running);
    // it may be the better way to a device bridged already
    dedupe(server);
}

/// End bridges of devices another player now plays to better (a Squeezebox player that
/// connected, a UPnP renderer found after the Chromecast), and forget their players.
pub fn dedupe(server: &Arc<Server>) {
    let mut ended = Vec::new();
    {
        let mut running = RUNNING.lock();
        let Some(map) = running.as_mut() else {
            return;
        };
        let worse: Vec<(String, String)> = map
            .iter()
            .filter_map(|(k, r)| {
                let other = same_device(server, map, k, r.ip?)?;
                Some((k.clone(), other))
            })
            .collect();
        for (key, other) in worse {
            if let Some(r) = map.remove(&key) {
                info!(
                    "{} ({key}) is the same device as player \"{other}\": removed",
                    r.name
                );
                r.task.abort();
                ended.push(super::mac_for(&key));
            }
        }
    }
    if ended.is_empty() {
        return;
    }
    // forget the players once their connections have closed
    let server = server.clone();
    crate::util::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for mac in ended {
            if let Some(p) = server.player(&mac) {
                server.forget_player(&p);
            }
        }
    });
}

/// End the players of protocols that are turned off.
pub fn reconfigure(server: &Server) {
    let mut running = RUNNING.lock();
    let Some(map) = running.as_mut() else {
        return;
    };
    for proto in PROTOCOLS {
        if proto.enabled(server) {
            continue;
        }
        let prefix = format!("{}:", proto.prefix);
        map.retain(|key, r| {
            let keep = !key.starts_with(&prefix);
            if !keep {
                r.task.abort();
            }
            keep
        });
    }
}

/// The server's HTTP address as devices on the network reach it.
pub fn base_url(server: &Server) -> String {
    let bind = server.config.bind_addr.trim();
    let ip = match bind.parse::<std::net::IpAddr>() {
        Ok(ip) if !ip.is_unspecified() => ip.to_string(),
        _ => crate::util::server_ip(),
    };
    let host = if ip.contains(':') {
        format!("[{ip}]")
    } else {
        ip
    };
    format!("http://{}:{}", host, server.http_port())
}

/// Start each protocol's discovery.
pub fn start(server: &Arc<Server>) {
    crate::util::spawn(super::upnp::run(server.clone()));
    crate::util::spawn(super::chromecast::run(server.clone()));
    crate::util::spawn(super::airplay::run(server.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_are_known() {
        assert_eq!(Protocol::by_prefix("upnp").enabled_pref, "bridgeUpnp");
        assert_eq!(PROTOCOLS.len(), 3);
    }
}
