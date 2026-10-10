//! `/jsonrpc.js` (`Slim::Web::JSONRPC`).

use crate::control::request::Source;
use crate::server::Server;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

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

/// Execute one JSON-RPC body; `None` means "close without response" in LMS.
pub async fn execute_body(server: &Arc<Server>, body: &[u8]) -> Option<Value> {
    let req: Value = serde_json::from_slice(body).ok()?;
    if req.get("method").and_then(|m| m.as_str()) != Some("slim.request") {
        return None;
    }
    let params = req.get("params")?.as_array()?;
    let player = params.first().map(value_to_token).unwrap_or_default();
    let cmd = params.get(1)?.as_array()?;
    let tokens: Vec<String> = cmd.iter().map(value_to_token).collect();
    let p = if player.is_empty() { None } else { server.resolve_client(&player) };
    let connected = p.as_ref().map(|p| p.is_connected()).unwrap_or(false);
    let raw = if player.is_empty() { None } else { Some(player.as_str()) };
    let mut r = server.dispatch.parse(p.as_ref().map(|p| p.id), raw, connected, &tokens);
    r.source = Source::JsonRpc;
    debug!("jsonrpc {} {:?}", player, tokens);
    let r = crate::control::dispatch::execute(server, r).await;
    if r.status != crate::control::request::Status::Done && r.status != crate::control::request::Status::Dispatchable {
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
            let mut r = Response::new(axum::body::Body::from(serde_json::to_vec(&v).unwrap_or_default()));
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            r
        }
        // LMS closes the connection without a response; an empty 500 is the closest HTTP analogue.
        None => (StatusCode::INTERNAL_SERVER_ERROR, "").into_response(),
    }
}

pub async fn handle_post(State(server): State<Arc<Server>>, _headers: HeaderMap, body: Bytes) -> Response {
    respond(execute_body(&server, &body).await)
}

pub async fn handle_get(State(server): State<Arc<Server>>, Query(q): Query<HashMap<String, String>>) -> Response {
    match q.get("request") {
        Some(r) => respond(execute_body(&server, r.as_bytes()).await),
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
