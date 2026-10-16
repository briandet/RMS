//! A web browser as a player (`GET /api/v1/browser-player`, a WebSocket).  The web UI's
//! "Play in this browser" opens it; the browser plays what the server sends it with an
//! `<audio>` element and reports back.  Messages are JSON: first `{"type": "welcome",
//! "player": "<id>"}`, then [`ToRenderer`] from the server; [`FromRenderer`] from the
//! browser.  The player lasts as long as the connection.
//!
//! A browser that plays through Web Audio (`clock=1`) keeps time: it sends `{"type":
//! "clock", "jiffies": <ms>}` first and its play points with the time they were taken, starts
//! at the time it is told to (`resume` with `at`), and plays a little ahead or behind on
//! `hold` and `skip`, so the server keeps it in step with the players it is synced with.
//!
//! One page of a browser plays at a time (its tabs and windows share its id).  Another page
//! that connects while one plays is told `{"type": "busy"}` and closed, unless it asks to take
//! over (`takeover=1`, the user's "Play here"): then the page playing is told `{"type":
//! "replaced"}` and closed.  Neither connects again by itself until the player is free, so two
//! pages don't take it from each other in turn (each takeover restarts what plays).

use super::{mac_for, FromRenderer, Identity, ToRenderer};
use crate::server::Server;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

#[derive(Debug, Deserialize)]
pub struct Params {
    /// The browser's own id (kept in its storage), so it is the same player every time.
    pub id: String,
    /// The player's name, e.g. "Firefox on Linux".
    pub name: Option<String>,
    /// Formats the browser plays, best first (`flc,mp3,ogg,ops,aac`).
    pub formats: Option<String>,
    /// `1`: the browser plays through Web Audio and keeps a clock, so it reports exact play
    /// points and can be kept in sync (it then asks for `pcm` at `rate`).
    pub clock: Option<String>,
    /// The highest sample rate it plays (its audio output's rate).
    pub rate: Option<u32>,
    /// `1`: take the player over from another page of this browser playing it.
    pub takeover: Option<String>,
}

/// A page playing: its connection's number, and the way to tell it it has been replaced
/// (closed once that connection has ended).
type Playing = (u64, oneshot::Sender<()>);
/// The page playing for each browser, by player id.
static PLAYING: LazyLock<Mutex<HashMap<String, Playing>>> = LazyLock::new(Default::default);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Close code of a page whose player another page took (`replaced`) or has (`busy`).
const CLOSE_ELSEWHERE: u16 = 4000;

/// Formats a browser may declare.
const KNOWN: &[&str] = &["pcm", "flc", "mp3", "ogg", "ops", "aac"];

pub async fn handle(
    State(server): State<Arc<Server>>,
    Query(p): Query<Params>,
    ws: WebSocketUpgrade,
) -> Response {
    let mut formats: Vec<String> = p
        .formats
        .as_deref()
        .unwrap_or("mp3")
        .split(',')
        .map(str::trim)
        .filter(|f| KNOWN.contains(f))
        .map(str::to_string)
        .collect();
    if formats.is_empty() {
        formats.push("mp3".into());
    }
    let name = p
        .name
        .map(|n| n.trim().chars().take(64).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Web browser".into());
    let identity = Identity {
        mac: mac_for(&format!("browser:{}", p.id)),
        name,
        model: "browser".into(),
        model_name: "Web browser".into(),
        formats,
        max_sample_rate: p
            .rate
            .filter(|r| (8_000..=384_000).contains(r))
            .unwrap_or(192_000),
        accurate: p.clock.as_deref() == Some("1"),
    };
    let takeover = p.takeover.as_deref() == Some("1");
    ws.on_upgrade(move |socket| relay(server, identity, socket, takeover))
}

/// Tell a page the player is elsewhere (`busy`, `replaced`) and close it.
async fn elsewhere(socket: &mut WebSocket, kind: &str) {
    let msg = serde_json::json!({ "type": kind });
    let _ = socket.send(Message::Text(msg.to_string().into())).await;
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: CLOSE_ELSEWHERE,
            reason: kind.to_string().into(),
        })))
        .await;
}

async fn relay(server: Arc<Server>, identity: Identity, mut socket: WebSocket, takeover: bool) {
    let key = identity.mac.to_string();
    let conn = NEXT.fetch_add(1, Ordering::Relaxed);
    let (kick_tx, mut kick_rx) = oneshot::channel::<()>();
    let busy = {
        let mut playing = PLAYING.lock();
        // a page that has gone doesn't count
        let other = playing.get(&key).is_some_and(|(_, tx)| !tx.is_closed());
        if other && !takeover {
            true
        } else {
            if let Some((_, old)) = playing.insert(key.clone(), (conn, kick_tx)) {
                let _ = old.send(());
            }
            false
        }
    };
    if busy {
        debug!("{}: another page of this browser plays", identity.name);
        elsewhere(&mut socket, "busy").await;
        return;
    }
    relay_player(server, identity, socket, &mut kick_rx).await;
    let mut playing = PLAYING.lock();
    if playing.get(&key).is_some_and(|(c, _)| *c == conn) {
        playing.remove(&key);
    }
}

async fn relay_player(
    server: Arc<Server>,
    identity: Identity,
    mut socket: WebSocket,
    kick: &mut oneshot::Receiver<()>,
) {
    let (to_tx, mut to_rx) = mpsc::channel::<ToRenderer>(32);
    let (from_tx, from_rx) = mpsc::channel::<FromRenderer>(32);
    let name = identity.name.clone();
    // first, who the browser is as a player
    let welcome = serde_json::json!({ "type": "welcome", "player": identity.mac.to_string() });
    if socket
        .send(Message::Text(welcome.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    let bridge = crate::util::spawn(super::run(server, identity, to_tx, from_rx));
    loop {
        tokio::select! {
            // another page of this browser took the player
            Ok(()) = &mut *kick => {
                debug!("{name}: another page of this browser plays now");
                elsewhere(&mut socket, "replaced").await;
                break;
            }
            cmd = to_rx.recv() => {
                // the bridge ended: the server dropped the player
                let Some(cmd) = cmd else { break };
                let text = serde_json::to_string(&cmd).unwrap_or_default();
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(t) => match serde_json::from_str::<FromRenderer>(&t) {
                        Ok(m) => {
                            if from_tx.send(m).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => debug!("{name}: {e}: {t}"),
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    drop(from_tx);
    match bridge.await {
        Ok(Err(e)) => warn!("{name}: {e}"),
        Err(e) => warn!("{name}: {e}"),
        Ok(Ok(())) => {}
    }
}
