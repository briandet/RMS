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

use super::{mac_for, FromRenderer, Identity, ToRenderer};
use crate::server::Server;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::mpsc;
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
}

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
    ws.on_upgrade(move |socket| relay(server, identity, socket))
}

async fn relay(server: Arc<Server>, identity: Identity, mut socket: WebSocket) {
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
