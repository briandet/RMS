//! Embedded web UI (`crates/rms/web`).

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

#[derive(rust_embed::RustEmbed)]
#[folder = "web/"]
#[exclude = "dev/*"]
#[exclude = ".gitkeep"]
pub struct Assets;

fn serve(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut r = Response::new(Body::from(f.data.into_owned()));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref()).unwrap(),
            );
            if path.ends_with(".html") {
                r.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            }
            r
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub async fn index() -> Response {
    if Assets::get("index.html").is_some() {
        serve("index.html")
    } else {
        axum::response::Html(
            "<!doctype html><title>RMS</title><h1>Rusty Music Server</h1><p>The web UI is not bundled in this build. \
             Use a Lyrion/LMS-compatible controller (Squeezer, Material, iPeng) against this server.</p>",
        )
        .into_response()
    }
}

pub async fn static_file(Path(path): Path<String>) -> Response {
    let p = path.strip_prefix("web/").unwrap_or(&path);
    if p.contains("..") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    serve(p)
}

/// Legacy `/html/...` paths used by some controllers (`/html/docs/...`, `/html/images/...`).
pub async fn html_compat(Path(path): Path<String>) -> Response {
    serve(&format!("html/{}", path))
}
