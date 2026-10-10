//! Server-wide notification bus.
//!
//! Every state change that the reference server reports via `notifyFromArray` is published
//! here as a [`Notification`].  CLI `listen`/`subscribe`, Cometd subscriptions and query
//! subscriptions (`status subscribe:`) all consume this stream.

use slimproto::MacAddress;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Notification {
    /// Player the notification concerns, if any.
    pub player: Option<MacAddress>,
    /// Command words, e.g. `["playlist", "newsong", "Title", "3"]` or `["client", "new"]`.
    pub words: Arc<Vec<String>>,
}

impl Notification {
    pub fn new(player: Option<MacAddress>, words: Vec<String>) -> Self {
        Notification { player, words: Arc::new(words) }
    }

    pub fn first(&self) -> &str {
        self.words.first().map(String::as_str).unwrap_or("")
    }

    pub fn second(&self) -> &str {
        self.words.get(1).map(String::as_str).unwrap_or("")
    }

    /// `"playlist newsong"` style key used for matching subscriptions.
    pub fn matches(&self, filter: &[String]) -> bool {
        if filter.is_empty() {
            return true;
        }
        filter.iter().any(|f| {
            let parts: Vec<&str> = f.split(',').collect();
            parts.iter().all(|p| {
                let p = p.trim();
                self.first() == p || self.second() == p
            }) || f == self.first()
        })
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
        let (tx, _) = broadcast::channel(4096);
        Notifier { tx }
    }

    pub fn notify(&self, player: Option<MacAddress>, words: &[&str]) {
        let n = Notification::new(player, words.iter().map(|s| s.to_string()).collect());
        tracing::debug!("notify {:?} {:?}", player.map(|p| p.to_string()), n.words);
        let _ = self.tx.send(n);
    }

    pub fn notify_owned(&self, player: Option<MacAddress>, words: Vec<String>) {
        let n = Notification::new(player, words);
        tracing::debug!("notify {:?} {:?}", player.map(|p| p.to_string()), n.words);
        let _ = self.tx.send(n);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.tx.subscribe()
    }
}
