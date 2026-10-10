//! Artwork and file download endpoints (`Slim::Web::Graphics`, `downloadMusicFile`,
//! `Slim::Web::ImageProxy`).

use crate::server::Server;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use std::collections::HashMap;
use std::sync::Arc;

fn image_response(bytes: Vec<u8>, ct: &str) -> Response {
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_str(ct).unwrap_or(HeaderValue::from_static("image/jpeg")));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=86400"));
    r
}

/// Grey placeholder used when no artwork exists (LMS serves `html/images/cover.png`).
fn placeholder(spec: &str) -> Response {
    let (w, h) = parse_size(spec).unwrap_or((300, 300));
    let img = image::RgbImage::from_fn(w.max(1), h.max(1), |x, y| {
        let cx = x as i64 - w as i64 / 2;
        let cy = y as i64 - h as i64 / 2;
        let r2 = (w.min(h) as i64 / 3).pow(2);
        if cx * cx + cy * cy < r2 && cx * cx + cy * cy > r2 / 9 { image::Rgb([150, 150, 160]) } else { image::Rgb([90, 90, 100]) }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    if image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    image_response(out.into_inner(), "image/png")
}

fn parse_size(spec: &str) -> Option<(u32, u32)> {
    let re = regex::Regex::new(r"_(\d+)x(\d+)").ok()?;
    let c = re.captures(spec)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

/// `/music/<id>/<spec>` — `cover`, `cover.jpg`, `cover_300x300_o`, `thumb.png`, `download`, …
pub async fn music(State(server): State<Arc<Server>>, Path((id, spec)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Response {
    if spec.starts_with("download") {
        return download(&server, &id, &headers).await;
    }
    let mut spec = spec;
    if let Some(size) = q.get("size").and_then(|s| s.parse::<u32>().ok()) {
        spec = format!("cover_{}x{}_o", size, size);
    }
    let mut cover_id = id.clone();
    if id == "current" {
        let Some(player) = q.get("player").and_then(|p| server.resolve_client(p)) else {
            return placeholder(&spec);
        };
        let (coverid, art_url) = {
            let c = player.controller();
            let c = c.lock();
            match c.playing_song() {
                Some(s) => (s.track.coverid.clone().or(s.track.id.map(|i| i.to_string())), s.remote_meta.cover.clone().or(s.track.artwork_url.clone())),
                None => (None, None),
            }
        };
        if let Some(u) = art_url {
            if u.starts_with("http") {
                return proxy_image(&server, &u, &spec).await;
            }
            return Redirect::temporary(&format!("/{}", u.trim_start_matches('/'))).into_response();
        }
        match coverid {
            Some(c) => cover_id = c,
            None => return placeholder(&spec),
        }
    }
    let lib = server.library.clone();
    let spec2 = spec.clone();
    let res = tokio::task::spawn_blocking(move || lib.artwork(&cover_id, &spec2)).await.ok().flatten();
    match res {
        Some((bytes, ct)) => image_response(bytes, &ct),
        None => placeholder(&spec),
    }
}

async fn proxy_image(server: &Arc<Server>, url: &str, spec: &str) -> Response {
    let Ok(resp) = server.http.get(url).send().await else { return placeholder(spec) };
    if !resp.status().is_success() {
        return placeholder(spec);
    }
    let Ok(bytes) = resp.bytes().await else { return placeholder(spec) };
    let spec = spec.to_string();
    let data = bytes.to_vec();
    let resized = tokio::task::spawn_blocking(move || rms_library::Library::resize_image(&data, &spec)).await.ok().flatten();
    match resized {
        Some((b, ct)) => image_response(b, &ct),
        None => image_response(bytes.to_vec(), "image/jpeg"),
    }
}

/// `/imageproxy/<url-escaped>/<spec>`.
pub async fn imageproxy(State(server): State<Arc<Server>>, Path((url, spec)): Path<(String, String)>) -> Response {
    let url = percent_encoding::percent_decode_str(&url).decode_utf8_lossy().to_string();
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    proxy_image(&server, &url, &spec).await
}

/// `/html/images/<name>`: icons referenced by Jive menus.  We serve generated placeholders so
/// clients never break on missing images.
pub async fn html_image(Path(name): Path<String>) -> Response {
    if let Some(f) = super::ui::Assets::get(&format!("html/images/{}", name)) {
        let mime = mime_guess::from_path(&name).first_or_octet_stream();
        return image_response(f.data.to_vec(), mime.as_ref());
    }
    placeholder("_100x100")
}

/// `/music/<trackid>/download` with byte-range support.
async fn download(server: &Arc<Server>, id: &str, headers: &HeaderMap) -> Response {
    let Ok(track_id) = id.parse::<i64>() else { return StatusCode::NOT_FOUND.into_response() };
    let Some(t) = server.library.track(track_id) else { return StatusCode::NOT_FOUND.into_response() };
    let Some(path) = crate::util::path_from_file_url(&t.url) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(meta) = tokio::fs::metadata(&path).await else { return StatusCode::NOT_FOUND.into_response() };
    let len = meta.len();
    let (start, end) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| {
            let (s, e) = r.split_once('-')?;
            let s: u64 = s.parse().ok()?;
            let e: u64 = if e.is_empty() { len - 1 } else { e.parse().ok()? };
            (s <= e && e < len).then_some((s, e))
        })
        .map(|(s, e)| (s, e))
        .unwrap_or((0, len.saturating_sub(1)));
    let partial = headers.contains_key(header::RANGE);
    let spec = crate::stream::StreamSpec {
        source: crate::stream::SourceKind::File { path: path.clone(), offset: start, length: Some(end - start + 1) },
        format: t.content_type.clone(),
        song_id: 0,
    };
    let Ok(stream) = crate::stream::source::open(&spec, &server.http).await else { return StatusCode::NOT_FOUND.into_response() };
    let mut r = Response::new(Body::from_stream(stream));
    if partial {
        *r.status_mut() = StatusCode::PARTIAL_CONTENT;
        r.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, len)).unwrap());
    }
    r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(end - start + 1));
    r.headers_mut().insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let mime = crate::stream::mime_for_format(&t.content_type).map(str::to_string).unwrap_or_else(|| mime_guess::from_path(&path).first_or_octet_stream().to_string());
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap());
    let fname = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", fname.replace('"', ""))) {
        r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    r
}
