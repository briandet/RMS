//! The native API (`/api/v1`): typed JSON resources, player commands and a pushed event
//! stream.  See `docs/api.md`.

pub mod commands;
pub mod events;
pub mod library;
pub mod model;
pub mod playlists;
pub mod services;
pub mod settings;
pub mod state;
#[cfg(test)]
mod typescript;

use crate::server::Server;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use commands::CommandError;
use library::{AlbumParams, FolderParams, LibraryError, ListParams, TrackParams};
use model::{
    ApiError, Command, MoveRequest, NewFavorite, PlayerSettingsPatch, ReleaseChoice, ScanRequest,
    ServerSettingsPatch, SpotifyLogin, SpotifyPatch, TagChanges,
};
use serde::{Deserialize, Serialize};
use services::MenuParams;
use std::sync::Arc;

pub fn router() -> Router<Arc<Server>> {
    Router::new()
        .route("/api/v1/server", get(get_server))
        .route("/api/v1/players", get(get_players))
        .route("/api/v1/players/{id}", get(get_player))
        .route("/api/v1/players/{id}/queue", get(get_queue))
        .route("/api/v1/players/{id}/spectrum", get(get_spectrum))
        .route("/api/v1/players/{id}/spotify-queue", get(get_spotify_queue))
        .route("/api/v1/players/{id}/command", post(post_command))
        .route(
            "/api/v1/players/{id}/announce",
            post(post_announce).layer(axum::extract::DefaultBodyLimit::max(
                crate::player::announce::MAX_BYTES,
            )),
        )
        .route(
            "/api/v1/players/{id}/settings",
            get(get_player_settings).patch(patch_player_settings),
        )
        .route("/api/v1/events", get(events::handle))
        .route(
            "/api/v1/browser-player",
            get(crate::bridge::browser::handle),
        )
        .route("/api/v1/artists", get(get_artists))
        .route("/api/v1/artists/{id}", get(get_artist))
        .route(
            "/api/v1/artists/{id}/top-tracks",
            get(get_artist_top_tracks),
        )
        .route("/api/v1/artists/{id}/similar", get(get_artist_similar))
        .route("/api/v1/albums", get(get_albums))
        .route("/api/v1/albums/{id}", get(get_album))
        .route(
            "/api/v1/albums/{id}/release",
            get(get_album_release)
                .put(put_album_release)
                .delete(delete_album_release),
        )
        .route("/api/v1/albums/{id}/tags", get(get_album_tags))
        .route(
            "/api/v1/albums/{id}/cover-art",
            get(get_cover_art).post(post_cover_art),
        )
        .route(
            "/api/v1/albums/{id}/files",
            get(get_album_files).post(post_album_files),
        )
        .route("/api/v1/tags", post(post_tags))
        .route("/api/v1/tags/edits", get(get_tag_edits))
        .route("/api/v1/tags/edits/{batch}/undo", post(post_tag_undo))
        .route("/api/v1/genres", get(get_genres))
        .route("/api/v1/years", get(get_years))
        .route(
            "/api/v1/tracks",
            get(get_tracks).merge(post(post_track).layer(axum::extract::DefaultBodyLimit::max(
                crate::upload::MAX_BYTES,
            ))),
        )
        .route("/api/v1/tracks/{id}", get(get_track))
        .route("/api/v1/tracks/{id}/similar", get(get_track_similar))
        .route("/api/v1/tracks/{id}/analysis", get(get_track_analysis))
        .route("/api/v1/similar", post(post_similar))
        .route("/api/v1/playlists", get(get_playlists).post(post_playlist))
        .route(
            "/api/v1/playlists/{id}",
            get(get_playlist)
                .patch(patch_playlist)
                .delete(delete_playlist),
        )
        .route(
            "/api/v1/playlists/{id}/tracks",
            get(get_playlist_tracks)
                .put(put_playlist_tracks)
                .post(post_playlist_tracks),
        )
        .route("/api/v1/folders", get(get_folder))
        .route("/api/v1/search", get(get_search))
        .route("/api/v1/favorites", get(get_favorites).post(post_favorite))
        .route(
            "/api/v1/favorites/{id}",
            axum::routing::delete(delete_favorite),
        )
        .route("/api/v1/radio", get(get_radio))
        .route("/api/v1/listenbrainz", get(get_listenbrainz))
        .route(
            "/api/v1/listenbrainz/accounts",
            post(post_listenbrainz_account),
        )
        .route(
            "/api/v1/listenbrainz/accounts/{user}",
            axum::routing::patch(patch_listenbrainz_account).delete(delete_listenbrainz_account),
        )
        .route("/api/v1/spotify", get(get_spotify).patch(patch_spotify))
        .route(
            "/api/v1/spotify/learned",
            get(get_spotify_learned).delete(delete_spotify_learned),
        )
        .route("/api/v1/spotify/items", get(get_spotify_items))
        .route("/api/v1/spotify/login", post(post_spotify_login))
        .route("/api/v1/spotify/logout", post(post_spotify_logout))
        .route("/api/v1/settings", get(get_settings).patch(patch_settings))
        .route("/api/v1/library/scan", post(post_scan))
        .route("/api/v1/library/analysis", get(get_analysis_progress))
        .route("/api/v1/query", post(post_query))
        .route("/api/v1/query/fields", get(get_query_fields))
        .route(
            "/api/v1/query/views",
            get(get_query_views).put(put_query_views),
        )
        .route(
            "/api/v1/{*rest}",
            get(not_found)
                .post(not_found)
                .patch(not_found)
                .delete(not_found),
        )
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
    ApiQuery(page): ApiQuery<Page>,
) -> Response {
    let limit = page.limit.unwrap_or(usize::MAX);
    match state::queue(&server, &id, page.offset, limit) {
        Some(q) => Json(q).into_response(),
        None => unknown_player(&id),
    }
}

/// What Spotify plays next on the player's Connect device: its queue lives in the app, not
/// in the player's playlist (which holds the one endless Connect stream).
async fn get_spotify_queue(State(server): State<Arc<Server>>, Path(id): Path<String>) -> Response {
    use model::{SpotifyQueue, Track};
    let Some(p) = server.resolve_client(&id) else {
        return unknown_player(&id);
    };
    let playing = p
        .controller()
        .lock()
        .playing_song()
        .map(|s| s.url().to_string());
    let connect = playing.as_deref() == Some(crate::plugins::spotify::connect::url(&p.id).as_str());
    let Some(sp) = crate::plugins::spotify::get().filter(|_| connect) else {
        return Json(SpotifyQueue {
            connect: false,
            items: Vec::new(),
            error: None,
        })
        .into_response();
    };
    let (items, error) = match sp
        .queue(&crate::plugins::spotify::connect::key(&p.id))
        .await
    {
        Ok(q) => (q, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let items = items
        .into_iter()
        .map(|(url, m)| {
            let (spotify_album, spotify_artist) = (m.album_menu(), m.artist_menu());
            Track {
                id: None,
                url,
                title: m.title,
                artist: Some(m.artist).filter(|a| !a.is_empty()),
                artist_id: None,
                album: Some(m.album).filter(|a| !a.is_empty()),
                album_id: None,
                duration: Some(m.secs).filter(|&d| d > 0.0),
                track_number: None,
                disc: None,
                year: None,
                artwork: m.image,
                spotify_album,
                spotify_artist,
                remote: true,
                station: None,
                format: None,
                detail: None,
            }
        })
        .collect();
    Json(SpotifyQueue {
        connect: true,
        items,
        error,
    })
    .into_response()
}

#[derive(Deserialize)]
struct SpectrumParams {
    /// position (seconds) a live stream's readings are wanted from
    #[serde(default)]
    from: f64,
}

/// The playing track's spectrum bars: a stream's live readings, else the library track's
/// stored curve (worked out now if it has none yet).
async fn get_spectrum(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    ApiQuery(q): ApiQuery<SpectrumParams>,
) -> Response {
    use base64::Engine;
    use model::{Spectrum, SpectrumKind};
    use rms_library::spectrum::FPS;
    let Some(p) = server.resolve_client(&id) else {
        return unknown_player(&id);
    };
    let playing = {
        let c = p.controller();
        let c = c.lock();
        c.playing_song().map(|s| (s.id, s.track.id, s.is_remote()))
    };
    let reply = |kind, key: String, start, curve: &[u8]| {
        Json(Spectrum {
            kind,
            key,
            fps: FPS,
            start,
            curve: base64::engine::general_purpose::STANDARD.encode(curve),
        })
        .into_response()
    };
    let none = || reply(SpectrumKind::None, String::new(), 0.0, &[]);
    let Some((song, track, remote)) = playing else {
        return none();
    };
    if let Some(w) = crate::stream::spectrum::window(song, q.from) {
        return reply(SpectrumKind::Live, format!("live:{song}"), w.start, &w.data);
    }
    let Some(track) = track.filter(|_| !remote) else {
        return none();
    };
    let library = server.library.clone();
    let ffmpeg = server.has_ffmpeg().then(crate::server::ffmpeg_path);
    let curve =
        crate::util::spawn_blocking(move || library.track_spectrum(track, ffmpeg.as_deref()))
            .await
            .ok()
            .flatten();
    match curve {
        Some(c) => reply(SpectrumKind::Track, format!("track:{track}"), 0.0, &c),
        None => none(),
    }
}

async fn post_command(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    b: Result<Json<Command>, JsonRejection>,
) -> Response {
    let Some(p) = server.resolve_client(&id) else {
        return unknown_player(&id);
    };
    let cmd = match body(b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    reply(commands::run(&server, &p, cmd).await)
}

#[derive(Debug, Deserialize, Default)]
struct AnnounceParams {
    volume: Option<i32>,
}

/// `POST /api/v1/players/{id}/announce`: the body is the clip, its `Content-Type` an audio
/// type.  Answers once the clip is queued; the player is put back when it ends.
async fn post_announce(
    State(server): State<Arc<Server>>,
    Path(id): Path<String>,
    ApiQuery(q): ApiQuery<AnnounceParams>,
    headers: axum::http::HeaderMap,
    clip: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    use crate::player::announce::{announce, AnnounceError};
    let Some(p) = server.resolve_client(&id) else {
        return unknown_player(&id);
    };
    let clip = match clip {
        Ok(b) => b,
        Err(e) => return error(e.status(), e.body_text()),
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match announce(&server, &p, &clip, content_type, q.volume) {
        Ok(()) => (StatusCode::ACCEPTED, Json(serde_json::json!({}))).into_response(),
        Err(AnnounceError::Invalid(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(AnnounceError::Busy) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "an announcement is already playing on this player's group",
        ),
        Err(AnnounceError::Failed(m)) => error(StatusCode::UNPROCESSABLE_ENTITY, m),
    }
}

fn reply<T: Serialize>(r: Result<T, CommandError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(CommandError::Invalid(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(CommandError::Failed(m)) => error(StatusCode::UNPROCESSABLE_ENTITY, m),
    }
}

/// `{}` for a done action.
fn done(r: Result<(), CommandError>) -> Response {
    reply(r.map(|()| serde_json::json!({})))
}

/// `Query`, answering a bad query string with `{"error": …}` like everything else.
struct ApiQuery<T>(T);

impl<T, S> axum::extract::FromRequestParts<S> for ApiQuery<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| ApiQuery(v))
            .map_err(|e| error(StatusCode::BAD_REQUEST, e.body_text()))
    }
}

/// `Path`, answering a bad path parameter with `{"error": …}`.
struct ApiPath<T>(T);

impl<T, S> axum::extract::FromRequestParts<S> for ApiPath<T>
where
    T: serde::de::DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(v)| ApiPath(v))
            .map_err(|e| error(StatusCode::NOT_FOUND, e.body_text()))
    }
}

#[allow(clippy::result_large_err)]
fn body<T>(b: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    b.map(|Json(v)| v)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e.body_text()))
}

fn library_reply<T: Serialize>(r: Result<T, LibraryError>, what: &str) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(LibraryError::NotFound) => error(StatusCode::NOT_FOUND, format!("no such {}", what)),
        Err(LibraryError::Invalid(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(LibraryError::Failed(m)) => error(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

// ---------------------------------------------------------------- library

async fn get_artists(
    State(s): State<Arc<Server>>,
    ApiQuery(p): ApiQuery<library::ArtistParams>,
) -> Response {
    library_reply(library::artists(&s, p).await, "artist")
}

async fn get_artist(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::artist_by_id(&s, id).await, "artist")
}

async fn get_albums(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<AlbumParams>) -> Response {
    library_reply(library::albums(&s, p).await, "album")
}

async fn get_album(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::album_by_id(&s, id).await, "album")
}

async fn get_album_release(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::album_release(&s, id).await, "album")
}

async fn put_album_release(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    b: Result<Json<ReleaseChoice>, JsonRejection>,
) -> Response {
    let choice = match body(b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    choose_release(&s, id, Some(choice.mbid.trim())).await
}

async fn delete_album_release(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    choose_release(&s, id, None).await
}

/// Match the album to a release (`Some`), or have it matched again (`None`); answers with
/// the album's release as `GET` does.
async fn choose_release(s: &Arc<Server>, id: i64, mbid: Option<&str>) -> Response {
    use crate::plugins::musicbrainz::ChooseError;
    match crate::plugins::musicbrainz::choose_release(s, id, mbid).await {
        Ok(_) => library_reply(library::album_release(s, id).await, "album"),
        Err(ChooseError::NotFound) => error(StatusCode::NOT_FOUND, "no such album"),
        Err(ChooseError::Invalid(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(ChooseError::Failed(m)) => error(StatusCode::BAD_GATEWAY, m),
    }
}

#[derive(Deserialize)]
struct TagsQuery {
    release: Option<String>,
}

async fn get_album_tags(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<TagsQuery>,
) -> Response {
    let s2 = s.clone();
    let r = crate::util::spawn_blocking(move || {
        crate::tagedit::proposal(&s2, id, q.release.as_deref().filter(|r| !r.is_empty()))
    })
    .await;
    match r {
        Ok(Ok((release, tracks))) => Json(library::album_tags(&s, release, tracks)).into_response(),
        Ok(Err(e)) => tag_error(e, "album"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn post_tags(
    State(s): State<Arc<Server>>,
    b: Result<Json<TagChanges>, JsonRejection>,
) -> Response {
    let changes = match body(b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let changes = changes
        .changes
        .into_iter()
        .map(|c| (c.track_id, c.tags))
        .collect();
    match crate::tagedit::apply(&s, changes).await {
        Ok(o) => Json(library::tag_edit_result(o)).into_response(),
        Err(e) => tag_error(e, "track"),
    }
}

#[derive(Deserialize)]
struct FilesQuery {
    template: Option<String>,
}

async fn get_album_files(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<FilesQuery>,
) -> Response {
    let template = q
        .template
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| crate::tagedit::template(&s));
    let s2 = s.clone();
    let t2 = template.clone();
    match crate::util::spawn_blocking(move || crate::tagedit::move_plan(&s2, id, &t2)).await {
        Ok(Ok(plan)) => Json(library::album_files(&s, template, plan)).into_response(),
        Ok(Err(e)) => tag_error(e, "album"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn post_album_files(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    b: Result<Json<MoveRequest>, JsonRejection>,
) -> Response {
    let req = match body(b) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let template = req
        .template
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| crate::tagedit::template(&s));
    match crate::tagedit::move_album(&s, id, &template).await {
        Ok(m) => {
            let r = library::tag_edit_result(m.outcome);
            Json(model::MoveResult {
                batch: r.batch,
                changed: r.changed,
                failed: r.failed,
                album_id: m.album_id,
            })
            .into_response()
        }
        Err(e) => tag_error(e, "album"),
    }
}

#[derive(Deserialize)]
struct CoverQuery {
    release: Option<String>,
}

/// The release to take cover art from: asked for, or the album's.
fn cover_release(s: &Server, id: i64, asked: Option<String>) -> Option<(String, Option<String>)> {
    match asked.filter(|r| !r.is_empty()) {
        Some(r) => {
            let rg = s
                .library
                .cached_release(&r, i64::MAX)
                .map(|x| x.release_group)
                .filter(|g| !g.is_empty());
            Some((r, rg))
        }
        None => crate::tagedit::album_release(s, id),
    }
}

async fn get_cover_art(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<CoverQuery>,
) -> Response {
    let (s2, s3) = (s.clone(), s.clone());
    let Ok(Some(((state, archive), release))) = crate::util::spawn_blocking(move || {
        s2.library.album_job(id)?;
        Some((
            (
                crate::tagedit::cover_state(&s2, id),
                s2.library.album_uses_release_art(id),
            ),
            cover_release(&s2, id, q.release),
        ))
    })
    .await
    else {
        return error(StatusCode::NOT_FOUND, "no such album");
    };
    let images = match &release {
        Some((r, rg)) => match crate::plugins::coverart::images(&s3, r, rg.as_deref()).await {
            Ok(i) => i,
            Err(e) => return error(StatusCode::BAD_GATEWAY, e),
        },
        None => Vec::new(),
    };
    Json(model::CoverArt {
        release_mbid: release.map(|r| r.0),
        images: images
            .into_iter()
            .map(|i| model::CoverImage {
                id: i.id,
                types: i.types,
                front: i.front,
                thumbnail: i.thumbnail,
            })
            .collect(),
        files: state.files as i64,
        embedded: state.embedded as i64,
        folder_image: state
            .folder
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())),
        archive,
        folder_shared: state.folder_shared,
        blocked: crate::tagedit::blocked(&s),
    })
    .into_response()
}

async fn post_cover_art(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<CoverQuery>,
    b: Result<Json<model::CoverChoice>, JsonRejection>,
) -> Response {
    let choice = match body(b) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Some(why) = crate::tagedit::blocked(&s) {
        return error(StatusCode::FORBIDDEN, why);
    }
    // only an image the archive lists for the album's release
    let Some((release, rg)) = cover_release(&s, id, q.release) else {
        return error(
            StatusCode::BAD_REQUEST,
            "the album isn't matched to a release",
        );
    };
    let images = match crate::plugins::coverart::images(&s, &release, rg.as_deref()).await {
        Ok(i) => i,
        Err(e) => return error(StatusCode::BAD_GATEWAY, e),
    };
    let Some(image) = images.into_iter().find(|i| i.id == choice.image) else {
        return error(
            StatusCode::BAD_REQUEST,
            format!("no image {} for the release", choice.image),
        );
    };
    let cover = match crate::plugins::coverart::download(&s, &image).await {
        Ok(c) => c,
        Err(e) => return error(StatusCode::BAD_GATEWAY, e),
    };
    match crate::tagedit::apply_cover(&s, id, cover, choice.folder, choice.embed).await {
        Ok(o) => Json(library::tag_edit_result(o)).into_response(),
        Err(e) => tag_error(e, "album"),
    }
}

async fn get_tag_edits(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<Page>) -> Response {
    Json(library::tag_edits(&s, p.limit.unwrap_or(50))).into_response()
}

async fn post_tag_undo(State(s): State<Arc<Server>>, ApiPath(batch): ApiPath<i64>) -> Response {
    match crate::tagedit::undo(&s, batch).await {
        Ok(o) => Json(library::tag_edit_result(o)).into_response(),
        Err(e) => tag_error(e, "change"),
    }
}

fn tag_error(e: crate::tagedit::Error, what: &str) -> Response {
    use crate::tagedit::Error;
    match e {
        Error::NotFound => error(StatusCode::NOT_FOUND, format!("no such {what}")),
        Error::Invalid(m) => error(StatusCode::BAD_REQUEST, m),
        Error::Forbidden(m) => error(StatusCode::FORBIDDEN, m),
        Error::Failed(m) => error(StatusCode::INTERNAL_SERVER_ERROR, m),
    }
}

async fn get_genres(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<ListParams>) -> Response {
    library_reply(library::genres(&s, p).await, "genre")
}

async fn get_years(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<ListParams>) -> Response {
    library_reply(library::years(&s, p).await, "year")
}

async fn get_tracks(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<TrackParams>) -> Response {
    library_reply(library::tracks(&s, p).await, "track")
}

#[derive(Debug, Deserialize, Default)]
struct UploadParams {
    name: Option<String>,
    folder: Option<String>,
}

/// `POST /api/v1/tracks?name=&folder=`: the body is a music file; answers the tracks it
/// became, with their detail.
async fn post_track(
    State(s): State<Arc<Server>>,
    ApiQuery(q): ApiQuery<UploadParams>,
    data: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    let data = match data {
        Ok(b) => b,
        Err(e) => return error(e.status(), e.body_text()),
    };
    let Some(name) = q.name.filter(|n| !n.trim().is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "name is missing (the file name, like \"01 Song.flac\")",
        );
    };
    match crate::upload::upload(&s, &name, q.folder.as_deref(), &data).await {
        Ok(ids) => match library::tracks_with_detail(&s, ids).await {
            Ok(tracks) => (StatusCode::CREATED, Json(tracks)).into_response(),
            Err(e) => library_reply::<()>(Err(e), "track"),
        },
        Err(e) => tag_error(e, "track"),
    }
}

async fn get_track(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::track_detail(&s, id).await, "track")
}

async fn get_track_analysis(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::track_analysis(&s, id).await, "track")
}

async fn get_analysis_progress(State(s): State<Arc<Server>>) -> Response {
    library_reply(library::analysis_progress(&s).await, "library")
}

async fn post_query(
    State(s): State<Arc<Server>>,
    b: Result<Json<model::TableRequest>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(r) => library_reply(library::table(&s, r).await, "query"),
        Err(r) => r,
    }
}

async fn get_query_fields() -> Response {
    Json(library::table_fields()).into_response()
}

async fn get_query_views(State(s): State<Arc<Server>>) -> Response {
    Json(library::table_views(&s)).into_response()
}

async fn put_query_views(
    State(s): State<Arc<Server>>,
    b: Result<Json<Vec<model::TableView>>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(v) => library_reply(library::set_table_views(&s, v), "view"),
        Err(r) => r,
    }
}

#[derive(Debug, Deserialize, Default)]
struct SimilarParams {
    limit: Option<usize>,
    #[serde(default)]
    detail: bool,
}

async fn get_track_similar(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(p): ApiQuery<SimilarParams>,
) -> Response {
    if let Err(r) = library::track_detail(&s, id).await {
        return library_reply::<()>(Err(r), "track");
    }
    let r = model::SimilarRequest {
        seeds: vec![model::Item::Track { id }],
        limit: p.limit,
        exclude: Vec::new(),
        detail: p.detail,
    };
    library_reply(library::similar(&s, r).await, "track")
}

async fn post_similar(
    State(s): State<Arc<Server>>,
    b: Result<Json<model::SimilarRequest>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(r) => library_reply(library::similar(&s, r).await, "track"),
        Err(r) => r,
    }
}

async fn get_playlists(
    State(s): State<Arc<Server>>,
    ApiQuery(p): ApiQuery<ListParams>,
) -> Response {
    library_reply(library::playlists(&s, p).await, "playlist")
}

async fn get_playlist(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::playlist_by_id(&s, id).await, "playlist")
}

async fn get_playlist_tracks(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(p): ApiQuery<ListParams>,
) -> Response {
    library_reply(library::playlist_tracks(&s, id, p).await, "playlist")
}

async fn post_playlist(
    State(s): State<Arc<Server>>,
    b: Result<Json<model::NewPlaylist>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(p) => library_reply(playlists::create(&s, p).await, "playlist"),
        Err(r) => r,
    }
}

async fn patch_playlist(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    b: Result<Json<model::PlaylistPatch>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(p) => library_reply(playlists::rename(&s, id, p).await, "playlist"),
        Err(r) => r,
    }
}

async fn delete_playlist(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(
        playlists::delete(&s, id)
            .await
            .map(|()| serde_json::json!({})),
        "playlist",
    )
}

async fn put_playlist_tracks(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    b: Result<Json<model::PlaylistOrder>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(p) => library_reply(
            playlists::reorder(&s, id, p)
                .await
                .map(|()| serde_json::json!({})),
            "playlist",
        ),
        Err(r) => r,
    }
}

async fn post_playlist_tracks(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
    b: Result<Json<model::PlaylistAdd>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(p) => library_reply(
            playlists::add(&s, id, p)
                .await
                .map(|n| serde_json::json!({ "count": n })),
            "playlist",
        ),
        Err(r) => r,
    }
}

async fn get_artist_top_tracks(
    State(s): State<Arc<Server>>,
    ApiPath(id): ApiPath<i64>,
) -> Response {
    library_reply(library::top_tracks(&s, id).await, "artist")
}

async fn get_artist_similar(State(s): State<Arc<Server>>, ApiPath(id): ApiPath<i64>) -> Response {
    library_reply(library::similar_artists(&s, id).await, "artist")
}

async fn get_folder(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<FolderParams>) -> Response {
    library_reply(library::folder(&s, p).await, "folder")
}

async fn get_search(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<ListParams>) -> Response {
    library_reply(library::search(&s, p).await, "result")
}

// ---------------------------------------------------------------- favorites, Spotify

async fn get_favorites(
    State(s): State<Arc<Server>>,
    ApiQuery(p): ApiQuery<MenuParams>,
) -> Response {
    reply(services::menu(&s, "favorites", p).await)
}

async fn get_radio(State(s): State<Arc<Server>>, ApiQuery(p): ApiQuery<MenuParams>) -> Response {
    reply(services::radio(&s, p).await)
}

async fn get_listenbrainz(
    State(s): State<Arc<Server>>,
    ApiQuery(p): ApiQuery<MenuParams>,
) -> Response {
    reply(services::listenbrainz(&s, p).await)
}

async fn post_listenbrainz_account(
    State(s): State<Arc<Server>>,
    b: Result<Json<model::NewListenBrainzAccount>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(a) => reply(services::add_listenbrainz_account(&s, a).await),
        Err(r) => r,
    }
}

async fn patch_listenbrainz_account(
    State(s): State<Arc<Server>>,
    Path(user): Path<String>,
    b: Result<Json<model::ListenBrainzAccountPatch>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(p) => done(services::patch_listenbrainz_account(&s, &user, p)),
        Err(r) => r,
    }
}

async fn delete_listenbrainz_account(
    State(s): State<Arc<Server>>,
    Path(user): Path<String>,
) -> Response {
    done(services::remove_listenbrainz_account(&s, &user))
}

async fn post_favorite(
    State(s): State<Arc<Server>>,
    b: Result<Json<NewFavorite>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(f) => done(services::add_favorite(&s, f).await),
        Err(r) => r,
    }
}

async fn delete_favorite(State(s): State<Arc<Server>>, Path(id): Path<String>) -> Response {
    done(services::delete_favorite(&s, &id).await)
}

fn no_spotify() -> Response {
    error(StatusCode::NOT_FOUND, "Spotify is not available")
}

async fn get_spotify(State(s): State<Arc<Server>>) -> Response {
    if !services::spotify_available() {
        return no_spotify();
    }
    reply(services::spotify_status(&s).await)
}

async fn patch_spotify(State(s): State<Arc<Server>>, Json(b): Json<SpotifyPatch>) -> Response {
    if !services::spotify_available() {
        return no_spotify();
    }
    reply(services::spotify_patch(&s, b).await)
}

async fn get_spotify_learned(State(s): State<Arc<Server>>) -> Response {
    reply(services::spotify_learned(&s).await)
}

async fn delete_spotify_learned(State(s): State<Arc<Server>>) -> Response {
    done(services::spotify_forget(&s).await)
}

async fn get_spotify_items(
    State(s): State<Arc<Server>>,
    ApiQuery(p): ApiQuery<MenuParams>,
) -> Response {
    if !services::spotify_available() {
        return no_spotify();
    }
    reply(services::menu(&s, "spotify", p).await)
}

async fn post_spotify_login(
    State(s): State<Arc<Server>>,
    b: Option<Json<SpotifyLogin>>,
) -> Response {
    if !services::spotify_available() {
        return no_spotify();
    }
    let login = b.map(|Json(l)| l).unwrap_or_default();
    reply(services::spotify_login(&s, login).await)
}

async fn post_spotify_logout(State(s): State<Arc<Server>>) -> Response {
    if !services::spotify_available() {
        return no_spotify();
    }
    done(services::spotify_logout(&s).await)
}

// ---------------------------------------------------------------- settings

async fn get_settings(State(s): State<Arc<Server>>) -> Response {
    Json(settings::server_settings(&s)).into_response()
}

async fn patch_settings(
    State(s): State<Arc<Server>>,
    b: Result<Json<ServerSettingsPatch>, JsonRejection>,
) -> Response {
    match body(b) {
        Ok(patch) => reply(settings::update_server_settings(&s, patch).await),
        Err(r) => r,
    }
}

async fn post_scan(State(s): State<Arc<Server>>, b: Option<Json<ScanRequest>>) -> Response {
    let mode = b.and_then(|Json(r)| r.mode).unwrap_or_default();
    done(settings::scan(&s, mode).await)
}

async fn get_player_settings(State(s): State<Arc<Server>>, Path(id): Path<String>) -> Response {
    match s.resolve_client(&id) {
        Some(p) => Json(settings::player_settings(&s, &p)).into_response(),
        None => unknown_player(&id),
    }
}

async fn patch_player_settings(
    State(s): State<Arc<Server>>,
    Path(id): Path<String>,
    b: Result<Json<PlayerSettingsPatch>, JsonRejection>,
) -> Response {
    let Some(p) = s.resolve_client(&id) else {
        return unknown_player(&id);
    };
    match body(b) {
        Ok(patch) => reply(settings::update_player_settings(&s, &p, patch).await),
        Err(r) => r,
    }
}
