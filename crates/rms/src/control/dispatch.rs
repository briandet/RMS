//! Dispatch table and request execution (`Slim::Control::Request::addDispatch` / `new` / `execute`).

use super::request::{Request, Source, Status};
use crate::server::Server;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;

pub type SyncFn = fn(&Arc<Server>, &mut Request);
pub type AsyncFn = fn(Arc<Server>, Request) -> BoxFuture<'static, Request>;

#[derive(Clone, Copy)]
pub enum Handler {
    /// Pure notification: never dispatchable, can be notified.
    None,
    Sync(SyncFn),
    Async(AsyncFn),
}

/// `needsClient` values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NeedsClient {
    No = 0,
    Yes = 1,
    /// Client id required but may be disconnected (Jive `menu`).
    Disconnected = 2,
}

#[derive(Clone)]
pub struct Entry {
    /// Positional parameter names (`_index`, …) and `?` placeholders, in order.
    pub params: Vec<String>,
    pub needs_client: NeedsClient,
    pub is_query: bool,
    pub has_tags: bool,
    pub handler: Handler,
}

#[derive(Default)]
struct Node {
    children: HashMap<String, Node>,
    /// [command variant, query variant]
    leaf: [Option<usize>; 2],
}

#[derive(Default)]
pub struct DispatchTable {
    entries: Vec<Entry>,
    root: Node,
}

impl DispatchTable {
    /// Register `terms` (verbs followed by `_param`/`?` names).  A later registration of the
    /// same verbs/variant replaces the earlier one, like `addDispatch`.
    pub fn add(
        &mut self,
        terms: &[&str],
        needs_client: u8,
        is_query: bool,
        has_tags: bool,
        handler: Handler,
    ) {
        let mut verbs = Vec::new();
        let mut params = Vec::new();
        for t in terms {
            if params.is_empty() && !t.starts_with('_') && *t != "?" {
                verbs.push(t.to_string());
            } else {
                params.push(t.to_string());
            }
        }
        let variant = if terms.last() == Some(&"?") { 1 } else { 0 };
        let entry = Entry {
            params,
            needs_client: match needs_client {
                0 => NeedsClient::No,
                2 => NeedsClient::Disconnected,
                _ => NeedsClient::Yes,
            },
            is_query,
            has_tags,
            handler,
        };
        let idx = self.entries.len();
        self.entries.push(entry);
        let mut node = &mut self.root;
        for v in &verbs {
            node = node.children.entry(v.clone()).or_default();
        }
        node.leaf[variant] = Some(idx);
    }

    pub fn entry(&self, idx: usize) -> &Entry {
        &self.entries[idx]
    }

    /// Parse a token array into a request (`Request->new`).  `client_known` tells whether
    /// the client id refers to a connected player.
    pub fn parse(
        &self,
        client: Option<slimproto::MacAddress>,
        client_raw: Option<&str>,
        client_connected: bool,
        line: &[String],
    ) -> Request {
        let mut req = Request::blank();
        req.client = client;
        let mut node = &self.root;
        let mut i = 0;
        while i < line.len() {
            match node.children.get(&line[i]) {
                Some(n) => {
                    req.terms.push(line[i].clone());
                    node = n;
                    i += 1;
                }
                None => break,
            }
        }
        let is_query_line = line.last().map(|s| s == "?").unwrap_or(false);
        let found = if req.terms.is_empty() {
            None
        } else {
            node.leaf[if is_query_line { 1 } else { 0 }]
        };
        // A command line that ends in '?' but has no query variant falls back to nothing (104).
        let Some(idx) = found else {
            req.status = Status::NotDispatchable;
            for (j, tok) in line.iter().enumerate().skip(i) {
                req.params.insert(format!("_p{}", j), Some(tok.clone()));
            }
            return req;
        };
        let entry = &self.entries[idx];
        req.entry = Some(idx);
        req.is_query = entry.is_query;
        for p in &entry.params {
            if is_query_line {
                if p != "?" {
                    req.params.insert(p.clone(), line.get(i).cloned());
                }
                i += 1;
            } else {
                req.params.insert(p.clone(), line.get(i).cloned());
                i += 1;
            }
        }
        while i < line.len() {
            let tok = &line[i];
            if entry.has_tags && !tok.is_empty() {
                if let Some((k, v)) = tok.split_once(':') {
                    if !k.is_empty() {
                        req.params.insert(k.to_string(), Some(v.to_string()));
                        i += 1;
                        continue;
                    }
                }
            }
            req.params.insert(format!("_p{}", i), Some(tok.clone()));
            i += 1;
        }
        req.status = match entry.handler {
            Handler::None => Status::NotDispatchable,
            _ => match entry.needs_client {
                NeedsClient::No => Status::Dispatchable,
                NeedsClient::Yes if client.is_some() && client_connected => Status::Dispatchable,
                NeedsClient::Disconnected if client.is_some() || client_raw.is_some() => {
                    if !client_connected {
                        req.disconnected_client = client_raw
                            .map(str::to_string)
                            .or(client.map(|c| c.to_string()));
                        req.client = None;
                    }
                    Status::Dispatchable
                }
                _ => {
                    req.client = None;
                    Status::NeedsClient
                }
            },
        };
        req
    }

    /// `can <terms> ?`: true when the terms resolve to something other than "unknown".
    pub fn can(&self, terms: &[String]) -> bool {
        let req = self.parse(None, None, false, terms);
        req.status != Status::NotDispatchable
    }
}

/// Execute a parsed request.  Commands that finish `Done` are notified to listeners.
pub async fn execute(server: &Arc<Server>, mut req: Request) -> Request {
    if req.status != Status::Dispatchable {
        return req;
    }
    let Some(idx) = req.entry else {
        req.status = Status::NotDispatchable;
        return req;
    };
    let handler = server.dispatch.entry(idx).handler;
    tracing::debug!(
        "execute {:?} {:?} {:?}",
        req.client.map(|c| c.to_string()),
        req.terms,
        req.params
    );
    // mark user activity
    if let Some(p) = req.client.and_then(|c| server.player(&c)) {
        p.state.lock().last_activity = crate::util::now();
    }
    match handler {
        Handler::None => req.status = Status::NotDispatchable,
        Handler::Sync(f) => {
            f(server, &mut req);
        }
        Handler::Async(f) => {
            req = f(server.clone(), req).await;
        }
    }
    if req.status == Status::Dispatchable {
        // handlers that did not set a status explicitly (LMS: request stays dispatchable and
        // is echoed without notification) — queries are treated as done.
        if req.is_query {
            req.status = Status::Done;
        }
    }
    if req.status == Status::Done && !req.is_query {
        server.notifier.notify_request(&req);
    }
    req
}

/// Convenience: build and execute a request from string tokens.
pub async fn execute_tokens(
    server: &Arc<Server>,
    client: Option<slimproto::MacAddress>,
    tokens: Vec<String>,
    source: Source,
) -> Request {
    let connected = client
        .and_then(|c| server.player(&c))
        .map(|p| p.is_connected())
        .unwrap_or(false);
    let client_raw = client.map(|c| c.to_string());
    let mut req = server
        .dispatch
        .parse(client, client_raw.as_deref(), connected, &tokens);
    req.source = source;
    execute(server, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(_: &Arc<Server>, r: &mut Request) {
        r.set_done();
    }

    fn table() -> DispatchTable {
        let mut t = DispatchTable::default();
        t.add(
            &["playlist", "index", "?"],
            1,
            true,
            false,
            Handler::Sync(noop),
        );
        t.add(
            &[
                "playlist",
                "index",
                "_index",
                "_fadein",
                "_noplay",
                "_seekdata",
            ],
            1,
            false,
            false,
            Handler::Sync(noop),
        );
        t.add(
            &["playlist", "artist", "_index", "?"],
            1,
            true,
            false,
            Handler::Sync(noop),
        );
        t.add(
            &["albums", "_index", "_quantity"],
            0,
            true,
            true,
            Handler::Sync(noop),
        );
        t.add(&["version", "?"], 0, true, false, Handler::Sync(noop));
        t.add(&["playlist", "newsong"], 1, false, false, Handler::None);
        t
    }

    fn toks(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn parsing() {
        let t = table();
        let mac = slimproto::MacAddress([0, 4, 32, 1, 2, 3]);
        let r = t.parse(Some(mac), None, true, &toks("playlist index ?"));
        assert!(r.is_query);
        assert_eq!(r.status, Status::Dispatchable);
        assert_eq!(r.terms, vec!["playlist", "index"]);
        let r = t.parse(Some(mac), None, true, &toks("playlist index +1"));
        assert!(!r.is_query);
        assert_eq!(r.param("_index"), Some("+1"));
        assert_eq!(r.param("_fadein"), None);
        let r = t.parse(Some(mac), None, true, &toks("playlist artist 3 ?"));
        assert_eq!(r.param("_index"), Some("3"));
        let r = t.parse(
            None,
            None,
            false,
            &toks("albums 0 10 tags:al artist_id:4 url:http://x/y"),
        );
        assert_eq!(r.param("_quantity"), Some("10"));
        assert_eq!(r.param("tags"), Some("al"));
        assert_eq!(r.param("url"), Some("http://x/y"));
        let r = t.parse(None, None, false, &toks("playlist index 1"));
        assert_eq!(r.status, Status::NeedsClient);
        let r = t.parse(None, None, false, &toks("foo bar"));
        assert_eq!(r.status, Status::NotDispatchable);
        assert_eq!(r.param("_p0"), Some("foo"));
        let r = t.parse(Some(mac), None, true, &toks("playlist newsong x"));
        assert_eq!(r.status, Status::NotDispatchable);
        assert!(!t.can(&toks("version"))); // only query variant registered
    }
}
