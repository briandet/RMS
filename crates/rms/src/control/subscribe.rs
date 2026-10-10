//! Query subscriptions (`registerAutoExecute` / `__autoexecute`).
//!
//! A query executed with a `subscribe:<secs>` tag on a transport that supports pushing
//! results (Cometd, CLI, JSON-RPC with `X-Jive`) is re-executed whenever a relevant
//! notification arrives (after the filter's delay, coalescing bursts) and at least every
//! `<secs>` seconds when `<secs>` > 0.  `subscribe:-` removes the subscription.

use super::request::Request;
use crate::notify::Notification;
use crate::server::Server;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

/// Receives every re-execution result.  Returning `false` drops the subscription.
pub type Sink = Arc<dyn Fn(Request) -> bool + Send + Sync>;

struct Sub {
    poke: mpsc::UnboundedSender<f64>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub struct Subscriptions {
    /// key: `<connection>|<request name>|<client or global>`
    subs: DashMap<String, Sub>,
}

fn key(cnx: &str, req: &Request) -> String {
    format!("{}|{}|{}", cnx, req.request_str(), req.client.map(|c| c.to_string()).unwrap_or_else(|| "global".into()))
}

impl Subscriptions {
    /// Register (or replace / remove) the subscription for an executed query.
    pub fn register(&self, server: &Arc<Server>, cnx: &str, req: &Request, sink: Sink) {
        let Some(val) = req.param("subscribe").map(str::to_string) else { return };
        let k = key(cnx, req);
        if let Some((_, old)) = self.subs.remove(&k) {
            old.task.abort();
        }
        if val == "-" {
            debug!("unsubscribed {}", k);
            return;
        }
        let timeout: f64 = val.parse().unwrap_or(0.0);
        let (tx, mut rx) = mpsc::unbounded_channel::<f64>();
        let mut virgin = req.clone();
        virgin.results.clear();
        let server2 = server.clone();
        let k2 = k.clone();
        let task = tokio::spawn(async move {
            let mut deadline: Option<tokio::time::Instant> = None;
            let periodic = if timeout > 0.0 { Some(Duration::from_secs_f64(timeout)) } else { None };
            let mut next_periodic = periodic.map(|p| tokio::time::Instant::now() + p);
            loop {
                let wake = match (deadline, next_periodic) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                tokio::select! {
                    msg = rx.recv() => match msg {
                        Some(delay) => {
                            deadline = Some(tokio::time::Instant::now() + Duration::from_secs_f64(delay.max(0.0)));
                            continue;
                        }
                        None => return,
                    },
                    _ = async {
                        match wake {
                            Some(w) => tokio::time::sleep_until(w).await,
                            None => futures::future::pending::<()>().await,
                        }
                    } => {}
                }
                deadline = None;
                next_periodic = periodic.map(|p| tokio::time::Instant::now() + p);
                let mut r = virgin.clone();
                r.status = super::request::Status::Dispatchable;
                let r = super::dispatch::execute(&server2, r).await;
                if r.status.is_error() || !sink(r) {
                    debug!("subscription {} ended", k2);
                    server2.subscriptions.subs.remove(&k2);
                    return;
                }
            }
        });
        debug!("subscribed {} (timeout {})", k, timeout);
        self.subs.insert(k, Sub { poke: tx, task });
    }

    /// Cometd: subscriptions are additionally keyed by their response channel so that
    /// `/slim/unsubscribe` can remove exactly one.
    pub fn register_with_channel(&self, server: &Arc<Server>, cnx: &str, channel: &str, req: &Request, sink: Sink) {
        self.register(server, &format!("{}#{}", cnx, channel), req, sink);
    }

    pub fn unregister_channel(&self, cnx: &str, channel: &str) {
        self.remove_prefix(&format!("{}#{}|", cnx, channel));
    }

    /// Remove every subscription belonging to a connection.
    pub fn unregister_connection(&self, cnx: &str) {
        self.remove_prefix(&format!("{}|", cnx));
        self.remove_prefix(&format!("{}#", cnx));
    }

    fn remove_prefix(&self, prefix: &str) {
        let keys: Vec<String> = self.subs.iter().filter(|e| e.key().starts_with(prefix)).map(|e| e.key().clone()).collect();
        for k in keys {
            if let Some((_, s)) = self.subs.remove(&k) {
                s.task.abort();
            }
        }
    }

    /// Feed a notification to all subscriptions (called by the notification pump).
    pub fn notify(&self, server: &Server, n: &Notification) {
        for e in self.subs.iter() {
            let mut parts = e.key().rsplitn(3, '|');
            let client = parts.next().unwrap_or("global");
            let name = parts.next().unwrap_or("");
            if let Some(delay) = filter(server, name, client, n) {
                let _ = e.value().poke.send(delay - 1.0);
            }
        }
    }
}

/// Per-query notification filters; returns the LMS "relevance" (delay + 1 s) or None.
fn filter(server: &Server, name: &str, client: &str, n: &Notification) -> Option<f64> {
    let first = n.first();
    let second = n.second();
    match name {
        "status" => {
            let ours = n.player.map(|p| p.to_string());
            if ours.as_deref() != Some(client) {
                // other players: only sync changes and metadata within our group matter
                let in_group = n
                    .player
                    .and_then(|p| server.player(&p))
                    .zip(slimproto::MacAddress::parse(client).and_then(|c| server.player(&c)))
                    .map(|(a, b)| Arc::ptr_eq(&a.controller(), &b.controller()))
                    .unwrap_or(false);
                if in_group && (first == "sync" || (first == "playlist" && second == "newmetadata") || first == "newmetadata") {
                    return Some(1.3);
                }
                if first == "playlist" && second == "sync" && in_group {
                    return Some(1.3);
                }
                return None;
            }
            if first == "prefset" {
                let pref = n.word(2);
                return matches!(pref, "alarmSnoozeSeconds" | "digitalVolumeControl" | "libraryId").then_some(1.3);
            }
            if matches!(first, "ir" | "button" | "debug" | "pref" | "display" | "displaynotify") {
                return None;
            }
            if first == "client" && second == "forget" {
                return Some(1.0);
            }
            if first == "mixer" && second == "muting" {
                return Some(1.4);
            }
            if first == "playlist" && second == "stop" {
                return Some(2.0);
            }
            if first == "playlist" && (second == "open" || second == "jump" || second == "index") {
                return Some(2.5);
            }
            Some(1.3)
        }
        "serverstatus" => match first {
            "client" if matches!(second, "forget" | "new" | "disconnect" | "reconnect") => Some(1.0),
            "connect" => Some(1.0),
            "wipecache" | "rescan" | "power" | "name" | "library" => Some(1.3),
            "prefset" | "pref" | "playerpref" | "sync" => Some(1.3),
            "playlist" if second == "sync" => Some(1.3),
            _ => None,
        },
        "displaystatus" => {
            if first == "displaynotify" && n.player.map(|p| p.to_string()).as_deref() == Some(client) {
                Some(1.0)
            } else {
                None
            }
        }
        "date" => None,
        _ => None,
    }
}

/// Background task: forwards every notification to the subscription registry.
pub async fn pump(server: Arc<Server>) {
    let mut rx = server.notifier.subscribe();
    loop {
        match rx.recv().await {
            Ok(n) => server.subscriptions.notify(&server, &n),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(k)) => tracing::warn!("subscription pump lagged by {}", k),
            Err(_) => return,
        }
    }
}
