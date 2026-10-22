//! Spotify (the role of Lyrion's third-party "Spotty" plugin), built on librespot.
//!
//! * Sign-in: OAuth authorization code with PKCE against Spotify's own desktop client id
//!   (as librespot does).  Spotify redirects to `http://127.0.0.1:5588/login`; when the
//!   browser runs on this machine we catch that request, otherwise the user pastes the
//!   redirected address into the web UI.  The access token logs a librespot session in once;
//!   librespot's reusable credentials are kept for later sessions, and the refresh token for
//!   the Connect devices' first log-in.
//! * Browsing ([`menu`]): through the session ([`browse`]) and Spotify's GraphQL API
//!   ([`pathfinder`]), as the official apps do, presented through the XMLBrowser menus.  The
//!   Web API isn't used: the shared client id is rate-limited to the point of being useless,
//!   and apps of one's own lost the endpoints RMS needs in February 2026.
//! * Playback ([`stream`]): a librespot player per stream decodes the track to 16-bit PCM,
//!   which goes to the player like any other stream (`spotify://track:<id>` URLs).

pub mod browse;
pub mod connect;
pub mod learn;
pub mod menu;
pub mod model;
pub mod pathfinder;
pub mod stream;

use base64::Engine;
use dashmap::DashMap;
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::{Session, SessionConfig};
use model::{Album, Artist, Context, Found, Playlist, SearchKind, Track};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Spotify's desktop client id, which librespot uses for its OAuth flow: login5, which
/// sessions and Connect devices get their tokens from, refuses any other.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_PORT: u16 = 5588;
/// The sign-in (its refresh token).  Named from when there was a second sign-in, with the
/// user's own app, for the Web API.
const OAUTH_FILE: &str = "oauth_playback.json";
/// Where the sign-in was kept before that (of Spotify's client id), or the second sign-in
/// (of the user's own app, no use now): read once, then removed.
const OLD_OAUTH_FILE: &str = "oauth.json";
/// How long browsed lists are kept.
const LIST_TTL: Duration = Duration::from_secs(300);

/// Scopes requested at sign-in (librespot's set, without the write scopes).
const SCOPES: &[&str] = &[
    "streaming",
    "user-read-playback-state",
    "user-read-currently-playing",
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
    /// The album's and the first artist's ids, for browsing to them.
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
}

impl TrackMeta {
    /// The album's menu id (`album:<id>`).
    pub fn album_menu(&self) -> Option<String> {
        self.album_id.as_ref().map(|id| format!("album:{id}"))
    }

    /// The first artist's menu id (`artist:<id>`).
    pub fn artist_menu(&self) -> Option<String> {
        self.artist_id.as_ref().map(|id| format!("artist:{id}"))
    }
}

#[derive(Serialize, Deserialize, Default)]
struct OAuthState {
    refresh_token: String,
    /// The client id the refresh token belongs to, in files from when a sign-in could be of
    /// the user's own app (absent: Spotify's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
    #[serde(skip)]
    access_token: Option<(String, Instant)>,
}

struct PendingLogin {
    verifier: String,
    state: String,
}

/// A browsed list, kept for [`LIST_TTL`].
type Cached = Arc<dyn std::any::Any + Send + Sync>;

pub struct Spotify {
    dir: PathBuf,
    http: reqwest::Client,
    session: tokio::sync::Mutex<Option<Session>>,
    oauth: Mutex<Option<OAuthState>>,
    pending: Mutex<Option<PendingLogin>>,
    /// Track metadata by `spotify://track:<id>` URL.
    pub meta: DashMap<String, TrackMeta>,
    /// Browsed lists by what they are (`playlists`, `album:<id>`, …), briefly.
    lists: Mutex<HashMap<String, (Instant, Cached)>>,
    user: Mutex<Option<String>>,
    /// The GraphQL queries' IDs.
    pathfinder: pathfinder::Ids,
    /// The user's Release Radar (playlist id), once found.
    release_radar: Mutex<Option<String>>,
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
            let oauth = load_sign_in(&dir);
            let pathfinder = pathfinder::Ids::new(&dir, http.clone());
            Arc::new(Spotify {
                dir,
                http,
                session: tokio::sync::Mutex::new(None),
                oauth: Mutex::new(oauth),
                pending: Mutex::new(None),
                meta: DashMap::new(),
                lists: Mutex::new(HashMap::new()),
                user: Mutex::new(None),
                pathfinder,
                release_radar: Mutex::new(None),
                server: std::sync::OnceLock::new(),
            })
        })
        .clone()
}

/// The sign-in saved in `dir`.  One in [`OLD_OAUTH_FILE`] is moved to [`OAUTH_FILE`] when it
/// is Spotify's client id's and there is none there yet; that file goes either way.
fn load_sign_in(dir: &std::path::Path) -> Option<OAuthState> {
    let read = |file: &str| -> Option<OAuthState> {
        std::fs::read_to_string(dir.join(file))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    };
    let mut oauth = read(OAUTH_FILE);
    if dir.join(OLD_OAUTH_FILE).exists() {
        let old = read(OLD_OAUTH_FILE)
            .filter(|o| o.client_id.as_deref().is_none_or(|id| id == CLIENT_ID));
        if oauth.is_none() {
            if let Some(o) = &old {
                let _ = std::fs::write(
                    dir.join(OAUTH_FILE),
                    serde_json::to_string(o).unwrap_or_default(),
                );
            }
            oauth = old;
        }
        let _ = std::fs::remove_file(dir.join(OLD_OAUTH_FILE));
    }
    oauth
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
                self.dir.join(OAUTH_FILE),
                serde_json::to_string(o).unwrap_or_default(),
            );
        }
    }

    /// Tell the service which server it plays for (at start).
    pub fn attach(&self, server: &Arc<crate::server::Server>) {
        let _ = self.server.set(Arc::downgrade(server));
    }

    /// The server it plays for.
    pub fn server(&self) -> std::sync::Weak<crate::server::Server> {
        self.server.get().cloned().unwrap_or_default()
    }

    /// Signed in: a session can log in (stored credentials, or the sign-in's token).
    pub fn signed_in(&self) -> bool {
        self.dir.join("credentials.json").exists() || self.oauth.lock().is_some()
    }

    pub fn username(&self) -> Option<String> {
        self.user.lock().clone()
    }

    /// Forget what was browsed (another account signs in, or none).
    fn forget_browsed(&self) {
        self.lists.lock().clear();
        *self.release_radar.lock() = None;
    }

    // ---- sign-in ------------------------------------------------------------------------------

    /// Start signing in: the Spotify authorization URL to open.  Also listens on the
    /// redirect port to finish by itself when the browser runs on this machine.
    pub fn start_login(self: &Arc<Self>) -> String {
        let verifier = random_string(96);
        let state = random_string(16);
        let mut url = url::Url::parse("https://accounts.spotify.com/authorize").expect("url");
        url.query_pairs_mut()
            .append_pair("client_id", CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri())
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge(&verifier))
            .append_pair("scope", &SCOPES.join(" "))
            .append_pair("state", &state);
        *self.pending.lock() = Some(PendingLogin { verifier, state });
        let me = self.clone();
        crate::util::spawn(async move { me.listen_for_redirect().await });
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
        let verifier = self
            .pending
            .lock()
            .as_ref()
            .map(|p| p.verifier.clone())
            .ok_or("no sign-in in progress; start again")?;
        let resp: Value = self
            .http
            .post("https://accounts.spotify.com/api/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri().as_str()),
                ("client_id", CLIENT_ID),
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
            client_id: None,
            access_token: Some((
                access.clone(),
                Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
            )),
        });
        self.save_oauth();
        self.forget_browsed();
        // Log a session in with the new token; librespot stores reusable credentials.  The
        // Connect devices log in afresh with it.
        let _ = std::fs::remove_file(self.dir.join("credentials.json"));
        *self.session.lock().await = None;
        let session = self.connect(Credentials::with_access_token(access)).await?;
        connect::signed_in_again();
        let user = session.username();
        info!("Spotify: signed in as {user}");
        Ok(user)
    }

    /// Forget the account.
    pub async fn logout(&self) {
        if let Some(s) = self.session.lock().await.take() {
            s.shutdown();
        }
        *self.oauth.lock() = None;
        *self.user.lock() = None;
        self.forget_browsed();
        let _ = std::fs::remove_file(self.dir.join("credentials.json"));
        let _ = std::fs::remove_file(self.dir.join(OAUTH_FILE));
        let _ = std::fs::remove_file(self.dir.join(OLD_OAUTH_FILE));
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
                Err(e) => warn!("{e}; trying the sign-in's token"),
            }
        }
        let token = self.token().await?;
        self.connect(Credentials::with_access_token(token)).await
    }

    /// An access token of the sign-in, from its refresh token (for a session's first log-in).
    pub(crate) async fn token(&self) -> Result<String, String> {
        let refresh = {
            let g = self.oauth.lock();
            let o = g.as_ref().ok_or("not signed in to Spotify")?;
            if let Some((t, until)) = &o.access_token {
                if *until > Instant::now() {
                    return Ok(t.clone());
                }
            }
            o.refresh_token.clone()
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
                ("client_id", CLIENT_ID),
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

    /// The account's display name (else its username) and product (`premium`, `free`, …).
    pub async fn account(&self) -> Result<(String, Option<String>), String> {
        self.cached("account".into(), async {
            let session = self.session().await?;
            let user = session.username();
            let name = browse::display_name(&session, &user).await.unwrap_or(user);
            Ok((name, session.get_user_attribute("type")))
        })
        .await
    }

    // ---- browsing -----------------------------------------------------------------------------

    /// What `fetch` gives, kept as `key` for [`LIST_TTL`].
    async fn cached<T>(
        &self,
        key: String,
        fetch: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String>
    where
        T: Clone + Send + Sync + 'static,
    {
        let kept = self
            .lists
            .lock()
            .get(&key)
            .filter(|(at, _)| at.elapsed() < LIST_TTL)
            .and_then(|(_, v)| v.downcast_ref::<T>().cloned());
        if let Some(v) = kept {
            return Ok(v);
        }
        let v = fetch.await?;
        let mut lists = self.lists.lock();
        lists.retain(|_, (at, _)| at.elapsed() < LIST_TTL);
        lists.insert(key, (Instant::now(), Arc::new(v.clone())));
        Ok(v)
    }

    pub async fn liked(&self) -> Result<Vec<Track>, String> {
        self.cached("liked".into(), async {
            browse::liked(&self.session().await?).await
        })
        .await
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>, String> {
        self.cached("playlists".into(), async {
            browse::playlists(&self.session().await?).await
        })
        .await
    }

    pub async fn saved_albums(&self) -> Result<Vec<Album>, String> {
        self.cached("albums".into(), async {
            browse::saved_albums(&self.session().await?).await
        })
        .await
    }

    pub async fn followed_artists(&self) -> Result<Vec<Artist>, String> {
        self.cached("artists".into(), async {
            browse::followed_artists(&self.session().await?).await
        })
        .await
    }

    pub async fn recently_played(&self) -> Result<Vec<Context>, String> {
        self.cached("recent".into(), async {
            browse::recently_played(&self.session().await?).await
        })
        .await
    }

    pub async fn album_tracks(&self, id: &str) -> Result<Vec<Track>, String> {
        self.cached(format!("album:{id}"), async {
            browse::album_tracks(&self.session().await?, id).await
        })
        .await
    }

    pub async fn playlist_tracks(&self, id: &str) -> Result<Vec<Track>, String> {
        self.cached(format!("playlist:{id}"), async {
            browse::playlist_tracks(&self.session().await?, id).await
        })
        .await
    }

    pub async fn artist_top_tracks(&self, id: &str) -> Result<Vec<Track>, String> {
        self.cached(format!("artisttop:{id}"), async {
            browse::artist_top_tracks(&self.session().await?, id).await
        })
        .await
    }

    pub async fn artist_albums(&self, id: &str) -> Result<Vec<Album>, String> {
        self.cached(format!("artistalbums:{id}"), async {
            browse::artist_albums(&self.session().await?, id).await
        })
        .await
    }

    /// The user's top tracks of the last four weeks, through the GraphQL API.
    pub async fn top_tracks(&self) -> Result<Vec<Track>, String> {
        self.cached("toptracks".into(), async {
            let session = self.session().await?;
            let uris = self.top(&session, "tracks").await?;
            browse::tracks(&session, &uris).await
        })
        .await
    }

    /// The user's top artists of the last four weeks, through the GraphQL API.
    pub async fn top_artists(&self) -> Result<Vec<Artist>, String> {
        self.cached("topartists".into(), async {
            let session = self.session().await?;
            let uris = self.top(&session, "artists").await?;
            browse::artists(&session, &uris).await
        })
        .await
    }

    /// The URIs of the user's top `kind` (tracks or artists).
    async fn top(&self, session: &Session, kind: &str) -> Result<Vec<String>, String> {
        let vars = pathfinder::top_variables(kind, 50);
        let data = self
            .pathfinder
            .query(&self.http, session, "userTopContent", vars)
            .await?;
        Ok(pathfinder::top_uris(&data, kind))
    }

    /// Search for one kind of thing, through the GraphQL API.  `term` may use the apps' field
    /// filters (`artist:"…"`, `isrc:…`).
    pub async fn search(
        &self,
        kind: SearchKind,
        term: &str,
        limit: usize,
    ) -> Result<Found, String> {
        self.cached(format!("search{}:{limit}:{term}", kind.name()), async {
            let session = self.session().await?;
            let (op, field) = pathfinder::search_op(kind);
            let vars = pathfinder::search_variables(term, limit);
            let data = self
                .pathfinder
                .query(&self.http, &session, op, vars)
                .await?;
            let uris = pathfinder::search_uris(&data, field);
            Ok(match kind {
                SearchKind::Track => Found::Tracks(browse::tracks(&session, &uris).await?),
                SearchKind::Album => Found::Albums(browse::albums(&session, &uris).await?),
                SearchKind::Artist => Found::Artists(browse::artists(&session, &uris).await?),
                SearchKind::Playlist => Found::Playlists(pathfinder::search_playlists(&data)),
            })
        })
        .await
    }

    pub async fn search_tracks(&self, term: &str, limit: usize) -> Result<Vec<Track>, String> {
        match self.search(SearchKind::Track, term, limit).await? {
            Found::Tracks(t) => Ok(t),
            _ => Ok(Vec::new()),
        }
    }

    pub async fn search_artists(&self, term: &str, limit: usize) -> Result<Vec<Artist>, String> {
        match self.search(SearchKind::Artist, term, limit).await? {
            Found::Artists(a) => Ok(a),
            _ => Ok(Vec::new()),
        }
    }

    /// The albums Spotify's editors picked, from Browse's New Releases page (through the
    /// GraphQL API).
    pub async fn new_releases(&self) -> Result<Vec<Album>, String> {
        self.cached("new".into(), async {
            let session = self.session().await?;
            let vars = pathfinder::new_releases_variables();
            let data = self
                .pathfinder
                .query(&self.http, &session, "browsePage", vars)
                .await?;
            let uris = pathfinder::new_release_uris(&data)
                .ok_or("Spotify's New Releases page has no albums section any more")?;
            browse::albums(&session, &uris).await
        })
        .await
    }

    /// The user's Release Radar (playlist id: one for each user, for good, its tracks
    /// changing each Friday), from Browse's New Releases page the first time.
    pub async fn release_radar(&self) -> Result<String, String> {
        if let Some(id) = self.release_radar.lock().clone() {
            return Ok(id);
        }
        let session = self.session().await?;
        let vars = pathfinder::new_releases_variables();
        let data = self
            .pathfinder
            .query(&self.http, &session, "browsePage", vars)
            .await?;
        let id = pathfinder::release_radar(&data)
            .ok_or("Release Radar wasn't found on Spotify's New Releases page")?;
        *self.release_radar.lock() = Some(id.clone());
        Ok(id)
    }

    /// What Spotify plays next on a player's Connect device (`key`, see [`connect::key`]),
    /// as the device last heard from Spotify: `spotify://track:…` URLs and what they are.
    pub async fn queue(&self, key: &str) -> Result<Vec<(String, TrackMeta)>, String> {
        let uris = connect::up_next(key);
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        let tracks = browse::tracks(&self.session().await?, &uris).await?;
        Ok(tracks
            .iter()
            .inspect(|t| self.remember(t))
            .map(|t| (t.url(), t.meta()))
            .collect())
    }

    /// Remember the metadata of a track seen while browsing.
    pub fn remember(&self, track: &Track) {
        self.meta.insert(track.url(), track.meta());
    }

    /// Metadata of a track URL, from what was browsed or the session.
    pub async fn track_meta(&self, url: &str) -> Option<TrackMeta> {
        // (a learned track's remembered metadata has no ids: looked up again for them)
        let known = self.meta.get(url).map(|m| m.clone());
        if let Some(m) = known.as_ref().filter(|m| m.album_id.is_some()) {
            return Some(m.clone());
        }
        let (kind, id) = parse_url(url)?;
        if kind != "track" {
            return known;
        }
        let looked_up = async {
            let session = self.session().await.ok()?;
            browse::tracks(&session, &[format!("spotify:track:{id}")])
                .await
                .ok()?
                .pop()
        };
        let Some(t) = looked_up.await else {
            return known;
        };
        self.remember(&t);
        Some(t.meta())
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
        Ok(self.tracks(url).await?.iter().map(Track::url).collect())
    }

    /// The tracks of an album, playlist, artist (top tracks), liked songs or a track.
    pub async fn tracks(&self, url: &str) -> Result<Vec<Track>, String> {
        let (kind, id) = parse_url(url).ok_or("not a Spotify link")?;
        let tracks = match kind.as_str() {
            "track" => {
                let session = self.session().await?;
                browse::tracks(&session, &[format!("spotify:track:{id}")]).await?
            }
            "album" => self.album_tracks(&id).await?,
            "playlist" => self.playlist_tracks(&id).await?,
            "artist" => self.artist_top_tracks(&id).await?,
            "collection" => self.liked().await?,
            _ => return Err(format!("can't play Spotify {kind} links")),
        };
        tracks.iter().for_each(|t| self.remember(t));
        Ok(tracks)
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
    use super::*;

    fn state(client_id: Option<&str>) -> String {
        serde_json::to_string(&OAuthState {
            refresh_token: "r".into(),
            client_id: client_id.map(str::to_string),
            access_token: None,
        })
        .unwrap()
    }

    #[test]
    fn an_old_sign_in_of_spotifys_client_id_is_kept_and_one_of_an_own_app_dropped() {
        // from before the two sign-ins: moved
        for id in [None, Some(CLIENT_ID)] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(OLD_OAUTH_FILE), state(id)).unwrap();
            assert_eq!(load_sign_in(dir.path()).unwrap().refresh_token, "r");
            assert!(!dir.path().join(OLD_OAUTH_FILE).exists());
            assert!(load_sign_in(dir.path()).is_some(), "kept in the new file");
        }
        // the user's own app (browsing only): dropped
        let own = "0123456789abcdef0123456789abcdef";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(OLD_OAUTH_FILE), state(Some(own))).unwrap();
        assert!(load_sign_in(dir.path()).is_none());
        assert!(!dir.path().join(OLD_OAUTH_FILE).exists());
        // both: the playback one stays
        std::fs::write(dir.path().join(OLD_OAUTH_FILE), state(Some(own))).unwrap();
        std::fs::write(dir.path().join(OAUTH_FILE), state(Some(CLIENT_ID))).unwrap();
        let kept = load_sign_in(dir.path()).unwrap();
        assert_eq!(kept.client_id.as_deref(), Some(CLIENT_ID));
        assert!(!dir.path().join(OLD_OAUTH_FILE).exists());
    }

    #[test]
    fn new_sign_ins_are_saved_without_a_client_id() {
        assert_eq!(state(None), r#"{"refresh_token":"r"}"#);
    }

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

    #[tokio::test]
    async fn lists_are_kept_for_a_while_and_forgotten_on_sign_out() {
        let dir = tempfile::tempdir().unwrap();
        let sp = Spotify {
            dir: dir.path().to_path_buf(),
            http: reqwest::Client::new(),
            session: tokio::sync::Mutex::new(None),
            oauth: Mutex::new(None),
            pending: Mutex::new(None),
            meta: DashMap::new(),
            lists: Mutex::new(HashMap::new()),
            user: Mutex::new(None),
            pathfinder: pathfinder::Ids::new(dir.path(), reqwest::Client::new()),
            release_radar: Mutex::new(Some("rr".into())),
            server: OnceLock::new(),
        };
        let fetch = |n: u32| async move { Ok::<_, String>(vec![n]) };
        assert_eq!(sp.cached("k".into(), fetch(1)).await, Ok(vec![1]));
        assert_eq!(sp.cached("k".into(), fetch(2)).await, Ok(vec![1]));
        // another type under the same key is fetched, not mistaken for it
        assert_eq!(sp.cached("k".into(), async { Ok(3u8) }).await, Ok(3));
        // an error isn't kept
        assert!(sp
            .cached::<u8>("e".into(), async { Err("no".into()) })
            .await
            .is_err());
        assert_eq!(sp.cached("e".into(), async { Ok(4u8) }).await, Ok(4));
        sp.logout().await;
        assert_eq!(sp.cached("k".into(), async { Ok(5u8) }).await, Ok(5));
        assert!(sp.release_radar.lock().is_none());
    }
}
