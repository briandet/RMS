//! Spotify's own GraphQL API ("pathfinder"), for what neither the Web API (rate-limited on
//! the shared client id) nor the session's services offer: the user's top tracks and artists.
//!
//! It answers the session's login5 token, with the client token and a Spotify user agent
//! (librespot's; others are refused).  Queries go by ID (the hash of a "persisted query"),
//! which Spotify doesn't publish and changes with the query.  [`scrape`] finds them in the
//! web player's JavaScript; they are kept in `pathfinder.json` in the Spotify data folder
//! (which can be edited by hand, e.g. to put in an ID when the web player changes too much
//! for [`scrape`]).  They are looked up again when the file is missing or lacks a query, and
//! when Spotify answers that an ID is stale (412), at most once an hour.  Undocumented: if
//! Spotify changes a query's variables, the menus say so until RMS is updated.

pub mod scrape;

use futures::future::BoxFuture;
use librespot_core::Session;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
/// The IDs file, in the Spotify data folder.
pub const FILE: &str = "pathfinder.json";
/// Looking the IDs up again, at the soonest.
const REFRESH_EVERY: Duration = Duration::from_secs(3600);

/// Looks the IDs up afresh: the queries (name → ID) and where they were found.
type Refresher = Box<
    dyn Fn() -> BoxFuture<'static, Result<(String, HashMap<String, String>), String>> + Send + Sync,
>;

/// The query IDs: kept in [`FILE`], looked up again by a [`Refresher`].
pub struct Ids {
    file: PathBuf,
    refresher: Refresher,
    ids: Mutex<Option<HashMap<String, String>>>,
    /// The last look-up.
    refreshed: tokio::sync::Mutex<Option<Instant>>,
    endpoint: String,
}

impl Ids {
    /// Kept in `dir`, looked up in the web player ([`scrape`]).
    pub fn new(dir: &Path, http: reqwest::Client) -> Self {
        Self::with_refresher(
            dir.join(FILE),
            Box::new(move || {
                let http = http.clone();
                Box::pin(async move { scrape::scrape(&http).await })
            }),
        )
    }

    pub fn with_refresher(file: PathBuf, refresher: Refresher) -> Self {
        Ids {
            file,
            refresher,
            ids: Mutex::new(None),
            refreshed: tokio::sync::Mutex::new(None),
            endpoint: ENDPOINT.into(),
        }
    }

    /// `op`'s ID: from the file, else after looking them up (`true`: they were).
    async fn id(&self, op: &str) -> Result<(String, bool), String> {
        if let Some(id) = self.known(op) {
            return Ok((id, false));
        }
        self.refresh().await?;
        self.known(op)
            .map(|id| (id, true))
            .ok_or_else(|| format!("Spotify's {op} query wasn't found in the web player"))
    }

    fn known(&self, op: &str) -> Option<String> {
        let mut ids = self.ids.lock();
        if ids.is_none() {
            *ids = std::fs::read_to_string(&self.file)
                .ok()
                .and_then(|s| parse_ids(&s));
        }
        ids.as_ref()?.get(op).cloned()
    }

    /// Look the IDs up again (unless that was within the hour) and keep them.
    async fn refresh(&self) -> Result<(), String> {
        let mut last = self.refreshed.lock().await;
        if last.is_some_and(|t| t.elapsed() < REFRESH_EVERY) {
            return Err("Spotify's query IDs were looked up less than an hour ago".into());
        }
        *last = Some(Instant::now());
        let (source, ops) = (self.refresher)()
            .await
            .inspect_err(|e| warn!("Spotify: looking up the query IDs: {e}"))?;
        info!("Spotify: {} query IDs from {source}", ops.len());
        if let Err(e) = write_ids(&self.file, &source, &ops) {
            warn!("Spotify: {}: {e}", self.file.display());
        }
        *self.ids.lock() = Some(ops);
        Ok(())
    }

    /// Run query `op` with `variables`: its `data`.
    pub async fn query(
        &self,
        http: &reqwest::Client,
        session: &Session,
        op: &str,
        variables: Value,
    ) -> Result<Value, String> {
        let token = session
            .login5()
            .auth_token()
            .await
            .map_err(|e| format!("Spotify token: {e}"))?
            .access_token;
        let client_token = session
            .spclient()
            .client_token()
            .await
            .map_err(|e| format!("Spotify client token: {e}"))?;
        self.post(http, &token, &client_token, op, &variables).await
    }

    /// [`Self::query`] with the tokens; a stale ID is refreshed and the query sent again.
    async fn post(
        &self,
        http: &reqwest::Client,
        token: &str,
        client_token: &str,
        op: &str,
        variables: &Value,
    ) -> Result<Value, String> {
        // looked up for this query: a stale ID is the newest there is
        let mut refreshed = false;
        loop {
            let (id, ran) = self.id(op).await?;
            refreshed |= ran;
            let resp = http
                .post(&self.endpoint)
                .bearer_auth(token)
                .header("client-token", client_token)
                .header(
                    reqwest::header::USER_AGENT,
                    librespot_core::version::FALLBACK_USER_AGENT,
                )
                .json(&request(op, &id, variables))
                .send()
                .await
                .map_err(|e| format!("Spotify {op}: {e}"))?;
            let status = resp.status().as_u16();
            if status == 412 {
                // a stale ID
                warn!("Spotify {op}: the query ID is stale");
                if refreshed {
                    return Err(format!(
                        "Spotify's {op} query ID is stale, and the web player has no other"
                    ));
                }
                self.refresh().await?;
                refreshed = true;
                continue;
            }
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            return answer(op, status, body);
        }
    }
}

/// A persisted-query request.
fn request(op: &str, id: &str, variables: &Value) -> Value {
    json!({
        "operationName": op,
        "variables": variables,
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": id } },
    })
}

/// The `data` of an answer, or what went wrong.
fn answer(op: &str, status: u16, body: Value) -> Result<Value, String> {
    let errors: Vec<&str> = body["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["message"].as_str())
        .collect();
    match body.get("data").filter(|d| !d.is_null()) {
        Some(d) if status == 200 => Ok(d.clone()),
        _ if !errors.is_empty() => Err(format!(
            "Spotify changed its {op} query (RMS needs an update): {}",
            errors.join("; ")
        )),
        _ => Err(format!(
            "Spotify {op}: {status} {}",
            body["error"]["message"].as_str().unwrap_or("")
        )),
    }
}

/// The IDs in [`FILE`].
fn parse_ids(s: &str) -> Option<HashMap<String, String>> {
    let v: Value = serde_json::from_str(s).ok()?;
    Some(
        v["operations"]
            .as_object()?
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect(),
    )
}

/// Write [`FILE`] (whole or not at all).
fn write_ids(file: &Path, source: &str, ops: &HashMap<String, String>) -> std::io::Result<()> {
    let doc = json!({
        "fetched_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "source": source,
        "operations": ops.iter().collect::<BTreeMap<_, _>>(),
    });
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.new");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc)? + "\n")?;
    std::fs::rename(&tmp, file)
}

/// The URIs of the user's top tracks or artists (`kind`) in a `userTopContent` answer.
pub fn top_uris(data: &Value, kind: &str) -> Vec<String> {
    let (field, typename) = match kind {
        "tracks" => ("topTracks", "Track"),
        _ => ("topArtists", "Artist"),
    };
    data["me"]["profile"][field]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| &i["data"])
        .filter(|d| d["__typename"] == typename)
        .filter_map(|d| d["uri"].as_str().map(str::to_string))
        .collect()
}

/// `userTopContent`'s variables for the top `kind` (tracks or artists) of the last four
/// weeks, as the profile page shows them.
pub fn top_variables(kind: &str, limit: usize) -> Value {
    let input = json!({
        "offset": 0, "limit": limit, "sortBy": "AFFINITY", "timeRange": "SHORT_TERM",
    });
    json!({
        "includeTopTracks": kind == "tracks",
        "topTracksInput": input,
        "includeTopArtists": kind != "tracks",
        "topArtistsInput": input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn top_content_lists_the_uris_of_its_tracks_or_artists() {
        let data = json!({ "me": { "profile": {
            "topTracks": { "items": [
                { "data": { "__typename": "Track", "uri": "spotify:track:t1" } },
                { "data": { "__typename": "Episode", "uri": "spotify:episode:e1" } },
                { "data": { "__typename": "Track", "uri": "spotify:track:t2" } },
            ]},
            "topArtists": { "items": [
                { "data": { "__typename": "Artist", "uri": "spotify:artist:a1" } },
            ]},
        }}});
        assert_eq!(
            top_uris(&data, "tracks"),
            ["spotify:track:t1", "spotify:track:t2"]
        );
        assert_eq!(top_uris(&data, "artists"), ["spotify:artist:a1"]);
        assert!(top_uris(&json!({ "me": null }), "tracks").is_empty());
    }

    #[test]
    fn an_answer_is_its_data_or_says_what_changed() {
        let ok = json!({ "data": { "me": {} } });
        assert_eq!(answer("q", 200, ok), Ok(json!({ "me": {} })));
        let changed = json!({ "errors": [{ "message": "missing variable `$x`" }] });
        let err = answer("q", 400, changed).unwrap_err();
        assert!(
            err.contains("changed its q query") && err.contains("missing variable"),
            "{err}"
        );
        let refused =
            json!({ "error": { "status": 403, "message": "Client/request not allowed" } });
        assert_eq!(
            answer("q", 403, refused),
            Err("Spotify q: 403 Client/request not allowed".into())
        );
    }

    /// A refresher that finds `ops`, and how many times it ran.
    fn finds(ops: &[(&str, &str)]) -> (Refresher, Arc<AtomicUsize>) {
        let ops: HashMap<String, String> = ops
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let refresher: Refresher = Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let ops = ops.clone();
            Box::pin(async move { Ok(("web-player.test.js".to_string(), ops)) })
        });
        (refresher, runs)
    }

    #[tokio::test]
    async fn a_missing_id_is_looked_up_at_most_hourly_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        let (refresher, runs) = finds(&[("userTopContent", "abc")]);
        let ids = Ids::with_refresher(file.clone(), refresher);
        assert_eq!(ids.id("userTopContent").await, Ok(("abc".into(), true)));
        assert_eq!(ids.id("userTopContent").await, Ok(("abc".into(), false)));
        // not found, and looked up just now: not again
        let err = ids.id("searchDesktop").await.unwrap_err();
        assert!(err.contains("less than an hour ago"), "{err}");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // kept in the file, for the next start
        let (refresher, runs) = finds(&[]);
        let next = Ids::with_refresher(file, refresher);
        assert_eq!(next.id("userTopContent").await, Ok(("abc".into(), false)));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_id_put_in_the_file_by_hand_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(&file, r#"{"operations": {"userTopContent": "mine"}}"#).unwrap();
        let (refresher, runs) = finds(&[]);
        let ids = Ids::with_refresher(file, refresher);
        assert_eq!(ids.id("userTopContent").await, Ok(("mine".into(), false)));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_failed_look_up_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let ids = Ids::with_refresher(
            dir.path().join(FILE),
            Box::new(|| Box::pin(async { Err("no queries found in the web player".into()) })),
        );
        let err = ids.id("userTopContent").await.unwrap_err();
        assert!(err.contains("no queries found"), "{err}");
        assert!(!dir.path().join(FILE).exists());
    }

    /// A pathfinder that knows query `q` by ID "new": 412 for any other.
    async fn fake_pathfinder() -> String {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/query",
            post(|Json(req): Json<Value>| async move {
                match req["extensions"]["persistedQuery"]["sha256Hash"].as_str() {
                    Some("new") => (
                        axum::http::StatusCode::OK,
                        Json(json!({ "data": { "vars": req["variables"] } })),
                    ),
                    _ => (
                        axum::http::StatusCode::PRECONDITION_FAILED,
                        Json(json!("Invalid query hash")),
                    ),
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{addr}/query")
    }

    #[tokio::test]
    async fn a_stale_id_is_looked_up_and_the_query_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        std::fs::write(&file, r#"{"operations": {"q": "old"}}"#).unwrap();
        let mut ids = Ids::with_refresher(file, finds(&[("q", "new")]).0);
        ids.endpoint = fake_pathfinder().await;
        let http = reqwest::Client::new();
        let vars = json!({ "x": 1 });
        assert_eq!(
            ids.post(&http, "t", "c", "q", &vars).await,
            Ok(json!({ "vars": { "x": 1 } }))
        );
        assert_eq!(ids.known("q").as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn a_still_stale_id_is_reported_once_looked_up() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        let mut ids = Ids::with_refresher(file, finds(&[("q", "old")]).0);
        ids.endpoint = fake_pathfinder().await;
        let http = reqwest::Client::new();
        let err = ids
            .post(&http, "t", "c", "q", &json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("web player has no other"), "{err}");
        // looked up once (for the missing file); within the hour, not again
        let err = ids
            .post(&http, "t", "c", "q", &json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("less than an hour ago"), "{err}");
    }

    #[test]
    fn top_variables_ask_for_one_kind() {
        let v = top_variables("artists", 50);
        assert_eq!(v["includeTopArtists"], true);
        assert_eq!(v["includeTopTracks"], false);
        assert_eq!(v["topArtistsInput"]["timeRange"], "SHORT_TERM");
        assert_eq!(v["topArtistsInput"]["limit"], 50);
    }
}
