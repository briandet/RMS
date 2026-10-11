//! UPnP / DLNA media renderers as players.  (Not implemented yet.)

use crate::server::Server;
use std::sync::Arc;

/// Find devices while the protocol is turned on and run a player for each.
pub async fn run(_server: Arc<Server>) {}
