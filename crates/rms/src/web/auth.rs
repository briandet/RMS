//! HTTP Basic authentication (`Slim::Web::HTTP::checkAuthorization`).
//!
//! With the `authorize` server pref on, every HTTP request except the players' own
//! `/stream` fetches (which carry their per-stream password, see `stream_handler`) must
//! present `username` / `password`.  Without it the CLI would be password-protected while
//! JSON-RPC and Cometd — the same command set — stayed open on port 9000.

use crate::server::Server;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// LMS stores the password as `sha1_base64(password)` (unpadded); accept that or a
/// plain-text match, so a password set with `pref password <plain>` works too.
pub fn check_password(given: &str, stored: &str) -> bool {
    if stored.is_empty() {
        return given.is_empty();
    }
    use base64::Engine;
    use sha1::Digest;
    let hashed = base64::engine::general_purpose::STANDARD_NO_PAD.encode(sha1::Sha1::digest(given));
    ct_eq(given, stored) || ct_eq(&hashed, stored)
}

/// Constant-time string comparison (length is not secret).
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// `user:pass` from an `Authorization: Basic …` header value.
fn basic_credentials(value: &str) -> Option<(String, String)> {
    use base64::Engine;
    let b64 = value.strip_prefix("Basic ")?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (u, p) = s.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

pub fn authorized(server: &Server, authorization: Option<&str>) -> bool {
    if !server.prefs.server.get_bool("authorize") {
        return true;
    }
    let Some((user, pass)) = authorization.and_then(basic_credentials) else {
        return false;
    };
    ct_eq(&user, &server.prefs.server.get_str("username"))
        && check_password(&pass, &server.prefs.server.get_str("password"))
}

/// Router middleware.  `/stream*` keeps its own per-stream check and the Subsonic API
/// (`/rest/*`) its own sign-in; CORS preflights pass so browsers can learn that
/// credentials are needed.
pub async fn require(State(server): State<Arc<Server>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if req.method() == Method::OPTIONS
        || path == "/stream"
        || path == "/stream.mp3"
        || path.starts_with("/rest/")
    {
        return next.run(req).await;
    }
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if authorized(&server, auth) {
        return next.run(req).await;
    }
    let mut resp = (StatusCode::UNAUTHORIZED, "authorization required").into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Lyrion Music Server\""),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        assert!(check_password("", ""));
        assert!(!check_password("x", ""));
        assert!(check_password("secret", "secret"));
        // sha1_base64("secret") as LMS stores it (no padding)
        assert!(check_password("secret", "5en6G6MezRroT3XKqkdPOmY/BfQ"));
        assert!(!check_password("Secret", "5en6G6MezRroT3XKqkdPOmY/BfQ"));
    }

    #[test]
    fn basic_header() {
        assert_eq!(
            basic_credentials("Basic dXNlcjpwYTpzcw=="),
            Some(("user".into(), "pa:ss".into()))
        );
        assert_eq!(basic_credentials("Bearer x"), None);
    }
}
