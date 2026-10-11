//! The [OpenSubsonic](https://opensubsonic.netlify.app/) API (`/rest/*`), for the many
//! Subsonic apps on phones and desktops (DSub, Symfonium, Substreamer, Feishin, play:Sub, …).
//! They browse the library, stream to themselves, and use the jukebox to play on a
//! Squeezebox.  See `docs/subsonic.md`.

mod browse;
mod media;
mod user;

use crate::server::Server;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Subsonic API version implemented.
pub const API_VERSION: &str = "1.16.1";

pub fn router() -> Router<Arc<Server>> {
    Router::new().route("/rest/{method}", any(handle))
}

/// The request's parameters: the query string and, for a form POST, the body.  Keys may
/// repeat (`id=1&id=2`).
pub struct Params(Vec<(String, String)>);

impl Params {
    fn parse(query: Option<&str>, body: &[u8]) -> Params {
        let mut v: Vec<(String, String)> = Vec::new();
        if let Some(q) = query {
            v.extend(url::form_urlencoded::parse(q.as_bytes()).into_owned());
        }
        v.extend(url::form_urlencoded::parse(body).into_owned());
        Params(v)
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, k: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn require(&self, k: &str) -> Result<&str> {
        self.get(k)
            .ok_or_else(|| Error::new(10, format!("Required parameter is missing: {k}")))
    }

    pub fn int(&self, k: &str) -> Result<Option<i64>> {
        match self.get(k) {
            None | Some("") => Ok(None),
            Some(v) => v
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| Error::new(0, format!("Invalid value for {k}: {v}"))),
        }
    }

    /// A count or offset, at most `max`.
    pub fn size(&self, k: &str, default: usize, max: usize) -> Result<usize> {
        Ok(self
            .int(k)?
            .map(|n| n.max(0) as usize)
            .unwrap_or(default)
            .min(max))
    }

    pub fn flag(&self, k: &str, default: bool) -> bool {
        match self.get(k) {
            Some(v) => matches!(v, "true" | "1"),
            None => default,
        }
    }
}

/// A Subsonic error: `code` from the spec, and a message.
#[derive(Debug)]
pub struct Error {
    pub code: u32,
    pub message: String,
}

impl Error {
    pub fn new(code: u32, message: impl Into<String>) -> Error {
        Error {
            code,
            message: message.into(),
        }
    }

    pub fn not_found(what: &str) -> Error {
        Error::new(70, format!("{what} not found"))
    }
}

/// What a method returns: fields of `subsonic-response`, or a response of its own (media).
pub enum Reply {
    Fields(Map<String, Value>),
    Raw(Response),
}

impl Reply {
    pub fn empty() -> Reply {
        Reply::Fields(Map::new())
    }

    pub fn one(key: &str, v: Value) -> Reply {
        let mut m = Map::new();
        m.insert(key.into(), v);
        Reply::Fields(m)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

async fn handle(
    State(server): State<Arc<Server>>,
    Path(method): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    let p = Params::parse(query.as_deref(), if form { &body } else { &[] });
    let method = method.strip_suffix(".view").unwrap_or(&method).to_string();
    let format = Format::of(&p);
    if let Err(e) = authenticate(&server, &p, &headers) {
        return format.error(e);
    }
    match call(&server, &method, &p, &headers).await {
        Ok(Reply::Fields(fields)) => format.ok(fields),
        Ok(Reply::Raw(r)) => r,
        Err(e) => format.error(e),
    }
}

async fn call(
    server: &Arc<Server>,
    method: &str,
    p: &Params,
    headers: &HeaderMap,
) -> Result<Reply> {
    use browse as b;
    match method {
        "ping" => Ok(Reply::empty()),
        "getLicense" => Ok(Reply::one(
            "license",
            json!({ "valid": true, "email": "", "licenseExpires": "2099-12-31T00:00:00Z" }),
        )),
        "getOpenSubsonicExtensions" => Ok(Reply::one(
            "openSubsonicExtensions",
            json!([
                { "name": "formPost", "versions": [1] },
                { "name": "songLyrics", "versions": [1] },
                { "name": "transcodeOffset", "versions": [1] },
            ]),
        )),
        "getMusicFolders" => b::music_folders(server),
        "getIndexes" => b::indexes(server, p).await,
        "getMusicDirectory" => b::music_directory(server, p).await,
        "getArtists" => b::artists(server),
        "getArtist" => b::artist(server, p),
        "getAlbum" => b::album(server, p),
        "getSong" => b::get_song(server, p),
        "getGenres" => b::genres(server),
        "getAlbumList" => b::album_list(server, p, false),
        "getAlbumList2" => b::album_list(server, p, true),
        "getRandomSongs" => b::random_songs(server, p),
        "getSongsByGenre" => b::songs_by_genre(server, p),
        "search2" => b::search(server, p, false),
        "search3" => b::search(server, p, true),
        "getStarred" => b::starred(server, false),
        "getStarred2" => b::starred(server, true),
        "getArtistInfo" | "getArtistInfo2" => {
            let key = if method == "getArtistInfo" {
                "artistInfo"
            } else {
                "artistInfo2"
            };
            Ok(Reply::one(key, json!({})))
        }
        "getAlbumInfo" | "getAlbumInfo2" => Ok(Reply::one("albumInfo", json!({}))),
        "getSimilarSongs" => b::similar_songs(server, p, "similarSongs"),
        "getSimilarSongs2" => b::similar_songs(server, p, "similarSongs2"),
        "getTopSongs" => Ok(Reply::one("topSongs", json!({}))),
        "getLyrics" => b::lyrics(server, p),
        "getLyricsBySongId" => b::lyrics_by_song_id(server, p),
        "getNowPlaying" => b::now_playing(server),
        "getInternetRadioStations" => b::radio_stations(server),
        "stream" => media::stream(server, p, headers).await,
        "download" => media::download(server, p, headers).await,
        "getCoverArt" => media::cover_art(server, p).await,
        "getPlaylists" => user::playlists(server),
        "getPlaylist" => user::playlist(server, p),
        "createPlaylist" => user::create_playlist(server, p),
        "updatePlaylist" => user::update_playlist(server, p),
        "deletePlaylist" => user::delete_playlist(server, p),
        "star" => user::star(server, p, true).await,
        "unstar" => user::star(server, p, false).await,
        "setRating" => user::set_rating(server, p),
        "scrobble" => user::scrobble(server, p),
        "getScanStatus" => user::scan_status(server),
        "startScan" => user::start_scan(server).await,
        "getUser" => user::user(server, p),
        "getUsers" => user::users(server),
        "jukeboxControl" => user::jukebox(server, p).await,
        "getBookmarks" => Ok(Reply::one("bookmarks", json!({}))),
        "getPlayQueue" => Ok(Reply::empty()),
        "getPodcasts" => Ok(Reply::one("podcasts", json!({}))),
        "getNewestPodcasts" => Ok(Reply::one("newestPodcasts", json!({}))),
        "getShares" => Ok(Reply::one("shares", json!({}))),
        "getChatMessages" => Ok(Reply::one("chatMessages", json!({}))),
        "getVideos" => Ok(Reply::one("videos", json!({}))),
        _ => Err(Error::new(
            0,
            format!("{method} is not supported by this server"),
        )),
    }
}

// ---------------------------------------------------------------- authentication

/// The user name clients sign in with when the server has no password set.
const ANY_USER: &str = "rms";

/// With the `authorize` pref off, anyone may use the API (like the web UI and JSON-RPC).
/// With it on: `u` and `p` (plain or `enc:<hex>`), or `u`, `t` = md5(password + `s`) and `s`
/// (only possible while the password is stored as plain text), or HTTP Basic.
fn authenticate(server: &Server, p: &Params, headers: &HeaderMap) -> Result<()> {
    if !server.prefs.server.get_bool("authorize") {
        return Ok(());
    }
    if p.get("apiKey").is_some() {
        return Err(Error::new(
            42,
            "API keys are not supported; use a user name and password",
        ));
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if p.get("u").is_none() && auth.is_some() {
        return if crate::web::auth::authorized(server, auth) {
            Ok(())
        } else {
            Err(Error::new(40, "Wrong username or password"))
        };
    }
    let user = p.require("u")?;
    let stored = server.prefs.server.get_str("password");
    let user_ok = user == server.prefs.server.get_str("username");
    let pass_ok = if let Some(pass) = p.get("p") {
        let pass = match pass.strip_prefix("enc:") {
            Some(hex) => decode_hex(hex).unwrap_or_default(),
            None => pass.to_string(),
        };
        crate::web::auth::check_password(&pass, &stored)
    } else if let (Some(t), Some(s)) = (p.get("t"), p.get("s")) {
        let want = format!("{:x}", md5::compute(format!("{stored}{s}")));
        if !want.eq_ignore_ascii_case(t) {
            if looks_hashed(&stored) {
                return Err(Error::new(
                    41,
                    "Token authentication is not possible with this server's stored password; \
                     send the password itself",
                ));
            }
            false
        } else {
            true
        }
    } else {
        return Err(Error::new(
            10,
            "Required parameter is missing: p or t and s",
        ));
    };
    if user_ok && pass_ok {
        Ok(())
    } else {
        Err(Error::new(40, "Wrong username or password"))
    }
}

/// `sha1_base64` as LMS stores passwords.
fn looks_hashed(stored: &str) -> bool {
    stored.len() == 27
        && stored
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

fn decode_hex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

/// The user name to show: the configured one, or a fixed one without authorization.
pub fn user_name(server: &Server, p: &Params) -> String {
    let configured = server.prefs.server.get_str("username");
    if server.prefs.server.get_bool("authorize") && !configured.is_empty() {
        configured
    } else {
        p.get("u")
            .filter(|u| !u.is_empty())
            .unwrap_or(ANY_USER)
            .to_string()
    }
}

// ---------------------------------------------------------------- response formats

enum Format {
    Xml,
    Json,
    Jsonp(String),
}

impl Format {
    fn of(p: &Params) -> Format {
        match p.get("f") {
            Some("json") => Format::Json,
            Some("jsonp") => Format::Jsonp(p.get("callback").unwrap_or("callback").to_string()),
            _ => Format::Xml,
        }
    }

    fn envelope(status: &str, fields: Map<String, Value>) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("status".into(), json!(status));
        m.insert("version".into(), json!(API_VERSION));
        m.insert("type".into(), json!("rms"));
        m.insert("serverVersion".into(), json!(env!("CARGO_PKG_VERSION")));
        m.insert("openSubsonic".into(), json!(true));
        m.extend(fields);
        m
    }

    fn ok(&self, fields: Map<String, Value>) -> Response {
        self.render(Self::envelope("ok", fields))
    }

    fn error(&self, e: Error) -> Response {
        let mut fields = Map::new();
        fields.insert(
            "error".into(),
            json!({ "code": e.code, "message": e.message }),
        );
        self.render(Self::envelope("failed", fields))
    }

    fn render(&self, m: Map<String, Value>) -> Response {
        let (body, ctype) = match self {
            Format::Json => (
                json!({ "subsonic-response": m }).to_string(),
                "application/json",
            ),
            Format::Jsonp(cb) => (
                format!(
                    "{}({});",
                    sanitize_callback(cb),
                    json!({ "subsonic-response": m })
                ),
                "application/javascript",
            ),
            Format::Xml => (to_xml(&m), "text/xml; charset=utf-8"),
        };
        let mut r = (StatusCode::OK, body).into_response();
        r.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
        r
    }
}

fn sanitize_callback(cb: &str) -> String {
    cb.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.' || *c == '$')
        .collect()
}

/// The XML form of a response: scalars become attributes, objects child elements, arrays
/// repeated child elements, and a `value` field the element's text.
fn to_xml(m: &Map<String, Value>) -> String {
    let mut out = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    let mut attrs = m.clone();
    attrs.insert("xmlns".into(), json!("http://subsonic.org/restapi"));
    element(&mut out, "subsonic-response", &attrs);
    out
}

fn element(out: &mut String, name: &str, m: &Map<String, Value>) {
    out.push('<');
    out.push_str(name);
    let mut text = None;
    for (k, v) in m {
        let scalar = match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        };
        match scalar {
            Some(s) if k == "value" => text = Some(s),
            Some(s) => {
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&escape(&s));
                out.push('"');
            }
            None => {}
        }
    }
    let children: Vec<(&String, &Value)> = m
        .iter()
        .filter(|(_, v)| v.is_object() || v.is_array())
        .collect();
    if children.is_empty() && text.is_none() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    if let Some(t) = text {
        out.push_str(&escape(&t));
    }
    for (k, v) in children {
        match v {
            Value::Object(o) => element(out, k, o),
            Value::Array(items) => {
                for it in items {
                    match it {
                        Value::Object(o) => element(out, k, o),
                        other => {
                            let mut o = Map::new();
                            o.insert("value".into(), other.clone());
                            element(out, k, &o);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_shape() {
        let m = Format::envelope(
            "ok",
            json!({ "artists": { "ignoredArticles": "The", "index": [
                { "name": "A", "artist": [ { "id": "ar-1", "name": "A & B" } ] }
            ] }, "lyrics": { "artist": "x", "value": "la <la>" } })
            .as_object()
            .unwrap()
            .clone(),
        );
        let x = to_xml(&m);
        assert!(x.contains(r#"<artists ignoredArticles="The"><index name="A"><artist id="ar-1" name="A &amp; B"/></index></artists>"#), "{x}");
        assert!(
            x.contains(r#"<lyrics artist="x">la &lt;la&gt;</lyrics>"#),
            "{x}"
        );
        assert!(x.contains(r#"xmlns="http://subsonic.org/restapi""#));
    }

    #[test]
    fn params_repeat_and_hex_passwords() {
        let p = Params::parse(Some("id=1&id=2&u=me"), b"songIdToAdd=3");
        assert_eq!(p.all("id"), vec!["1", "2"]);
        assert_eq!(p.get("songIdToAdd"), Some("3"));
        assert_eq!(decode_hex("736563726574").as_deref(), Some("secret"));
    }
}
