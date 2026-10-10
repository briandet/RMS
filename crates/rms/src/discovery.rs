//! UDP discovery responder on port 3483.

use crate::server::Server;
use slimproto::discovery::{DiscoveryRequest, ServerInfo};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

pub async fn run(server: Arc<Server>) -> anyhow::Result<()> {
    let addr = format!("{}:3483", server.config.bind_addr);
    let sock = UdpSocket::bind(&addr).await?;
    sock.set_broadcast(true)?;
    info!("UDP discovery listening on {}", addr);
    let mut buf = vec![0u8; 1500];
    loop {
        let (n, peer) = match sock.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(e) => {
                warn!("discovery recv error: {}", e);
                continue;
            }
        };
        let req = DiscoveryRequest::parse(&buf[..n]);
        let info = ServerInfo {
            name: server.library_name(),
            http_addr: server.config.http_addr.clone(),
            http_port: server.http_port(),
            version: crate::VERSION.to_string(),
            uuid: server.uuid(),
        };
        if let Some(resp) = info.respond(&req) {
            debug!("discovery {:?} from {} -> {} bytes", req, peer, resp.len());
            if let Err(e) = sock.send_to(&resp, peer).await {
                warn!("discovery send to {} failed: {}", peer, e);
            }
        } else if !matches!(req, DiscoveryRequest::Unknown) {
            debug!("ignoring discovery packet {:?} from {}", req, peer);
        }
    }
}
