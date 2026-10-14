//! Spotify (the role of Lyrion's third-party "Spotty" plugin), built on librespot.
//!
//! * Sign-in: OAuth authorization code with PKCE against Spotify's own desktop client id
//!   (as librespot does).  Spotify redirects to `http://127.0.0.1:5588/login`; when the
//!   browser runs on this machine we catch that request, otherwise the user pastes the
//!   redirected address into the web UI.  The access token logs a librespot session in once;
//!   librespot's reusable credentials are kept for later sessions and the refresh token for
//!   Web API access.
//! * Browsing ([`menu`]): the Spotify Web API, presented through the XMLBrowser menus.
//! * Playback ([`stream`]): a librespot player per stream decodes the track to 16-bit PCM,
//!   which goes to the player like any other stream (`spotify://track:<id>` URLs).

pub mod connect;
pub mod learn;
pub mod menu;
pub mod session_api;
pub mod stream;

use base64::Engine;
use dashmap::DashMap;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::{Session, SessionConfig};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Spotify's desktop client id, which librespot uses for its OAuth flow.  Shared by every
/// librespot-based app, so Spotify rate-limits its Web API traffic hard (HTTP 429 even on
/// the first request); the `spotifyClientId` pref replaces it with the user's own app.
const DEFAULT_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
/// Rate-limit waits longer than this fail the request instead of blocking the caller.
const MAX_RATE_LIMIT_WAIT: u64 = 3;

/// The client id to sign in with: the user's own (`spotifyClientId` pref) or the default.
pub fn client_id_or_default(pref: &str) -> String {
    let id = pref.trim();
    if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        id.to_ascii_lowercase()
    } else {
        DEFAULT_CLIENT_ID.to_string()
    }
}

fn default_client_id() -> String {
    DEFAULT_CLIENT_ID.to_string()
}
const REDIRECT_PORT: u16 = 5588;
const API: &str = "https://api.spotify.com/v1";

/// Scopes requested at sign-in (librespot's set, without the write scopes).
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-follow-read",
    "user-top-read",
    "user-read-recently-played",
    "user-read-playback-position",
];

pub fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}/login")
}

/// Metadata of a track, remembered from browsing.
#[derive(Debug, Clone, Default)]
pub struct TrackMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub secs: f64,
    pub image: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct OAuthState {
    refresh_token: String,
    /// The client id the refresh token belongs to (files from before it was stored used
    /// the default one).
    #[serde(default = "default_client_id")]
    client_id: String,
    #[serde(skip)]
    access_token: Option<(String, Instant)>,
}

struct PendingLogin {
    verifier: String,
    state: String,
    client_id: String,
}

pub struct Spotify {
    dir: PathBuf,
    http: reqwest::Client,
    session: tokio::sync::Mutex<Option<Session>>,
    oauth: Mutex<Option<OAuthState>>,
    pending: Mutex<Option<PendingLogin>>,
    /// Track metadata by `spotify://track:<id>` URL.
    pub meta: DashMap<String, TrackMeta>,
    /// Web API list responses (`ref` → items), briefly.
    lists: Mutex<std::collections::HashMap<String, (Instant, Vec<Value>)>>,
    user: Mutex<Option<String>>,
    /// The Web API failed (rate limit, no usable token): use the session until then.
    web_api_down_until: Mutex<Option<Instant>>,
    /// The server, for what plays (learning tracks, [`learn`]).
    server: std::sync::OnceLock<std::sync::Weak<crate::server::Server>>,
}

static INSTANCE: OnceLock<Arc<Spotify>> = OnceLock::new();

/// The Spotify service (set up at server start).
pub fn get() -> Option<&'static Arc<Spotify>> {
    INSTANCE.get()
}

pub fn init(data_dir: &std::path::Path, http: reqwest::Client) -> Arc<Spotify> {
    INSTANCE
        .get_or_init(|| {
            let dir = data_dir.join("spotify");
            let oauth = std::fs::read_to_string(dir.join("oauth.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
            Arc::new(Spotify {
                dir,
                http,
                session: tokio::sync::Mutex::new(None),
                oauth: Mutex::new(oauth),
                pending: Mutex::new(None),
                meta: DashMap::new(),
                lists: Mutex::new(Default::default()),
                user: Mutex::new(None),
                web_api_down_until: Mutex::new(None),
                server: std::sync::OnceLock::new(),
            })
        })
        .clone()
}

/// `spotify://track:ID` / `spotify:track:ID` / open.spotify.com links → (`kind`, `id`).
pub fn parse_url(url: &str) -> Option<(String, String)> {
    let rest = url
        .strip_prefix("spotify://")
        .or_else(|| url.strip_prefix("spotify:"))
        .or_else(|| url.strip_prefix("https://open.spotify.com/"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    let (kind, id) = rest.split_once([':', '/'])?;
    let id = id.trim_matches('/');
    if kind.is_empty() || (id.is_empty() && kind != "collection") {
        return None;
    }
    Some((kind.to_string(), id.to_string()))
}

pub fn is_spotify(url: &str) -> bool {
    url.starts_with("spotify:")
}

/// The canonical playlist URL of a Spotify item.
pub fn url_for(kind: &str, id: &str) -> String {
    format!("spotify://{kind}:{id}")
}

fn random_string(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    (0..len)
        .map(|_| CHARS[rand::random_range(0..CHARS.len())] as char)
        .collect()
}

/// PKCE S256 challenge of a verifier.
fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Code from a pasted redirect URL (or a bare code).
fn code_from(input: &str) -> Option<String> {
    let input = input.trim();
    if input.contains("code=") {
        let query = input.split_once('?').map(|(_, q)| q).unwrap_or(input);
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
    } else if !input.is_empty() && !input.contains([' ', '?', '/']) {
        Some(input.to_string())
    } else {
        None
    }
}

/// `TrackMeta` from a Web API track object.
pub fn meta_from_track(t: &Value) -> TrackMeta {
    let artists: Vec<&str> = t["artists"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    TrackMeta {
        title: t["name"].as_str().unwrap_or("").to_string(),
        artist: artists.join(", "),
        album: t["album"]["name"].as_str().unwrap_or("").to_string(),
        secs: t["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0,
        image: best_image(&t["album"]["images"]),
    }
}

/// The largest image of a Web API `images` array.
pub fn best_image(images: &Value) -> Option<String> {
    images
        .as_array()?
        .iter()
        .max_by_key(|i| i["width"].as_u64().unwrap_or(0))
        .and_then(|i| i["url"].as_str())
        .map(str::to_string)
}

impl Spotify {
    fn credentials_cache(&self) -> Option<Cache> {
        Cache::new(Some(&self.dir), None, None, None).ok()
    }

    /// Stable device id for this server.
    fn device_id(&self) -> String {
        let path = self.dir.join("device_id");
        if let Ok(id) = std::fs::read_to_string(&path) {
            if !id.trim().is_empty() {
                return id.trim().to_string();
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(&path, &id);
        id
    }

    fn save_oauth(&self) {
        let g = self.oauth.lock();
        if let Some(o) = g.as_ref() {
            let _ = std::fs::create_dir_all(&self.dir);
            let _ = std::fs::write(
                self.dir.join("oauth.json"),
                serde_json::to_string(o).unwrap_or_default(),
            );
        }
    }

    /// Signed in: credentials for a session exist.
    /// Tell the service which server it plays for (at start).
    pub fn attach(&self, server: &Arc<crate::server::Server>) {
        let _ = self.server.set(Arc::downgrade(server));
    }

    /// The server it plays for.
    pub fn server(&self) -> std::sync::Weak<crate::server::Server> {
        self.server.get().cloned().unwrap_or_default()
    }

    pub fn signed_in(&self) -> bool {
        self.dir.join("credentials.json").exists() || self.oauth.lock().is_some()
    }

    pub fn username(&self) -> Option<String> {
        self.user.lock().clone()
    }

    // ---- sign-in ------------------------------------------------------------------------------

    /// Start signing in: the Spotify authorization URL to open.  Also listens on the
    /// redirect port to finish by itself when the browser runs on this machine.
    pub fn start_login(self: &Arc<Self>, client_id: &str) -> String {
        let verifier = random_string(96);
        let state = random_string(16);
        let mut url = url::Url::parse("https://accounts.spotify.com/authorize").expect("url");
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri())
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("scope", &SCOPES.join(" "))
            .append_pair("state", &state);
        *self.pending.lock() = Some(PendingLogin {
            verifier,
            state,
            client_id: client_id.to_string(),
        });
        let me = self.clone();
        tokio::spawn(async move { me.listen_for_redirect().await });
        url.to_string()
    }

    /// Catch the redirect on `127.0.0.1:5588` (only reachable from this machine).
    async fn listen_for_redirect(self: Arc<Self>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT)).await else {
            return;
        };
        let accept = tokio::time::timeout(Duration::from_secs(600), listener.accept()).await;
        let Ok(Ok((mut sock, _))) = accept else {
            return;
        };
        let mut line = String::new();
        let mut reader = BufReader::new(&mut sock);
        if reader.read_line(&mut line).await.is_err() {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let result = self
            .finish_login(&format!("http://127.0.0.1{target}"))
            .await;
        let body = match &result {
            Ok(user) => format!("Signed in to Spotify as {user}. You can close this window."),
            Err(e) => format!("Spotify sign-in failed: {e}"),
        };
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await;
    }

    /// Finish signing in with the redirected URL (or its `code`).
    pub async fn finish_login(&self, input: &str) -> Result<String, String> {
        let code = code_from(input).ok_or("no authorization code in that address")?;
        if let Some(state) = input
            .split_once('?')
            .and_then(|(_, q)| {
                url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "state")
            })
            .map(|(_, v)| v.into_owned())
        {
            if self
                .pending
                .lock()
                .as_ref()
                .is_some_and(|p| p.state != state)
            {
                return Err("this sign-in link is outdated; start again".into());
            }
        }
        let (verifier, client_id) = self
            .pending
            .lock()
            .as_ref()
            .map(|p| (p.verifier.clone(), p.client_id.clone()))
            .ok_or("no sign-in in progress; start again")?;
        let resp: Value = self
            .http
            .post("https://accounts.spotify.com/api/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri().as_str()),
                ("client_id", client_id.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let access = resp["access_token"]
            .as_str()
            .ok_or_else(|| {
                resp["error_description"]
                    .as_str()
                    .unwrap_or("sign-in refused")
                    .to_string()
            })?
            .to_string();
        let expires = resp["expires_in"].as_u64().unwrap_or(3600);
        *self.pending.lock() = None;
        *self.oauth.lock() = Some(OAuthState {
            refresh_token: resp["refresh_token"].as_str().unwrap_or("").to_string(),
            client_id: client_id.clone(),
            access_token: Some((
                access.clone(),
                Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
            )),
        });
        self.save_oauth();
        self.lists.lock().clear();
        // Log a playback session in with the new token; librespot stores reusable
        // credentials.  A token of the user's own app may not be accepted for playback:
        // then keep the session credentials of an earlier sign-in, if there are any.
        let cached = self.dir.join("credentials.json");
        let backup = self.dir.join("credentials.json.prev");
        let had_cached = std::fs::rename(&cached, &backup).is_ok();
        *self.session.lock().await = None;
        match self.connect(Credentials::with_access_token(access)).await {
            Ok(session) => {
                let _ = std::fs::remove_file(&backup);
                let user = session.username();
                info!("Spotify: signed in as {user} (client id {client_id})");
                Ok(user)
            }
            Err(e) if had_cached => {
                let _ = std::fs::rename(&backup, &cached);
                warn!("{e} with client id {client_id}; keeping the earlier playback sign-in");
                let session = self.session().await?;
                Ok(session.username())
            }
            Err(e) => Err(if client_id == DEFAULT_CLIENT_ID {
                e
            } else {
                format!(
                    "{e}. Spotify accepted your app for browsing but not for playback: \
                     clear the client id, sign in once, then set it again and sign in."
                )
            }),
        }
    }

    /// Forget the account.
    pub async fn logout(&self) {
        if let Some(s) = self.session.lock().await.take() {
            s.shutdown();
        }
        *self.oauth.lock() = None;
        *self.user.lock() = None;
        self.lists.lock().clear();
        let _ = std::fs::remove_file(self.dir.join("credentials.json"));
        let _ = std::fs::remove_file(self.dir.join("oauth.json"));
        // the Connect devices' own credentials (`connect::add_device`)
        let _ = std::fs::remove_dir_all(self.dir.join("connect"));
    }

    // ---- session ------------------------------------------------------------------------------

    async fn connect(&self, credentials: Credentials) -> Result<Session, String> {
        let config = SessionConfig {
            device_id: self.device_id(),
            ..Default::default()
        };
        let session = Session::new(config, self.credentials_cache());
        session
            .connect(credentials, true)
            .await
            .map_err(|e| format!("Spotify login failed: {e}"))?;
        *self.user.lock() = Some(session.username());
        *self.session.lock().await = Some(session.clone());
        Ok(session)
    }

    /// The librespot session, (re)connecting as needed.
    pub async fn session(&self) -> Result<Session, String> {
        {
            let g = self.session.lock().await;
            if let Some(s) = g.as_ref().filter(|s| !s.is_invalid()) {
                return Ok(s.clone());
            }
        }
        if let Some(creds) = self.credentials_cache().and_then(|c| c.credentials()) {
            match self.connect(creds).await {
                Ok(s) => return Ok(s),
                Err(e) => warn!("{e}; trying the refresh token"),
            }
        }
        let token = self.oauth_token().await?;
        self.connect(Credentials::with_access_token(token)).await
    }

    /// A Web API access token from the stored refresh token.
    async fn oauth_token(&self) -> Result<String, String> {
        let (refresh, client_id) = {
            let g = self.oauth.lock();
            let o = g.as_ref().ok_or("not signed in to Spotify")?;
            if let Some((t, until)) = &o.access_token {
                if *until > Instant::now() {
                    return Ok(t.clone());
                }
            }
            (o.refresh_token.clone(), o.client_id.clone())
        };
        if refresh.is_empty() {
            return Err("no Spotify refresh token".into());
        }
        let resp: Value = self
            .http
            .post("https://accounts.spotify.com/api/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let access = resp["access_token"]
            .as_str()
            .ok_or("Spotify refused the refresh token; sign in again")?
            .to_string();
        let expires = resp["expires_in"].as_u64().unwrap_or(3600);
        if let Some(o) = self.oauth.lock().as_mut() {
            o.access_token = Some((
                access.clone(),
                Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
            ));
            if let Some(r) = resp["refresh_token"].as_str() {
                o.refresh_token = r.to_string();
            }
        }
        self.save_oauth();
        Ok(access)
    }

    /// Token for the Web API: the OAuth token, else one from the session (keymaster).
    async fn api_token(&self) -> Result<String, String> {
        if self.oauth.lock().is_some() {
            if let Ok(t) = self.oauth_token().await {
                return Ok(t);
            }
        }
        let session = self.session().await?;
        session
            .token_provider()
            .get_token(&SCOPES[1..].join(","))
            .await
            .map(|t| t.access_token)
            .map_err(|e| e.to_string())
    }

    // ---- Web API ------------------------------------------------------------------------------

    fn rate_limited_message(&self, wait: u64) -> String {
        let own = self
            .oauth
            .lock()
            .as_ref()
            .is_some_and(|o| o.client_id != DEFAULT_CLIENT_ID);
        if own {
            format!("Spotify is rate-limiting your app; try again in {wait}s")
        } else {
            format!(
                "Spotify is rate-limiting the shared client id (retry in {wait}s): \
                 set your own Spotify app's client id in Settings, then sign in again"
            )
        }
    }

    /// GET a Web API path (`/me/playlists?limit=50`): from the Web API, or through the
    /// session ([`session_api`]) when the Web API is unavailable.
    pub async fn api(&self, path: &str) -> Result<Value, String> {
        let down = self
            .web_api_down_until
            .lock()
            .is_some_and(|t| t > Instant::now());
        let web_err = if down {
            "Spotify Web API unavailable".to_string()
        } else {
            match self.web_api(path).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let wait = if e.contains("rate-limiting") { 60 } else { 600 };
                    *self.web_api_down_until.lock() =
                        Some(Instant::now() + Duration::from_secs(wait));
                    e
                }
            }
        };
        let session = self.session().await?;
        session_api::session_get(&session, path)
            .await
            .map_err(|e| format!("{e} ({web_err})"))
    }

    async fn web_api(&self, path: &str) -> Result<Value, String> {
        let url = if path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{API}{path}")
        };
        for _attempt in 0..3 {
            let token = self.api_token().await?;
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            if status != 200 {
                warn!(
                    "Spotify Web API {} -> {} (retry-after {:?})",
                    path,
                    status,
                    resp.headers().get("retry-after")
                );
            }
            match status {
                200 => return resp.json().await.map_err(|e| e.to_string()),
                401 => {
                    // token expired early
                    if let Some(o) = self.oauth.lock().as_mut() {
                        o.access_token = None;
                    }
                }
                429 => {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(1);
                    if wait > MAX_RATE_LIMIT_WAIT {
                        // don't hold the caller (a menu, a track about to play) for it
                        return Err(self.rate_limited_message(wait));
                    }
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                s => {
                    let body: Value = resp.json().await.unwrap_or(Value::Null);
                    let msg = body["error"]["message"].as_str().unwrap_or("").to_string();
                    return Err(format!("Spotify Web API {s}: {msg}"));
                }
            }
        }
        Err("Spotify Web API unavailable".into())
    }

    /// All items of a paged Web API list (following `next`), up to `max`.
    pub async fn api_all(&self, path: &str, max: usize) -> Result<Vec<Value>, String> {
        let key = format!("{path}#{max}");
        if let Some((at, items)) = self.lists.lock().get(&key) {
            if at.elapsed() < Duration::from_secs(300) {
                return Ok(items.clone());
            }
        }
        let mut items = Vec::new();
        let mut next = Some(path.to_string());
        while let Some(p) = next.take() {
            let page = self.api(&p).await?;
            // `/me/following` wraps its page in `artists`
            let page = if page.get("artists").is_some() && page.get("items").is_none() {
                page["artists"].clone()
            } else {
                page
            };
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            if items.len() < max {
                next = page["next"].as_str().map(str::to_string);
            }
        }
        items.truncate(max);
        self.lists
            .lock()
            .insert(key, (Instant::now(), items.clone()));
        Ok(items)
    }

    /// Remember the metadata of tracks seen while browsing.
    pub fn remember(&self, track: &Value) {
        if let Some(id) = track["id"].as_str() {
            self.meta
                .insert(url_for("track", id), meta_from_track(track));
        }
    }

    /// Metadata of a track URL, from what was browsed or the Web API.
    pub async fn track_meta(&self, url: &str) -> Option<TrackMeta> {
        if let Some(m) = self.meta.get(url) {
            return Some(m.clone());
        }
        let (kind, id) = parse_url(url)?;
        if kind != "track" {
            return None;
        }
        let t = self.api(&format!("/tracks/{id}")).await.ok()?;
        self.remember(&t);
        self.meta.get(url).map(|m| m.clone())
    }

    /// Whether a track can be played, by librespot's rule (`Player::find_available_alternative`):
    /// it is available and has audio files, or one of its alternatives (relinked tracks) is
    /// available.  When that cannot be found out (no session, metadata error) the track counts
    /// as playable and the stream has the last word.
    pub async fn track_playable(&self, url: &str) -> bool {
        use librespot_metadata::audio::AudioItem;
        let Some((kind, id)) = parse_url(url) else {
            return true;
        };
        if kind != "track" {
            return true;
        }
        let Ok(uri) = librespot_core::SpotifyUri::from_uri(&format!("spotify:track:{id}")) else {
            return true;
        };
        let Ok(session) = self.session().await else {
            return true;
        };
        let Ok(item) = AudioItem::get_file(&session, uri).await else {
            return true;
        };
        let alternatives = item.alternatives.map(|t| t.0).unwrap_or_default();
        match playable(
            item.availability.is_ok(),
            !item.files.is_empty(),
            alternatives,
        ) {
            Playable::Yes => true,
            Playable::No => false,
            Playable::IfAnyOf(alternatives) => {
                for alt in alternatives {
                    if AudioItem::get_file(&session, alt)
                        .await
                        .is_ok_and(|a| a.availability.is_ok())
                    {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// The tracks a Spotify URL stands for (an album, playlist, artist's top tracks, liked
    /// songs, or a single track), as track URLs.
    pub async fn expand(&self, url: &str) -> Result<Vec<String>, String> {
        if let Some((_, id)) = parse_url(url).filter(|(k, _)| k == "track") {
            return Ok(vec![url_for("track", &id)]);
        }
        // a player's Connect stream plays as it is
        if parse_url(url).is_some_and(|(k, _)| k == "connect") {
            return Ok(vec![url.to_string()]);
        }
        Ok(self
            .tracks(url)
            .await?
            .iter()
            .filter_map(|t| t["id"].as_str().map(|id| url_for("track", id)))
            .collect())
    }

    /// The Web API track objects of an album, playlist, artist (top tracks) or liked songs.
    pub async fn tracks(&self, url: &str) -> Result<Vec<Value>, String> {
        let (kind, id) = parse_url(url).ok_or("not a Spotify link")?;
        let tracks: Vec<Value> = match kind.as_str() {
            "track" => vec![self.api(&format!("/tracks/{id}")).await?],
            "album" => {
                let album = self.api(&format!("/albums/{id}")).await?;
                let mut tracks = album["tracks"]["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if let Some(next) = album["tracks"]["next"].as_str() {
                    tracks.extend(self.api_all(next, 1000).await?);
                }
                // album tracks lack the album object
                for t in &mut tracks {
                    t["album"] =
                        serde_json::json!({ "name": album["name"], "images": album["images"] });
                }
                tracks
            }
            "playlist" => self
                .api_all(&format!("/playlists/{id}/tracks?limit=100"), 2000)
                .await?
                .into_iter()
                .map(|i| i["track"].clone())
                .collect(),
            "artist" => {
                let r = self
                    .api(&format!("/artists/{id}/top-tracks?market=from_token"))
                    .await?;
                r["tracks"].as_array().cloned().unwrap_or_default()
            }
            "collection" => self
                .api_all("/me/tracks?limit=50", 2000)
                .await?
                .into_iter()
                .map(|i| i["track"].clone())
                .collect(),
            _ => return Err(format!("can't play Spotify {kind} links")),
        };
        Ok(tracks
            .into_iter()
            .filter(|t| {
                t["id"].is_string()
                    && t["type"].as_str().unwrap_or("track") == "track"
                    && !t["is_local"].as_bool().unwrap_or(false)
            })
            .inspect(|t| self.remember(t))
            .collect())
    }
}

/// librespot's playability rule for a track, before its alternatives are looked up.
#[derive(Debug, PartialEq)]
enum Playable<T> {
    Yes,
    No,
    /// Playable if one of these alternatives is available.
    IfAnyOf(Vec<T>),
}

fn playable<T>(available: bool, has_files: bool, alternatives: Vec<T>) -> Playable<T> {
    if !available {
        Playable::No
    } else if has_files {
        Playable::Yes
    } else if alternatives.is_empty() {
        // librespot: "Track should be available, but no alternatives found."
        Playable::No
    } else {
        Playable::IfAnyOf(alternatives)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn own_client_id_or_default() {
        let own = "0123456789ABCDEF0123456789abcdef";
        assert_eq!(client_id_or_default(own), own.to_ascii_lowercase());
        assert_eq!(
            client_id_or_default(&format!("  {own} ")),
            own.to_ascii_lowercase()
        );
        for bad in ["", "abc", "not a client id at all, no no no", &own[1..]] {
            assert_eq!(client_id_or_default(bad), DEFAULT_CLIENT_ID, "{bad:?}");
        }
    }

    #[test]
    fn oauth_files_without_client_id_use_the_default() {
        let old: OAuthState = serde_json::from_str(r#"{"refresh_token":"r"}"#).unwrap();
        assert_eq!(old.client_id, DEFAULT_CLIENT_ID);
        let own = OAuthState {
            refresh_token: "r".into(),
            client_id: "0123456789abcdef0123456789abcdef".into(),
            access_token: None,
        };
        let back: OAuthState = serde_json::from_str(&serde_json::to_string(&own).unwrap()).unwrap();
        assert_eq!(back.client_id, own.client_id);
    }
    use super::*;

    #[test]
    fn playability_follows_librespot() {
        assert_eq!(playable::<u8>(true, true, vec![]), Playable::Yes);
        assert_eq!(playable::<u8>(false, true, vec![]), Playable::No);
        // available but without audio files or alternatives: what librespot reports as
        // "Track should be available, but no alternatives found." and skips
        assert_eq!(playable::<u8>(true, false, vec![]), Playable::No);
        assert_eq!(playable(true, false, vec![7]), Playable::IfAnyOf(vec![7]));
        assert_eq!(playable(false, false, vec![7]), Playable::No);
    }

    #[test]
    fn urls() {
        assert_eq!(
            parse_url("spotify://track:4uLU6hMCjMI75M1A2tKUQC"),
            Some(("track".into(), "4uLU6hMCjMI75M1A2tKUQC".into()))
        );
        assert_eq!(
            parse_url("spotify:album:1A2B"),
            Some(("album".into(), "1A2B".into()))
        );
        assert_eq!(
            parse_url("https://open.spotify.com/playlist/37i9dQ?si=x"),
            Some(("playlist".into(), "37i9dQ".into()))
        );
        assert_eq!(
            parse_url("spotify://collection:"),
            Some(("collection".into(), "".into()))
        );
        assert_eq!(parse_url("http://example.com"), None);
        assert!(is_spotify("spotify://track:x"));
    }

    #[test]
    fn pkce_and_codes() {
        // RFC 7636 appendix B
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            code_from("http://127.0.0.1:5588/login?code=AQB12&state=xy").as_deref(),
            Some("AQB12")
        );
        assert_eq!(code_from("  AQB12 ").as_deref(), Some("AQB12"));
        assert_eq!(
            code_from("http://127.0.0.1:5588/login?error=access_denied"),
            None
        );
        assert_eq!(random_string(96).len(), 96);
    }

    #[test]
    fn track_metadata() {
        let t = serde_json::json!({
            "id": "abc", "name": "Song", "duration_ms": 215000,
            "artists": [{ "name": "A" }, { "name": "B" }],
            "album": { "name": "Album", "images": [{ "url": "s", "width": 64 }, { "url": "l", "width": 640 }] },
        });
        let m = meta_from_track(&t);
        assert_eq!(
            (m.title.as_str(), m.artist.as_str(), m.album.as_str()),
            ("Song", "A, B", "Album")
        );
        assert_eq!(m.secs, 215.0);
        assert_eq!(m.image.as_deref(), Some("l"));
    }
}
