//! Slimproto TCP listener and per-player connection handling (`Slim::Networking::Slimproto`).

use super::controller::Controller;
use super::Player;
use crate::server::Server;
use futures::{SinkExt, StreamExt};
use slimproto::client::{ClientMessage, DisconnectReason, Stat, StatEvent};
use slimproto::codec::SlimprotoCodec;
use slimproto::server::ServerFrame;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tracing::{debug, info, warn};

/// Seconds between `check_all_clients` passes.
const CHECK_INTERVAL: u64 = 5;
/// A player is considered dead after this many seconds of silence.
const DEAD_AFTER: f64 = 15.0;

pub async fn listen(server: Arc<Server>) -> anyhow::Result<()> {
    let addr = format!(
        "{}:{}",
        server.config.bind_addr, server.config.slimproto_port
    );
    let listener = TcpListener::bind(&addr).await?;
    info!("slimproto listening on {}", addr);
    tokio::spawn(heartbeat(server.clone()));
    loop {
        let (sock, peer) = listener.accept().await?;
        let server = server.clone();
        tokio::spawn(async move {
            let _ = sock.set_nodelay(true);
            if let Err(e) = handle(server, sock, peer).await {
                debug!("slimproto connection {} ended: {}", peer, e);
            }
        });
    }
}

/// Every 5 s: drop silent players and ask everybody for a status report (`strm t`).
async fn heartbeat(server: Arc<Server>) {
    let mut interval = tokio::time::interval(Duration::from_secs(CHECK_INTERVAL));
    loop {
        interval.tick().await;
        let now = crate::util::now();
        for p in server.connected_players() {
            let last = p.state.lock().last_heard;
            if now - last >= DEAD_AFTER {
                warn!(
                    "{}: no response for {:.0}s, disconnecting",
                    p.id,
                    now - last
                );
                server.close_connection(&p.id);
                continue;
            }
            p.request_status();
        }
        // sleep timers
        for p in server.connected_players() {
            let sleep_at = p.state.lock().sleep_time;
            if sleep_at > 0.0 && now >= sleep_at {
                p.state.lock().sleep_time = 0.0;
                info!("{}: sleep timer expired", p.id);
                p.controller().lock().stop();
                server.set_power(&p, false, false);
            }
        }
    }
}

async fn handle(
    server: Arc<Server>,
    sock: TcpStream,
    peer: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let mut framed = Framed::new(sock, SlimprotoCodec);
    // The first frame must be HELO within 5 seconds.
    let first = tokio::time::timeout(Duration::from_secs(5), framed.next()).await;
    let helo = match first {
        Ok(Some(Ok(ClientMessage::Helo(h)))) => h,
        Ok(Some(Ok(other))) => {
            warn!("{}: expected HELO, got {}", peer, other.op());
            return Ok(());
        }
        Ok(Some(Err(e))) => return Err(e.into()),
        Ok(None) => return Ok(()),
        Err(_) => {
            debug!("{}: no HELO within 5s", peer);
            return Ok(());
        }
    };
    info!(
        "{}: HELO device={} ({}) rev={} mac={} reconnect={} caps={}",
        peer,
        helo.device_id,
        helo.model().unwrap_or("?"),
        helo.revision,
        helo.mac,
        helo.reconnect,
        helo.capabilities.as_deref().unwrap_or("")
    );
    if helo.model().is_none() {
        warn!("{}: unknown device id {}, closing", peer, helo.device_id);
        framed.send(ServerFrame::Dsco).await?;
        return Ok(());
    }

    let (sink, mut stream) = framed.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerFrame>();
    let (player, conn_id, cancel) = server.player_connected(&helo, peer, tx);

    let writer_cancel = cancel.clone();
    let writer = tokio::spawn(async move {
        let mut sink = sink;
        loop {
            tokio::select! {
                _ = writer_cancel.cancelled() => break,
                f = rx.recv() => match f {
                    Some(frame) => {
                        if sink.send(frame).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
        let _ = sink.close().await;
    });

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = server.shutdown.cancelled() => break,
            msg = stream.next() => match msg {
                Some(Ok(m)) => handle_message(&server, &player, m),
                Some(Err(e)) => {
                    warn!("{}: protocol error: {}", player.id, e);
                    break;
                }
                None => break,
            }
        }
    }
    cancel.cancel();
    let _ = writer.await;
    server.player_disconnected(&player, conn_id);
    Ok(())
}

fn ctrl(player: &Player) -> Arc<parking_lot::Mutex<Controller>> {
    player.controller()
}

pub fn handle_message(server: &Arc<Server>, player: &Arc<Player>, msg: ClientMessage) {
    match msg {
        ClientMessage::Stat(stat) => handle_stat(server, player, stat),
        ClientMessage::Dsco(reason) => handle_dsco(player, reason),
        ClientMessage::Resp(headers) => handle_resp(server, player, &headers),
        ClientMessage::Meta(meta) => handle_meta(server, player, &meta),
        ClientMessage::Body(body) => {
            debug!("{}: BODY {} bytes (ignored)", player.id, body.len());
        }
        ClientMessage::Setd { id, value } => handle_setd(server, player, id, &value),
        ClientMessage::Ir { time, code } => {
            crate::control::ir::handle_ir(server, player, code, time)
        }
        ClientMessage::Butn { time, button } => {
            crate::control::ir::handle_button(server, player, button, time)
        }
        ClientMessage::Knob { position, .. } => crate::ui::send(
            server,
            player,
            crate::ui::Event::Knob(if position & 0x8000_0000 != 0 {
                -((position & 0x7fff_ffff) as i32)
            } else {
                position as i32
            }),
        ),
        ClientMessage::Bye { upgrading } => info!("{}: BYE! upgrading={}", player.id, upgrading),
        ClientMessage::Ureq => info!("{}: requests firmware update (not supported)", player.id),
        ClientMessage::Dbug(text) => info!("{} firmware: {}", player.id, text),
        ClientMessage::Anic => player.state.lock().animating_until = 0.0,
        ClientMessage::Alss { .. } | ClientMessage::RawIr(_) => {}
        ClientMessage::Shut => server.close_connection(&player.id),
        ClientMessage::Helo(_) => warn!("{}: unexpected second HELO", player.id),
        ClientMessage::Unknown { op, .. } => warn!(
            "{}: unknown slimproto op {:?}",
            player.id,
            String::from_utf8_lossy(&op)
        ),
    }
}

/// `_stat_handler` + `Squeezebox2::statHandler`.
fn handle_stat(server: &Arc<Server>, player: &Arc<Player>, stat: Stat) {
    player.record_stat(&stat);
    let ev = stat.event();
    if ev != StatEvent::Timer {
        debug!(
            "{}: STAT {} fullness={} output={} elapsed={:.3}",
            player.id,
            ev.code(),
            stat.fullness,
            stat.output_buffer_fullness,
            stat.elapsed_milliseconds as f64 / 1000.0
        );
    }
    {
        let mut s = player.state.lock();
        if ev == StatEvent::Connect {
            s.stream_start_ts = Some(stat.jiffies);
        } else if s.stream_start_ts.is_none() {
            return;
        }
    }
    let c = ctrl(player);
    let id = player.id;
    match ev {
        StatEvent::DecoderReady => {
            player.state.lock().ready_to_stream = true;
            c.lock().player_ready_to_stream(id);
        }
        StatEvent::NotSupported => {
            player.state.lock().ready_to_stream = true;
            let err = if stat.error_code != 0 {
                format!("DECODE_ERROR_{}", stat.error_code)
            } else {
                "PROBLEM_OPENING".to_string()
            };
            warn!(
                "{}: decoder does not support file format, code {}",
                id, stat.error_code
            );
            c.lock().player_streaming_failed(id, &err, false);
        }
        StatEvent::BufferReady => {
            player.state.lock().buffer_ready = true;
            c.lock().player_buffer_ready(id);
        }
        StatEvent::Underrun => {
            player.state.lock().ready_to_stream = true;
            c.lock().player_stopped(id);
        }
        StatEvent::Autostart => {
            player.state.lock().buffer_ready = true;
        }
        StatEvent::Connect => {
            let mut s = player.state.lock();
            s.ready_to_stream = false;
            s.buffer_ready = false;
            s.connecting = true;
        }
        StatEvent::TrackStarted => c.lock().player_track_started(id),
        StatEvent::OutputUnderrun => c.lock().player_output_underrun(id),
        StatEvent::EndOfStream => c.lock().player_end_of_stream(id),
        _ => c.lock().player_status_heartbeat(id),
    }
    let _ = server;
}

/// `_disco_handler`.
fn handle_dsco(player: &Arc<Player>, reason: DisconnectReason) {
    info!(
        "{}: data stream disconnected: {}",
        player.id,
        reason.description()
    );
    {
        let mut s = player.state.lock();
        if s.connecting {
            s.connecting = false;
            s.ready_to_stream = true;
        }
    }
    let c = ctrl(player);
    let mut c = c.lock();
    if reason != DisconnectReason::Normal && !c.is_streamout() {
        let playing = c.is_playing(true);
        player.state.lock().ready_to_stream = true;
        c.player_streaming_failed(player.id, reason.description(), playing);
        if c.is_playing(false) && player.buffer_fullness() == 0 {
            c.player_stopped(player.id);
        }
    } else {
        // `statHandler('EoS')` honours the STMc gate as well
        if player.state.lock().stream_start_ts.is_some() {
            c.player_end_of_stream(player.id);
        }
    }
}

/// `directHeaders`: the player reports HTTP response headers of its stream connection.
fn handle_resp(server: &Arc<Server>, player: &Arc<Player>, raw: &[u8]) {
    player.state.lock().connecting = false;
    debug!(
        "{}: RESP {}",
        player.id,
        String::from_utf8_lossy(raw).trim()
    );
    let c = ctrl(player);
    let mut c = c.lock();
    let Some(song) = c.queue.front().cloned() else {
        return;
    };
    let Some(direct_url) = song.direct_url.clone() else {
        return;
    };
    let Some(h) = crate::stream::remote::parse_direct_headers(raw) else {
        c.player_streaming_failed(player.id, "PROBLEM_CONNECTING", false);
        return;
    };
    if h.status < 200 || h.status > 399 {
        warn!(
            "{}: invalid response code {} from remote stream {}",
            player.id, h.status, direct_url
        );
        player.state.lock().ready_to_stream = true;
        c.player_streaming_failed(player.id, &h.status_line, false);
        return;
    }
    if let Some(redir) = h.redirect.filter(|_| (300..400).contains(&h.status)) {
        let new_url = url::Url::parse(&direct_url)
            .and_then(|b| b.join(&redir))
            .map(|u| u.to_string())
            .unwrap_or(redir);
        info!("{}: direct stream redirected to {}", player.id, new_url);
        if let Some(s) = c.queue.front_mut() {
            s.direct_url = Some(new_url);
            let s = s.clone();
            let params = crate::stream::PlayParams {
                paused: c.active_players().len() > 1,
                reconnect: false,
                fade_in: 0.0,
                replay_gain_db: None,
                buffer_threshold: None,
                transition: slimproto::server::TransitionType::None,
                transition_duration: 0,
                shared: None,
            };
            crate::stream::send_stream_start(server, player, &s, &params);
        }
        return;
    }
    if let Some(s) = c.queue.front_mut() {
        if let Some(t) = h.title.clone() {
            s.remote_meta.station = Some(t);
        }
        if let Some(b) = h.bitrate {
            s.stream_bitrate = Some(b);
            s.remote_meta.bitrate = Some(format!("{}kbps", b / 1000));
        }
        if let Some(ct) = &h.content_type {
            s.remote_meta.content_type = Some(ct.clone());
        }
    }
    info!(
        "{}: beginning direct stream (metaint {})",
        player.id, h.metaint
    );
    player.send(ServerFrame::Cont {
        metaint: h.metaint,
        loop_: false,
        guids: Vec::new(),
    });
}

/// `directMetadata`: ICY metadata block received by the player.
fn handle_meta(server: &Arc<Server>, player: &Arc<Player>, meta: &[u8]) {
    let (title, _url) = crate::stream::remote::parse_icy_metadata(meta);
    let Some(title) = title else { return };
    let c = ctrl(player);
    let mut c = c.lock();
    let master = c.master_id;
    let Some(song) = c.queue.front_mut() else {
        return;
    };
    if song.remote_meta.title.as_deref() == Some(title.as_str()) {
        return;
    }
    info!("{}: stream title \"{}\"", player.id, title);
    song.remote_meta.title = Some(title.clone());
    server.notifier.notify_owned(
        Some(master),
        vec!["playlist".into(), "newsong".into(), title],
    );
}

/// `playerSettingsFrame`.
fn handle_setd(server: &Arc<Server>, player: &Arc<Player>, id: u8, value: &[u8]) {
    match id {
        0 => {
            player.state.lock().name_query_pending = false;
            let name = String::from_utf8_lossy(value)
                .trim_end_matches('\0')
                .trim()
                .to_string();
            if name.is_empty() {
                let ours = player.name();
                player.push_player_name(&ours);
            } else if name != player.name() {
                info!("{}: player reports name \"{}\"", player.id, name);
                player.state.lock().name = name.clone();
                player.prefs.set("playername", serde_json::json!(name));
                server.notifier.notify(Some(player.id), &["name", &name]);
            }
        }
        4 => {
            if let Some(v) = value.first() {
                player.prefs.set("disableDac", serde_json::json!(*v));
            }
        }
        _ => debug!("{}: SETD id {} ignored", player.id, id),
    }
}
