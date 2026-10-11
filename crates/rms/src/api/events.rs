//! The event stream (`GET /api/v1/events`, Server-Sent Events).
//!
//! One task watches the notification bus.  Whenever something happens (and once a second as
//! a safety net) it rebuilds the server and player states, compares them with what was last
//! published and broadcasts only what changed.  A player's position counts as changed only
//! when it jumps (seek, new track, pause), not while it advances with the clock: clients
//! advance it themselves.

use super::model::{Event, Player, ServerInfo};
use super::state;
use crate::server::Server;
use axum::extract::State;
use axum::response::sse::{self, KeepAlive, Sse};
use futures::Stream;
use indexmap::IndexMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// A serialized event, shared by every connection.
#[derive(Debug)]
pub struct Message {
    pub name: &'static str,
    pub data: String,
}

impl Message {
    fn new(e: &Event) -> Arc<Message> {
        Arc::new(Message {
            name: e.name(),
            data: serde_json::to_string(e).unwrap_or_default(),
        })
    }
}

pub struct Hub {
    tx: broadcast::Sender<Arc<Message>>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            tx: broadcast::channel(256).0,
        }
    }
}

impl Hub {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Message>> {
        self.tx.subscribe()
    }

    fn publish(&self, e: &Event) {
        // no receivers is fine
        let _ = self.tx.send(Message::new(e));
    }
}

/// Collect notifications for this long before publishing, so a burst (e.g. `playlist
/// clear` + `playlist add` + `playlist newsong`) becomes one update.
const SETTLE: Duration = Duration::from_millis(30);
const TICK: Duration = Duration::from_secs(1);
/// Library totals are a database query: refresh them at most this often.
const SERVER_MIN_INTERVAL: Duration = Duration::from_millis(500);

pub async fn run(server: Arc<Server>) {
    let mut rx = server.notifier.subscribe();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut published = Published::default();
    let mut ticks: u64 = 0;
    loop {
        let mut server_dirty = false;
        tokio::select! {
            _ = server.shutdown.cancelled() => return,
            n = rx.recv() => {
                match n {
                    Ok(n) => server_dirty |= n.player.is_none(),
                    Err(broadcast::error::RecvError::Lagged(_)) => server_dirty = true,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                // let the burst settle
                let deadline = tokio::time::Instant::now() + SETTLE;
                while let Ok(r) = tokio::time::timeout_at(deadline, rx.recv()).await {
                    match r {
                        Ok(n) => server_dirty |= n.player.is_none(),
                        Err(broadcast::error::RecvError::Lagged(_)) => server_dirty = true,
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
            _ = tick.tick() => {
                ticks += 1;
                let scanning = server.library.is_scanning();
                let shown = published.server.as_ref().map(|s| s.scan.is_some());
                server_dirty = scanning || shown != Some(scanning) || ticks.is_multiple_of(10);
            }
        }
        if server.hub_receivers() == 0 {
            // nobody listening (every new client starts with a snapshot)
            continue;
        }
        published.update(&server, server_dirty);
    }
}

#[derive(Default)]
struct Published {
    server: Option<ServerInfo>,
    server_at: Option<Instant>,
    players: IndexMap<String, (Player, Instant)>,
}

impl Published {
    fn update(&mut self, server: &Arc<Server>, server_dirty: bool) {
        let due = self
            .server_at
            .map(|t| t.elapsed() >= SERVER_MIN_INTERVAL)
            .unwrap_or(true);
        if (server_dirty && due) || self.server.is_none() {
            let info = state::server_info(server);
            if self.server.as_ref() != Some(&info) {
                server.events.publish(&Event::Server {
                    server: info.clone(),
                });
                self.server = Some(info);
            }
            self.server_at = Some(Instant::now());
        }

        let now = Instant::now();
        let current = state::players(server);
        let ids: Vec<&str> = current.iter().map(|p| p.id.as_str()).collect();
        let gone: Vec<String> = self
            .players
            .keys()
            .filter(|id| !ids.contains(&id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            self.players.shift_remove(&id);
            server.events.publish(&Event::PlayerRemoved { id });
        }
        for p in current {
            let changed = match self.players.get(&p.id) {
                Some((old, at)) => differs(old, *at, &p, now),
                None => true,
            };
            if changed {
                server.events.publish(&Event::Player { player: p.clone() });
                self.players.insert(p.id.clone(), (p, now));
            }
        }
    }
}

/// Whether `new` (taken at `now`) tells a client anything that `old` (taken at `old_at`)
/// did not, given that clients advance `elapsed` while playing.
fn differs(old: &Player, old_at: Instant, new: &Player, now: Instant) -> bool {
    let moving = |p: &Player| p.mode == super::model::PlayMode::Play && !p.buffering;
    let expected = if moving(old) {
        old.elapsed + now.duration_since(old_at).as_secs_f64()
    } else {
        old.elapsed
    };
    if (new.elapsed - expected).abs() > 1.0 {
        return true;
    }
    let sleep_jump = match (old.sleep, new.sleep) {
        (Some(a), Some(b)) => {
            let expected = a - now.duration_since(old_at).as_secs_f64();
            (b - expected).abs() > 5.0
        }
        (a, b) => a.is_some() != b.is_some(),
    };
    if sleep_jump {
        return true;
    }
    let strip = |p: &Player| Player {
        elapsed: 0.0,
        sleep: None,
        ..p.clone()
    };
    strip(old) != strip(new)
}

impl Server {
    fn hub_receivers(&self) -> usize {
        self.events.tx.receiver_count()
    }
}

pub fn snapshot(server: &Server) -> Event {
    Event::Snapshot {
        server: state::server_info(server),
        players: state::players(server),
    }
}

/// `GET /api/v1/events`
pub async fn handle(
    State(server): State<Arc<Server>>,
) -> Sse<impl Stream<Item = Result<sse::Event, Infallible>>> {
    // subscribe before taking the snapshot so nothing falls in between
    let rx = server.events.subscribe();
    let first = Message::new(&snapshot(&server));
    let stream = futures::stream::unfold(
        (server, rx, Some(first)),
        |(server, mut rx, pending)| async move {
            if let Some(m) = pending {
                return Some((Ok(to_sse(&m)), (server, rx, None)));
            }
            let m = match rx.recv().await {
                Ok(m) => m,
                // fell behind: start over from a fresh snapshot
                Err(broadcast::error::RecvError::Lagged(_)) => Message::new(&snapshot(&server)),
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            Some((Ok(to_sse(&m)), (server, rx, None)))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn to_sse(m: &Message) -> sse::Event {
    sse::Event::default().event(m.name).data(&m.data)
}

#[cfg(test)]
mod tests {
    use super::super::model::*;
    use super::*;

    fn player(mode: PlayMode, elapsed: f64) -> Player {
        Player {
            id: "00:00:00:00:00:01".into(),
            name: "Kitchen".into(),
            model: "squeezelite".into(),
            model_name: "SqueezeLite".into(),
            ip: None,
            firmware: "v2".into(),
            connected: true,
            power: true,
            mode,
            buffering: false,
            elapsed,
            duration: Some(200.0),
            can_seek: true,
            volume: 40,
            muted: false,
            volume_fixed: false,
            shuffle: Shuffle::Off,
            repeat: Repeat::Off,
            queue: QueueInfo {
                version: 1.0,
                length: 3,
                index: Some(0),
                name: None,
            },
            now_playing: None,
            synced_with: vec![],
            sleep: None,
        }
    }

    #[test]
    fn position_moving_with_the_clock_is_not_a_change() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(5);
        let old = player(PlayMode::Play, 10.0);
        assert!(!differs(&old, t0, &player(PlayMode::Play, 15.2), t1));
        // seek
        assert!(differs(&old, t0, &player(PlayMode::Play, 90.0), t1));
        // paused: the position must stay put
        let paused = player(PlayMode::Pause, 10.0);
        assert!(!differs(&paused, t0, &player(PlayMode::Pause, 10.0), t1));
        assert!(differs(&paused, t0, &player(PlayMode::Play, 10.0), t1));
    }

    #[test]
    fn other_fields_are_changes() {
        let t0 = Instant::now();
        let old = player(PlayMode::Play, 10.0);
        let mut new = player(PlayMode::Play, 10.0);
        new.volume = 41;
        assert!(differs(&old, t0, &new, t0));
    }
}
