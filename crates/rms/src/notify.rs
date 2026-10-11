//! Server-wide notification bus.
//!
//! Every state change that the reference server reports via `notifyFromArray`, and every
//! successfully executed command, is published here as a [`Notification`].  CLI
//! `listen`/`subscribe`, Cometd subscriptions and query subscriptions (`status subscribe:`)
//! all consume this stream.

use crate::control::request::{Request, Source};
use slimproto::MacAddress;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Notification {
    /// Player the notification concerns, if any.
    pub player: Option<MacAddress>,
    /// Rendered request (`renderAsArray`): verbs, params and results,
    /// e.g. `["playlist", "newsong", "Title", "3"]` or `["mixer", "volume", "50"]`.
    pub words: Arc<Vec<String>>,
    /// JSON rendering when the notification carries structured data (e.g. `menustatus`).
    pub json: Option<Arc<Vec<serde_json::Value>>>,
    pub source: Source,
}

impl Notification {
    pub fn first(&self) -> &str {
        self.words.first().map(String::as_str).unwrap_or("")
    }

    pub fn second(&self) -> &str {
        self.words.get(1).map(String::as_str).unwrap_or("")
    }

    pub fn word(&self, i: usize) -> &str {
        self.words.get(i).map(String::as_str).unwrap_or("")
    }

    /// `subscribe a,b,c` CLI semantics: matches when the first verb is in the list.
    pub fn first_in(&self, list: &[String]) -> bool {
        list.iter().any(|f| f == self.first())
    }
}

#[derive(Clone)]
pub struct Notifier {
    tx: broadcast::Sender<Notification>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    pub fn new() -> Notifier {
        let (tx, _) = broadcast::channel(8192);
        Notifier { tx }
    }

    /// `notifyFromArray`.
    pub fn notify(&self, player: Option<MacAddress>, words: &[&str]) {
        self.notify_owned(player, words.iter().map(|s| s.to_string()).collect());
    }

    pub fn notify_owned(&self, player: Option<MacAddress>, words: Vec<String>) {
        tracing::debug!(
            "notify {} {:?}",
            player.map(|p| p.to_string()).unwrap_or_default(),
            words
        );
        let _ = self.tx.send(Notification {
            player,
            words: Arc::new(words),
            json: None,
            source: Source::Internal,
        });
    }

    /// Notification of an executed command.
    pub fn notify_request(&self, req: &Request) {
        // listeners must not see secrets either
        let words = crate::control::request::mask_secrets(&req.render_as_array());
        tracing::debug!(
            "notify {} {:?}",
            req.client.map(|p| p.to_string()).unwrap_or_default(),
            words
        );
        let _ = self.tx.send(Notification {
            player: req.client,
            words: Arc::new(words),
            json: None,
            source: req.source.clone(),
        });
    }

    /// Notification with a structured JSON form (Cometd delivers `json`, the CLI `words`).
    pub fn notify_json(&self, player: Option<MacAddress>, json: Vec<serde_json::Value>) {
        let words = json
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        let _ = self.tx.send(Notification {
            player,
            words: Arc::new(words),
            json: Some(Arc::new(json)),
            source: Source::Internal,
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listeners_never_see_the_spotify_login_code() {
        let n = Notifier::new();
        let mut rx = n.subscribe();
        let mut r = Request::blank();
        r.terms = vec!["spotify".into(), "login".into()];
        r.params.insert(
            "code".into(),
            Some("http://127.0.0.1:5588/login?code=AQDsecret".into()),
        );
        r.add_result("user", "someone");
        n.notify_request(&r);
        let words = rx.try_recv().unwrap().words;
        assert_eq!(*words, ["spotify", "login", "code:******", "user:someone"]);
    }
}
