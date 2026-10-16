//! `/jsonrpc.js` (`Slim::Web::JSONRPC`).

use crate::control::request::Source;
use crate::server::Server;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, info};

/// Command array elements arrive as JSON values; LMS stringifies them (numbers via Perl).
pub fn value_to_token(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => (*b as i32).to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                let s = f.to_string();
                s.trim_end_matches(".0").to_string()
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// A request that got no answer, logged once per sender and request: a device that asks
/// for something RMS doesn't do (a controller app, a player's own app) shows up in the log.
pub fn unanswered(via: &str, peer: Option<SocketAddr>, what: &str) {
    let from = sender(peer);
    let what: String = what.chars().take(300).collect();
    once(
        format!("{via} {from} {what}"),
        format!("{via} request from {from} not answered: {what}"),
    );
}

/// The first request of a sender (to see which devices use which interface).
pub fn first_contact(via: &str, peer: Option<SocketAddr>, what: &str) {
    let from = sender(peer);
    let what: String = what.chars().take(300).collect();
    once(
        format!("{via} {from}"),
        format!("{via}: first request from {from}: {what}"),
    );
}

fn sender(peer: Option<SocketAddr>) -> String {
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Log `line` at info level the first time `key` comes up (up to a limit).
fn once(key: String, line: String) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if seen.len() < 1000 && seen.insert(key) {
        info!("{line}");
    }
}

/// Execute one JSON-RPC body; `None` means "close without response" in LMS.
pub async fn execute_body(
    server: &Arc<Server>,
    body: &[u8],
    peer: Option<SocketAddr>,
) -> Option<Value> {
    let unanswered = |why: &str| {
        let text = String::from_utf8_lossy(body);
        unanswered("JSON-RPC", peer, &format!("{why}: {text}"));
    };
    let Ok(req) = serde_json::from_slice::<Value>(body) else {
        unanswered("not JSON");
        return None;
    };
    if req.get("method").and_then(|m| m.as_str()) != Some("slim.request") {
        unanswered("not slim.request");
        return None;
    }
    let Some(params) = req.get("params").and_then(|p| p.as_array()) else {
        unanswered("no params");
        return None;
    };
    let player = params.first().map(value_to_token).unwrap_or_default();
    let Some(cmd) = params.get(1).and_then(|c| c.as_array()) else {
        unanswered("no command");
        return None;
    };
    let tokens: Vec<String> = cmd.iter().map(value_to_token).collect();
    let p = if player.is_empty() {
        None
    } else {
        server.resolve_client(&player)
    };
    let connected = p.as_ref().map(|p| p.is_connected()).unwrap_or(false);
    let raw = if player.is_empty() {
        None
    } else {
        Some(player.as_str())
    };
    let mut r = server
        .dispatch
        .parse(p.as_ref().map(|p| p.id), raw, connected, &tokens);
    r.source = Source::JsonRpc;
    let masked = crate::control::request::mask_secrets(&tokens);
    debug!(
        "jsonrpc from {} {} {:?}",
        peer.map(|p| p.ip().to_string()).unwrap_or_default(),
        player,
        masked
    );
    first_contact("JSON-RPC", peer, &format!("{player} {masked:?}"));
    let r = crate::control::dispatch::execute(server, r).await;
    if r.status != crate::control::request::Status::Done
        && r.status != crate::control::request::Status::Dispatchable
    {
        let who = if p.is_none() && !player.is_empty() {
            " (unknown player)"
        } else {
            ""
        };
        crate::web::jsonrpc::unanswered(
            "JSON-RPC",
            peer,
            &format!("{:?}{who} {player} {masked:?}", r.status),
        );
        return None;
    }
    let mut out = serde_json::Map::new();
    if let Some(id) = req.get("id") {
        out.insert("id".into(), id.clone());
    }
    out.insert("method".into(), json!("slim.request"));
    out.insert("params".into(), Value::Array(params.clone()));
    out.insert("result".into(), Value::Object(r.results));
    Some(Value::Object(out))
}

fn respond(v: Option<Value>) -> Response {
    match v {
        Some(v) => {
            let mut r = Response::new(axum::body::Body::from(
                serde_json::to_vec(&v).unwrap_or_default(),
            ));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            r
        }
        // LMS closes the connection without a response; an empty 500 is the closest HTTP analogue.
        None => (StatusCode::INTERNAL_SERVER_ERROR, "").into_response(),
    }
}

/// Who sent the request (absent in tests that call the router directly).
pub fn peer(ext: &axum::http::Extensions) -> Option<SocketAddr> {
    ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0)
}

pub async fn handle_post(
    State(server): State<Arc<Server>>,
    ext: axum::http::Extensions,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    respond(execute_body(&server, &body, peer(&ext)).await)
}

pub async fn handle_get(
    State(server): State<Arc<Server>>,
    ext: axum::http::Extensions,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match q.get("request") {
        Some(r) => respond(execute_body(&server, r.as_bytes(), peer(&ext)).await),
        None => (StatusCode::BAD_REQUEST, "").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(value_to_token(&json!(5)), "5");
        assert_eq!(value_to_token(&json!(2.5)), "2.5");
        assert_eq!(value_to_token(&json!("x")), "x");
        assert_eq!(value_to_token(&json!(null)), "");
    }
}
