//! The native API (`/api/v1`): typed JSON resources, player commands and a pushed event
//! stream.  See `docs/api.md`.

pub mod commands;
pub mod events;
pub mod model;
pub mod state;

use crate::server::Server;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use model::{ApiError, Command};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/v1/server", get(get_server))
        .route("/api/v1/players", get(get_players))
        .route("/api/v1/players/{id}", get(get_player))
        .route("/api/v1/players/{id}/queue", get(get_queue))
        .route("/api/v1/players/{id}/command", post(post_command))
        .route("/api/v1/events", get(events::handle))
        .route("/api/v1/{*rest}", get(not_found).post(not_found))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ApiError { error: msg.into() })).into_response()
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "no such resource")
}

fn unknown_player(id: &str) -> Response {
    error(StatusCode::NOT_FOUND, format!("unknown player {}", id))
}

async fn get_server(State(server): State<Arc<Server>>) -> Response {
    Json(state::server_info(&server)).into_response()
}

async fn get_players(State(server): State<Arc<Server>>) -> Response {
    Json(state::players(&server)).into_response()
}

async fn get_player(State(server): State<Arc<Server>>, Path(id): Path<String>) -> Response {
    match server.resolve_client(&id) {
        Some(p) => Json(state::player(&server, &p)).into_response(),
        None => unknown_player(&id),
    }
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn get_queue(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Response {
    let limit = page.limit.unwrap_or(usize::MAX);
    match state::queue(&server, &id, page.offset, limit) {
        Some(q) => Json(q).into_response(),
        None => unknown_player(&id),
    }
}

async fn post_command(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    body: Result<Json<Command>, JsonRejection>,
) -> Response {
    let Some(p) = server.resolve_client(&id) else {
        return unknown_player(&id);
    };
    let cmd = match body {
        Ok(Json(c)) => c,
        Err(e) => return error(StatusCode::BAD_REQUEST, e.body_text()),
    };
    match commands::run(&server, &p, cmd).await {
        Ok(r) => Json(r).into_response(),
        Err(commands::CommandError::Invalid(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(commands::CommandError::Failed(m)) => error(StatusCode::UNPROCESSABLE_ENTITY, m),
    }
}
