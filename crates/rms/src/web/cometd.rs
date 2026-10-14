//! Bayeux / Cometd endpoint (`Slim::Web::Cometd`, `Slim::Web::Cometd::Manager`).
//!
//! Supports both connection types LMS offers: `streaming` (one never-ending chunked HTTP
//! response, every delivery is one chunk holding a JSON array) and `long-polling`.

use crate::control::request::{Request, Source};
use crate::server::Server;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::Response;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, info};

const RETRY_DELAY: u64 = 5000;
const LONG_POLLING_TIMEOUT: u64 = 60000;
const AUTOKILL_SECS: f64 = 180.0;
const STREAM_LOSS_GRACE_SECS: f64 = 10.0;

#[derive(Default)]
struct Client {
    pending: Vec<Value>,
    /// Active streaming connection.
    stream: Option<mpsc::UnboundedSender<Vec<Value>>>,
    /// Waiter for an outstanding long-poll.
    poll: Option<Arc<Notify>>,
    /// Bayeux channel patterns this client subscribed to.
    channels: Vec<String>,
    /// Plain notification subscriptions: response channel -> (verbs filter, player id, id)
    plain: HashMap<String, (Vec<String>, Option<String>, Value)>,
    /// When the client stopped having an active connection.
    idle_since: Option<f64>,
}

#[derive(Default)]
pub struct Manager {
    clients: Mutex<HashMap<String, Client>>,
}

fn create_uuid() -> String {
    format!("{:08x}", rand::random::<u32>())
}

fn http_date() -> String {
    chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

fn clid_from_channel(ch: &str) -> Option<String> {
    let re = regex::Regex::new(r"/([0-9a-f]{8})/").unwrap();
    re.captures(ch).map(|c| c[1].to_string())
}

/// Bayeux glob matching as `Manager::add_channels` compiles it (unanchored regexes).
fn channel_matches(pattern: &str, channel: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return channel.starts_with(&format!("{}/", prefix));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return channel
            .strip_prefix(&format!("{}/", prefix))
            .map(|rest| !rest.is_empty())
            .unwrap_or(false);
    }
    channel.contains(pattern)
}

impl Manager {
    fn is_valid(&self, clid: &str) -> bool {
        self.clients.lock().contains_key(clid)
    }

    fn add_client(&self, clid: &str) {
        self.clients.lock().insert(
            clid.to_string(),
            Client {
                idle_since: Some(crate::util::now()),
                ..Default::default()
            },
        );
    }

    fn remove_client(&self, server: &Server, clid: &str) {
        self.clients.lock().remove(clid);
        server
            .subscriptions
            .unregister_connection(&format!("cometd:{}", clid));
        info!("cometd client {} removed", clid);
    }

    /// `deliver_events`: to `clid` and to every client subscribed to a matching channel.
    pub fn deliver(&self, events: Vec<Value>, clid: Option<&str>) {
        let mut clients = self.clients.lock();
        let mut per: HashMap<String, Vec<Value>> = HashMap::new();
        if let Some(c) = clid {
            per.entry(c.to_string()).or_default();
        }
        for ev in &events {
            let ch = ev.get("channel").and_then(|c| c.as_str()).unwrap_or("");
            let mut targets: Vec<String> = clients
                .iter()
                .filter(|(_, c)| c.channels.iter().any(|p| channel_matches(p, ch)))
                .map(|(k, _)| k.clone())
                .collect();
            if let Some(c) = clid {
                if !targets.iter().any(|t| t == c) {
                    targets.push(c.to_string());
                }
            }
            for t in targets {
                per.entry(t).or_default().push(ev.clone());
            }
        }
        for (k, evs) in per {
            let Some(c) = clients.get_mut(&k) else {
                continue;
            };
            if let Some(tx) = &c.stream {
                let mut out = std::mem::take(&mut c.pending);
                out.extend(evs);
                if out.is_empty() {
                    continue;
                }
                if tx.send(out.clone()).is_err() {
                    c.stream = None;
                    c.idle_since = Some(crate::util::now());
                    c.pending = out;
                }
            } else {
                c.pending.extend(evs);
                if let Some(n) = &c.poll {
                    n.notify_one();
                }
            }
        }
    }

    fn take_pending(&self, clid: &str) -> Vec<Value> {
        self.clients
            .lock()
            .get_mut(clid)
            .map(|c| std::mem::take(&mut c.pending))
            .unwrap_or_default()
    }

    /// Plain (`/slim/subscribe` without `subscribe:`) notification subscriptions.
    pub fn on_notification(&self, n: &crate::notify::Notification) {
        let mut out: Vec<(String, Value)> = Vec::new();
        {
            let clients = self.clients.lock();
            for (clid, c) in clients.iter() {
                for (resp, (verbs, mac, id)) in &c.plain {
                    if !verbs.iter().any(|v| v == n.first()) {
                        continue;
                    }
                    if let (Some(m), Some(p)) = (mac, n.player) {
                        if *m != p.to_string() {
                            continue;
                        }
                    }
                    let data: Value = match &n.json {
                        Some(j) => Value::Array(j.as_ref().clone()),
                        None => {
                            Value::Array(n.words.iter().map(|w| Value::String(w.clone())).collect())
                        }
                    };
                    out.push((
                        clid.clone(),
                        json!({"channel": resp, "id": id, "data": data, "ext": {"priority": ""}}),
                    ));
                }
            }
        }
        for (clid, ev) in out {
            self.deliver(vec![ev], Some(&clid));
        }
    }

    /// Evict idle clients (long-poll not renewed within 180 s, streaming lost for 10 s).
    pub fn janitor(&self, server: &Server) {
        let now = crate::util::now();
        let dead: Vec<String> = self
            .clients
            .lock()
            .iter()
            .filter(|(_, c)| {
                c.stream.is_none()
                    && c.poll.is_none()
                    && c.idle_since
                        .map(|t| now - t > AUTOKILL_SECS)
                        .unwrap_or(false)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for d in dead {
            self.remove_client(server, &d);
        }
    }
}

/// Background tasks: plain-subscription delivery and eviction.
pub async fn run(server: Arc<Server>) {
    let mut rx = server.notifier.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            n = rx.recv() => match n {
                Ok(n) => server.cometd.on_notification(&n),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            },
            _ = tick.tick() => server.cometd.janitor(&server),
        }
    }
}

fn extract_message(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &Bytes,
) -> Option<String> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body_s = String::from_utf8_lossy(body).to_string();
    if ct.starts_with("text/json") || ct.starts_with("application/json") {
        return Some(body_s);
    }
    if !body_s.trim().is_empty() {
        if body_s.contains('=') && !body_s.trim_start().starts_with('[') {
            for pair in body_s.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "message" {
                        return Some(
                            percent_encoding::percent_decode_str(&v.replace('+', " "))
                                .decode_utf8_lossy()
                                .to_string(),
                        );
                    }
                }
            }
        }
        return Some(body_s);
    }
    query.get("message").cloned()
}

fn json_response(events: Vec<Value>) -> Response {
    let body = serde_json::to_vec(&Value::Array(events)).unwrap_or_default();
    debug!("cometd -> {}", String::from_utf8_lossy(&body));
    let mut r = Response::new(Body::from(body));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    h.insert(header::EXPIRES, HeaderValue::from_static("-1"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

enum Transport {
    None,
    Streaming,
    LongPoll(u64),
}

pub async fn handle(
    State(server): State<Arc<Server>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(msg) = extract_message(&headers, &query, &body) else {
        return json_response(vec![
            json!({"successful": false, "error": "no bayeux message found"}),
        ]);
    };
    let parsed: Value = match serde_json::from_str(&msg) {
        Ok(v) => v,
        Err(e) => return json_response(vec![json!({"successful": false, "error": e.to_string()})]),
    };
    let Value::Array(objs) = parsed else {
        return json_response(vec![
            json!({"successful": false, "error": "bayeux message not an array"}),
        ]);
    };
    if objs.iter().any(|o| !o.is_object()) {
        return json_response(vec![
            json!({"successful": false, "error": "bayeux event not a hash"}),
        ]);
    }
    let ua = headers
        .get("x-user-agent")
        .or_else(|| headers.get(header::USER_AGENT))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    debug!("cometd <- {}", msg);
    let mgr = &server.cometd;
    let mut events: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut clid: Option<String> = None;
    let mut transport = Transport::None;
    let mut first_event: Option<Value> = None;
    let mut delayed = false;

    for obj in &objs {
        let channel = obj
            .get("channel")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        let msgid = obj
            .get("id")
            .cloned()
            .filter(|v| !v.is_null() && v != &json!(0) && v != &json!(""))
            .unwrap_or(json!(""));
        let data = obj.get("data");
        if clid.is_none() {
            if let Some(c) = obj.get("clientId").and_then(|c| c.as_str()) {
                clid = Some(c.to_string());
            } else if channel == "/meta/handshake" {
                let c = create_uuid();
                mgr.add_client(&c);
                clid = Some(c);
            } else if channel.starts_with("/slim/subscribe") || channel.starts_with("/slim/request")
            {
                clid = data
                    .and_then(|d| d.get("response"))
                    .and_then(|r| r.as_str())
                    .and_then(clid_from_channel);
            } else if channel.starts_with("/slim/unsubscribe") {
                clid = data
                    .and_then(|d| d.get("unsubscribe"))
                    .and_then(|r| r.as_str())
                    .and_then(clid_from_channel);
            }
            if clid.is_none() {
                errors.push(
                    json!({"channel": channel, "error": "No clientId found", "id": obj.get("id")}),
                );
                break;
            }
        }
        let c = clid.clone().unwrap();
        if !mgr.is_valid(&c) {
            events.push(json!({
                "id": msgid, "channel": channel, "clientId": null, "successful": false,
                "timestamp": http_date(), "error": "invalid clientId",
                "advice": {"reconnect": "handshake", "interval": 0}
            }));
            break;
        }
        match channel.as_str() {
            "/meta/handshake" => {
                events.push(json!({
                    "id": msgid, "channel": "/meta/handshake", "version": "1.0",
                    "supportedConnectionTypes": ["long-polling", "streaming"],
                    "clientId": c, "successful": true,
                    "advice": {"reconnect": "retry", "interval": 0, "timeout": LONG_POLLING_TIMEOUT}
                }));
            }
            "/meta/connect" | "/meta/reconnect" => {
                let streaming =
                    obj.get("connectionType").and_then(|v| v.as_str()) == Some("streaming");
                first_event = Some(json!({
                    "id": msgid, "channel": channel, "clientId": c, "successful": true,
                    "timestamp": http_date(), "advice": {"interval": if streaming { RETRY_DELAY } else { 0 }}
                }));
                if streaming {
                    transport = Transport::Streaming;
                } else {
                    let mut timeout = obj
                        .get("advice")
                        .and_then(|a| a.get("timeout"))
                        .and_then(|t| t.as_u64())
                        .unwrap_or(LONG_POLLING_TIMEOUT);
                    if mgr
                        .clients
                        .lock()
                        .get(&c)
                        .map(|cl| !cl.pending.is_empty())
                        .unwrap_or(false)
                    {
                        timeout = 0;
                    }
                    transport = Transport::LongPoll(timeout);
                    delayed = true;
                }
            }
            "/meta/disconnect" => {
                events.push(json!({"id": msgid, "channel": "/meta/disconnect", "clientId": c, "successful": true, "timestamp": http_date()}));
                mgr.remove_client(&server, &c);
            }
            "/meta/subscribe" | "/meta/unsubscribe" => {
                let subs: Vec<String> = match obj.get("subscription") {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    _ => vec![],
                };
                let sub = channel == "/meta/subscribe";
                {
                    let mut clients = mgr.clients.lock();
                    if let Some(cl) = clients.get_mut(&c) {
                        for s in &subs {
                            if sub {
                                if !cl.channels.contains(s) {
                                    cl.channels.push(s.clone());
                                }
                            } else {
                                cl.channels.retain(|x| x != s);
                            }
                        }
                    }
                }
                for s in subs {
                    events.push(json!({"id": msgid, "channel": channel, "clientId": c, "successful": true, "subscription": s}));
                }
            }
            "/slim/subscribe" | "/slim/request" => {
                let is_sub = channel == "/slim/subscribe";
                let id = obj.get("id").cloned().unwrap_or(json!(0));
                let request = data.and_then(|d| d.get("request"));
                let response = data
                    .and_then(|d| d.get("response"))
                    .and_then(|r| r.as_str())
                    .map(str::to_string);
                let priority = data
                    .and_then(|d| d.get("priority"))
                    .cloned()
                    .unwrap_or(json!(""));
                let Some(request) = request else {
                    errors.push(json!({"channel": channel, "error": "request data key not found", "id": id}));
                    continue;
                };
                let Some(response) = response else {
                    errors.push(json!({"channel": channel, "error": "response data key not found", "id": id}));
                    continue;
                };
                let response_clid = clid_from_channel(&response).unwrap_or_else(|| c.clone());
                match handle_request(
                    &server,
                    is_sub,
                    request,
                    &response,
                    &id,
                    &priority,
                    &response_clid,
                    &ua,
                )
                .await
                {
                    Ok(result) => {
                        let id_truthy = !(id.is_null() || id == json!(0) || id == json!(""));
                        if is_sub || id_truthy {
                            events.push(json!({"channel": channel, "clientId": c, "successful": true, "id": id}));
                        }
                        if let Some(res) = result {
                            if !is_sub && !id_truthy {
                                continue;
                            }
                            // LMS: results go inline for long-polling (and CLI) connections;
                            // streaming clients get them on the stream, routed by channel.
                            let streaming_client = matches!(transport, Transport::Streaming)
                                || mgr
                                    .clients
                                    .lock()
                                    .get(&c)
                                    .map(|cl| cl.stream.is_some())
                                    .unwrap_or(false);
                            if matches!(transport, Transport::LongPoll(_)) || !streaming_client {
                                events.push(res);
                                if is_sub {
                                    delayed = false;
                                }
                            } else {
                                // to the client named in the response channel even if its
                                // /meta/subscribe for that channel hasn't arrived yet (the
                                // CometD Java client in Squeezer sends them concurrently)
                                mgr.deliver(vec![res], Some(&response_clid));
                            }
                        }
                    }
                    Err((err, need_client)) => {
                        let mut e = json!({"channel": channel, "error": err, "id": id});
                        if need_client && is_sub {
                            e["advice"] = json!({"reconnect": "retry"});
                            errors.push(e);
                            break;
                        }
                        errors.push(e);
                    }
                }
            }
            "/slim/unsubscribe" => {
                let unsub = data
                    .and_then(|d| d.get("unsubscribe"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                {
                    let mut clients = mgr.clients.lock();
                    if let Some(cl) = clients.get_mut(&c) {
                        cl.plain.remove(&unsub);
                    }
                }
                server
                    .subscriptions
                    .unregister_channel(&format!("cometd:{}", c), &unsub);
                events.push(json!({"channel": "/slim/unsubscribe", "clientId": c, "id": obj.get("id"), "successful": true, "data": data}));
            }
            other => {
                if !other.starts_with("/service/") {
                    mgr.deliver(vec![obj.clone()], None);
                }
                events.push(json!({"channel": other, "id": obj.get("id"), "successful": true}));
            }
        }
    }
    for mut e in errors {
        e["successful"] = json!(false);
        events.push(e);
    }

    let Some(c) = clid else {
        return json_response(events);
    };
    match transport {
        Transport::Streaming => {
            let (tx, rx) = mpsc::unbounded_channel::<Vec<Value>>();
            let mut first = Vec::new();
            if let Some(f) = first_event {
                first.push(f);
            }
            first.extend(events);
            first.extend(mgr.take_pending(&c));
            {
                let mut clients = mgr.clients.lock();
                if let Some(cl) = clients.get_mut(&c) {
                    cl.stream = Some(tx);
                    cl.idle_since = None;
                }
            }
            debug!("cometd {}: streaming connection established", c);
            let first_chunk =
                Bytes::from(serde_json::to_vec(&Value::Array(first)).unwrap_or_default());
            debug!(
                "cometd {} stream -> {}",
                c,
                String::from_utf8_lossy(&first_chunk)
            );
            let rest = futures::stream::unfold(
                (rx, server.clone(), c.clone()),
                |(mut rx, server, c)| async move {
                    match rx.recv().await {
                        Some(evs) => {
                            let chunk = serde_json::to_vec(&Value::Array(evs)).unwrap_or_default();
                            debug!("cometd {} stream -> {}", c, String::from_utf8_lossy(&chunk));
                            Some((
                                Ok::<Bytes, std::io::Error>(Bytes::from(chunk)),
                                (rx, server, c),
                            ))
                        }
                        None => {
                            debug!("cometd {}: streaming connection ended", c);
                            // connection replaced or client removed: start the 10 s grace period
                            let mut clients = server.cometd.clients.lock();
                            if let Some(cl) = clients.get_mut(&c) {
                                cl.idle_since = Some(
                                    crate::util::now() - AUTOKILL_SECS + STREAM_LOSS_GRACE_SECS,
                                );
                            }
                            None
                        }
                    }
                },
            );
            let stream =
                futures::stream::once(async move { Ok::<Bytes, std::io::Error>(first_chunk) });
            use futures::StreamExt;
            let body = Body::from_stream(stream.chain(rest));
            let mut r = Response::new(body);
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            h.insert(header::EXPIRES, HeaderValue::from_static("-1"));
            h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            r
        }
        Transport::LongPoll(timeout) if delayed => {
            let notify = Arc::new(Notify::new());
            {
                let mut clients = mgr.clients.lock();
                if let Some(cl) = clients.get_mut(&c) {
                    cl.poll = Some(notify.clone());
                    cl.idle_since = None;
                }
            }
            if timeout > 0
                && mgr
                    .clients
                    .lock()
                    .get(&c)
                    .map(|cl| cl.pending.is_empty())
                    .unwrap_or(true)
            {
                let _ =
                    tokio::time::timeout(Duration::from_millis(timeout), notify.notified()).await;
            }
            let mut out = Vec::new();
            if let Some(f) = first_event {
                out.push(f);
            }
            out.extend(events);
            {
                let mut clients = mgr.clients.lock();
                if let Some(cl) = clients.get_mut(&c) {
                    out.extend(std::mem::take(&mut cl.pending));
                    cl.poll = None;
                    cl.idle_since = Some(crate::util::now());
                }
            }
            json_response(out)
        }
        _ => {
            let mut out = Vec::new();
            if let Some(f) = first_event {
                out.push(f);
            }
            out.extend(events);
            json_response(out)
        }
    }
}

/// `handleRequest`: run a request for a Cometd client; returns the result event (if sync).
#[allow(clippy::too_many_arguments)]
async fn handle_request(
    server: &Arc<Server>,
    is_sub: bool,
    request: &Value,
    response: &str,
    id: &Value,
    priority: &Value,
    clid: &str,
    ua: &str,
) -> Result<Option<Value>, (String, bool)> {
    let Some(arr) = request.as_array() else {
        return Err(("invalid request arguments, array expected".into(), false));
    };
    let mac = arr
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let Some(args) = arr.get(1).and_then(|a| a.as_array()) else {
        return Err(("invalid request arguments, array expected".into(), false));
    };
    let tokens: Vec<String> = args
        .iter()
        .map(crate::web::jsonrpc::value_to_token)
        .collect();
    let has_subscribe = tokens.iter().any(|t| t.starts_with("subscribe:"));
    if is_sub && !has_subscribe {
        // plain notification subscription: first verb in the given list
        let mut clients = server.cometd.clients.lock();
        if let Some(cl) = clients.get_mut(clid) {
            let player = server.resolve_client(&mac).map(|p| p.id.to_string());
            cl.plain
                .insert(response.to_string(), (tokens.clone(), player, id.clone()));
        }
        return Ok(None);
    }
    let player = if mac.is_empty() {
        None
    } else {
        server.resolve_client(&mac)
    };
    let client = player.as_ref().map(|p| p.id);
    let connected = player.as_ref().map(|p| p.is_connected()).unwrap_or(false);
    let raw = if mac.is_empty() {
        None
    } else {
        Some(mac.as_str())
    };
    let mut req = server.dispatch.parse(client, raw, connected, &tokens);
    if req.status != crate::control::request::Status::Dispatchable {
        let need = req.status == crate::control::request::Status::NeedsClient;
        return Err((format!("invalid request: {}", status_text(&req)), need));
    }
    req.source = Source::Cometd(clid.to_string());
    let _ = ua;
    let req = crate::control::dispatch::execute(server, req).await;
    if req.status.is_error() {
        return Err((
            format!("request failed with error: {}", status_text(&req)),
            false,
        ));
    }
    let id_truthy = !(id.is_null() || id == &json!(0) || id == &json!(""));
    if !id_truthy {
        return Ok(None);
    }
    if has_subscribe && req.is_query {
        let resp = response.to_string();
        let id2 = id.clone();
        let prio = priority.clone();
        let mgr_server = server.clone();
        let c = clid.to_string();
        let sink: crate::control::subscribe::Sink = Arc::new(move |r: Request| {
            if !mgr_server.cometd.is_valid(&c) {
                return false;
            }
            let ev = json!({"channel": resp, "id": id2, "data": Value::Object(r.results), "ext": {"priority": prio}});
            mgr_server.cometd.deliver(vec![ev], Some(&c));
            true
        });
        server.subscriptions.register_with_channel(
            server,
            &format!("cometd:{}", clid),
            response,
            &req,
            sink,
        );
    }
    Ok(Some(
        json!({"channel": response, "id": id, "data": Value::Object(req.results.clone()), "ext": {"priority": priority}}),
    ))
}

pub fn status_text(req: &Request) -> &'static str {
    use crate::control::request::Status::*;
    match req.status {
        New => "New",
        Dispatchable => "Dispatchable",
        Processing => "Processing",
        Done => "Done",
        BadDispatch => "Bad dispatch!",
        BadParams => "Bad params!",
        NeedsClient => "Missing client!",
        NotDispatchable => "Unknown in dispatch table",
        BadConfig => "Bad Lyrion Music Server config",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(channel_matches(
            "/abcd1234/slim/**",
            "/abcd1234/slim/request/5"
        ));
        assert!(channel_matches(
            "/abcd1234/slim/request/*",
            "/abcd1234/slim/request/5"
        ));
        assert!(!channel_matches(
            "/abcd1234/slim/request/*",
            "/abcd1234/slim/request"
        ));
        assert!(channel_matches(
            "/abcd1234/slim/serverstatus",
            "/abcd1234/slim/serverstatus"
        ));
        assert_eq!(
            clid_from_channel("/abcd1234/slim/request").as_deref(),
            Some("abcd1234")
        );
        assert_eq!(clid_from_channel("/slim/request"), None);
    }
}
