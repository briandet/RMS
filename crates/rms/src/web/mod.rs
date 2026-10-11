//! HTTP server: audio streaming to players, JSON-RPC, Cometd, artwork and the web UI
//! (`Slim::Web::HTTP`).

pub mod artwork;
pub mod auth;
pub mod cometd;
pub mod jsonrpc;
pub mod ui;

use crate::server::Server;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::Router;
use bytes::Bytes;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/stream.mp3", get(stream_handler))
        .route("/stream", get(stream_handler))
        .route(
            "/jsonrpc.js",
            post(jsonrpc::handle_post).get(jsonrpc::handle_get),
        )
        .merge(crate::api::router())
        .route("/cometd", any(cometd::handle))
        .route("/cometd/{*rest}", any(cometd::handle))
        .route("/music/{id}/{spec}", get(artwork::music))
        .route("/contributor/{id}/{spec}", get(artwork::contributor))
        .route("/imageproxy/{url}/{spec}", get(artwork::imageproxy))
        .route("/html/images/{name}", get(artwork::html_image))
        .route("/html/{*path}", get(ui::html_compat))
        .route("/", get(ui::index))
        .route("/{*path}", get(ui::static_file))
        .layer(axum::middleware::from_fn_with_state(
            server.clone(),
            auth::require,
        ))
        .with_state(server)
}

fn server_header() -> HeaderValue {
    HeaderValue::from_str(&format!(
        "Lyrion Music Server ({} - rms {})",
        crate::VERSION,
        env!("CARGO_PKG_VERSION")
    ))
    .unwrap()
}

/// `/stream.mp3?player=<mac>`: the audio a player requested via `strm s`.
async fn stream_handler(
    State(server): State<Arc<Server>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(player) = q.get("player").and_then(|id| server.resolve_client(id)) else {
        return (StatusCode::NOT_FOUND, "unknown player").into_response();
    };
    if server.prefs.server.get_bool("authorize") {
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Basic "))
            .and_then(|b| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(b.trim())
                    .ok()
            })
            .map(|d| String::from_utf8_lossy(&d).to_string())
            .map(|cred| {
                Some(cred)
                    == player
                        .stream_password()
                        .map(|p| format!("squeezeboxXXX:{}", p))
            })
            .unwrap_or(false);
        if !ok {
            return (StatusCode::UNAUTHORIZED, "").into_response();
        }
    }
    let Some((generation, spec)) = player.pending_stream() else {
        warn!("{}: stream request without a pending stream", player.id);
        return (StatusCode::NOT_FOUND, "no stream").into_response();
    };
    let cancel = player.stream_cancel_token();
    let source = match crate::stream::source::open(&spec, &server.http).await {
        Ok(s) => s,
        Err(e) => {
            warn!("{}: cannot open stream source: {}", player.id, e);
            let c = player.controller();
            c.lock()
                .player_streaming_failed(player.id, "PROBLEM_OPENING", false);
            return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
        }
    };
    info!(
        "{}: streaming {} ({:?})",
        player.id,
        spec.format,
        match &spec.source {
            crate::stream::SourceKind::File { path, .. } => path.display().to_string(),
            crate::stream::SourceKind::Ffmpeg { .. } => "ffmpeg".into(),
            crate::stream::SourceKind::Remote { url } => url.clone(),
            crate::stream::SourceKind::Spotify { url, .. } => url.clone(),
        }
    );

    struct St {
        src: crate::stream::source::ByteStream,
        cancel: tokio_util::sync::CancellationToken,
        server: Arc<Server>,
        player: Arc<crate::player::Player>,
        generation: u64,
        song_id: u64,
        bytes: u64,
        done: bool,
    }
    let st = St {
        src: source,
        cancel,
        server: server.clone(),
        player: player.clone(),
        generation,
        song_id: spec.song_id,
        bytes: 0,
        done: false,
    };
    let body = futures::stream::unfold(st, |mut st| async move {
        if st.done {
            return None;
        }
        let next = tokio::select! {
            _ = st.cancel.cancelled() => {
                debug!("{}: stream generation {} cancelled", st.player.id, st.generation);
                return None;
            }
            n = st.src.next() => n,
        };
        match next {
            Some(Ok(chunk)) => {
                st.bytes += chunk.len() as u64;
                Some((Ok::<Bytes, std::io::Error>(chunk), st))
            }
            Some(Err(e)) => {
                warn!("{}: stream read error: {}", st.player.id, e);
                st.done = true;
                end_of_stream(&st.server, &st.player, st.generation, st.song_id, st.bytes);
                None
            }
            None => {
                st.done = true;
                end_of_stream(&st.server, &st.player, st.generation, st.song_id, st.bytes);
                None
            }
        }
    });
    let mut resp = Response::new(Body::from_stream(body));
    let h = resp.headers_mut();
    h.insert(header::SERVER, server_header());
    h.insert(header::CONNECTION, HeaderValue::from_static("close"));
    if let Some(ct) = spec.content_type() {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    }
    resp
}

/// `Source::_readNextChunk` end of song: tell the controller the server is done sending.
fn end_of_stream(
    _server: &Arc<Server>,
    player: &Arc<crate::player::Player>,
    generation: u64,
    song_id: u64,
    bytes: u64,
) {
    if player
        .stream_generation
        .load(std::sync::atomic::Ordering::SeqCst)
        != generation
    {
        return;
    }
    player.clear_pending_stream();
    let c = player.controller();
    let mut c = c.lock();
    if !c.is_master(player.id) {
        return;
    }
    if bytes == 0 {
        c.player_streaming_failed(player.id, "PROBLEM_OPENING", false);
    } else {
        debug!("{}: local end of stream after {} bytes", player.id, bytes);
        c.local_end_of_stream(song_id);
    }
}

pub async fn serve(server: Arc<Server>) -> anyhow::Result<()> {
    let addr = format!("{}:{}", server.config.bind_addr, server.http_port());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("HTTP listening on http://{}/", addr);
    let app = router(server.clone()).layer(tower_http::cors::CorsLayer::permissive());
    let shutdown = server.shutdown.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown.cancelled().await })
    .await?;
    Ok(())
}
