//! Identifying recordings with MusicBrainz (pref `musicbrainz`, off by default: it sends what
//! is played and owned to musicbrainz.org and acoustid.org).  In the background, one request a
//! second as MusicBrainz asks, each local recording and learned streamed track is found on
//! MusicBrainz, in order of trust:
//!
//! 1. its MusicBrainz recording id, from the tags;
//! 2. its ISRC (tags, or the streaming service);
//! 3. its audio fingerprint on AcoustID (with an AcoustID application key, pref `acoustidKey`:
//!    free from <https://acoustid.org/new-application>);
//! 4. a search by artist, title and length, taking a studio recording over live ones.
//!
//! What is found (the recording, its ISRC, its genres or else its artist's) is kept by the
//! library, which fills in what the tags leave out (`rms_library::musicbrainz`).

use crate::server::Server;
use rms_library::musicbrainz::{Found, Job};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

pub const PREF: &str = "musicbrainz";
pub const KEY_PREF: &str = "acoustidKey";
/// Recordings looked up per round.
const BATCH: usize = 25;
/// When there is nothing to look up, look again this often (new files, new learned tracks).
const IDLE: Duration = Duration::from_secs(300);
/// A genre must have this share of the top genre's votes to be taken.
const GENRE_SHARE: f64 = 0.25;
const MAX_GENRES: usize = 3;
/// A search result must score this (0–100) and be this close in length (seconds).
const MIN_SCORE: i64 = 95;
const LENGTH_SLACK: f64 = 5.0;

pub fn enabled(server: &Server) -> bool {
    server.prefs.server.get_bool(PREF)
}

/// Run the lookups for as long as the server runs.
pub async fn run(server: Arc<Server>) {
    let mut client = Client::new(&server);
    loop {
        if !enabled(&server) {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        let lib = server.library.clone();
        let jobs = tokio::task::spawn_blocking(move || lib.musicbrainz_pending(BATCH))
            .await
            .unwrap_or_default();
        if jobs.is_empty() {
            tokio::time::sleep(IDLE).await;
            continue;
        }
        client.key = server.prefs.server.get_str(KEY_PREF).trim().to_string();
        let mut found = 0;
        for job in &jobs {
            if !enabled(&server) {
                break;
            }
            let f = match client.identify(job).await {
                Ok(f) => f,
                Err(e) => {
                    // the service is down or refuses: try again later
                    warn!("MusicBrainz: {e}");
                    tokio::time::sleep(IDLE).await;
                    break;
                }
            };
            if f.mbid.is_some() {
                found += 1;
            }
            debug!(
                "MusicBrainz: \"{}\" by {}: {} ({})",
                job.title,
                job.artist,
                f.mbid.as_deref().unwrap_or("not found"),
                f.how
            );
            if let Err(e) = server.library.store_musicbrainz(&job.fingerprint, &f) {
                warn!("MusicBrainz: storing {}: {e}", job.fingerprint);
            }
        }
        let lib = server.library.clone();
        let changed = tokio::task::spawn_blocking(move || lib.apply_musicbrainz())
            .await
            .unwrap_or(0);
        info!(
            "MusicBrainz: identified {found} of {} recordings; {changed} tracks filled in",
            jobs.len()
        );
    }
}

pub struct Client {
    http: reqwest::Client,
    mb: String,
    acoustid: String,
    /// The AcoustID application key ("" without).
    pub key: String,
    /// Least time between requests to a service.
    interval: Duration,
    last_mb: Option<Instant>,
    last_acoustid: Option<Instant>,
}

impl Client {
    pub fn new(server: &Server) -> Client {
        let mb = server
            .config
            .musicbrainz_url
            .trim_end_matches('/')
            .to_string();
        let local = mb.starts_with("http://127.0.0.1") || mb.starts_with("http://localhost");
        Client {
            http: server.http.clone(),
            mb,
            acoustid: server.config.acoustid_url.trim_end_matches('/').to_string(),
            key: String::new(),
            // MusicBrainz: one request a second (AcoustID allows three)
            interval: if local {
                Duration::ZERO
            } else {
                Duration::from_millis(1100)
            },
            last_mb: None,
            last_acoustid: None,
        }
    }

    /// Find a recording on MusicBrainz.  `Err` only when the services can't be asked.
    pub async fn identify(&mut self, job: &Job) -> Result<Found, String> {
        if let Some(id) = job.mbid.as_deref().filter(|m| is_mbid(m)) {
            if let Some(f) = self.recording(id, "tag").await? {
                return Ok(f);
            }
        }
        if let Some(isrc) = job.isrc.as_deref() {
            let v = self
                .mb_get(&format!("/ws/2/isrc/{isrc}?inc=artist-credits&fmt=json"))
                .await?;
            if let Some(id) = best(job, v["recordings"].as_array(), false) {
                if let Some(f) = self.recording(&id, "isrc").await? {
                    return Ok(f);
                }
            }
        }
        if !self.key.is_empty() {
            if let (Some(print), Some(d)) = (&job.print, job.duration) {
                if let Some(id) = self.acoustid(job, print, d).await? {
                    if let Some(f) = self.recording(&id, "acoustid").await? {
                        return Ok(f);
                    }
                }
            }
        }
        if !job.title.is_empty() && !job.artist.is_empty() {
            let mut q = format!(
                "recording:\"{}\" AND artist:\"{}\"",
                lucene(&job.title),
                lucene(&job.artist)
            );
            if let Some(d) = job.duration {
                let ms = (d * 1000.0) as i64;
                let slack = (LENGTH_SLACK * 1000.0) as i64;
                q.push_str(&format!(" AND dur:[{} TO {}]", ms - slack, ms + slack));
            }
            let url = format!("/ws/2/recording?query={}&limit=10&fmt=json", percent(&q));
            let v = self.mb_get(&url).await?;
            if let Some(id) = best(job, v["recordings"].as_array(), true) {
                if let Some(f) = self.recording(&id, "search").await? {
                    return Ok(f);
                }
            }
        }
        Ok(Found {
            how: "none".into(),
            ..Default::default()
        })
    }

    /// A recording's ISRC, genres (or its artist's) and artist.
    async fn recording(&mut self, id: &str, how: &str) -> Result<Option<Found>, String> {
        let v = self
            .mb_get(&format!(
                "/ws/2/recording/{id}?inc=isrcs+genres+artist-credits&fmt=json"
            ))
            .await?;
        if v["id"].as_str().is_none() {
            return Ok(None);
        }
        let artist = &v["artist-credit"][0]["artist"];
        let mut genres = top_genres(&v["genres"]);
        if genres.is_empty() {
            genres = top_genres(&artist["genres"]);
        }
        Ok(Some(Found {
            mbid: v["id"].as_str().map(str::to_string),
            isrc: v["isrcs"][0].as_str().map(str::to_string),
            artist_mbid: artist["id"].as_str().map(str::to_string),
            genres,
            how: how.into(),
        }))
    }

    /// The recording AcoustID knows the fingerprint as, if it is sure.
    async fn acoustid(
        &mut self,
        job: &Job,
        print: &[u32],
        secs: f64,
    ) -> Result<Option<String>, String> {
        use base64::Engine;
        let fp = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(rms_library::print::compressed(print));
        wait(&mut self.last_acoustid, self.interval / 3).await;
        let form = [
            ("client", self.key.as_str()),
            ("meta", "recordings"),
            ("duration", &(secs.round() as i64).to_string()),
            ("fingerprint", &fp),
            ("format", "json"),
        ];
        let resp = self
            .http
            .post(format!("{}/v2/lookup", self.acoustid))
            .form(&form)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("AcoustID: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("AcoustID: {e}"))?;
        if v["status"] != "ok" {
            return Err(format!("AcoustID: {}", v["error"]["message"]));
        }
        let Some(results) = v["results"].as_array() else {
            return Ok(None);
        };
        let recordings: Vec<Value> = results
            .iter()
            .filter(|r| r["score"].as_f64().unwrap_or(0.0) >= 0.8)
            .flat_map(|r| r["recordings"].as_array().cloned().unwrap_or_default())
            .collect();
        // AcoustID gives length in seconds, MusicBrainz in milliseconds
        let recordings: Vec<Value> = recordings
            .into_iter()
            .map(|mut r| {
                if let Some(d) = r["duration"].as_f64() {
                    r["length"] = Value::from(d * 1000.0);
                }
                r
            })
            .collect();
        Ok(best(job, Some(&recordings), false))
    }

    async fn mb_get(&mut self, path: &str) -> Result<Value, String> {
        for attempt in 0..4 {
            wait(&mut self.last_mb, self.interval).await;
            let resp = self
                .http
                .get(format!("{}{}", self.mb, path))
                .header("User-Agent", user_agent())
                .header("Accept", "application/json")
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|e| format!("MusicBrainz: {e}"))?;
            match resp.status().as_u16() {
                // "busy": too many requests from everyone, or from us
                503 | 429 => {
                    tokio::time::sleep(Duration::from_secs(2 << attempt)).await;
                    continue;
                }
                404 => return Ok(Value::Null),
                s if s >= 500 => return Err(format!("MusicBrainz: HTTP {s}")),
                _ => {}
            }
            return resp.json().await.map_err(|e| format!("MusicBrainz: {e}"));
        }
        Err("MusicBrainz: busy".into())
    }
}

/// MusicBrainz wants to know who asks.
fn user_agent() -> String {
    format!(
        "RustyMusicServer/{} ( https://github.com/rustydc/RMS )",
        env!("CARGO_PKG_VERSION")
    )
}

async fn wait(last: &mut Option<Instant>, interval: Duration) {
    if let Some(t) = *last {
        let next = t + interval;
        let now = Instant::now();
        if next > now {
            tokio::time::sleep(next - now).await;
        }
    }
    *last = Some(Instant::now());
}

/// The recording among `found` that is the job's: about the same length, by preference a
/// studio recording (on an album, single or EP that isn't live, a compilation or a remix),
/// then the closest in length.  With `search`, only results scoring at least [`MIN_SCORE`].
fn best(job: &Job, found: Option<&Vec<Value>>, search: bool) -> Option<String> {
    let found = found?;
    let len = |r: &Value| r["length"].as_f64().map(|ms| ms / 1000.0);
    let studio = |r: &Value| {
        r["releases"].as_array().is_some_and(|rels| {
            rels.iter().any(|rel| {
                let rg = &rel["release-group"];
                matches!(rg["primary-type"].as_str(), Some("Album" | "Single" | "EP"))
                    && rg["secondary-types"]
                        .as_array()
                        .is_none_or(|t| t.is_empty())
            })
        })
    };
    found
        .iter()
        .filter(|r| r["id"].as_str().is_some())
        .filter(|r| !search || r["score"].as_i64().unwrap_or(0) >= MIN_SCORE)
        .filter(|r| match (job.duration, len(r)) {
            (Some(a), Some(b)) => (a - b).abs() <= LENGTH_SLACK,
            _ => true,
        })
        .min_by(|a, b| {
            let gap = |r: &Value| match (job.duration, len(r)) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => LENGTH_SLACK,
            };
            studio(b).cmp(&studio(a)).then(gap(a).total_cmp(&gap(b)))
        })
        .and_then(|r| r["id"].as_str().map(str::to_string))
}

/// The genres most voted for: at most [`MAX_GENRES`], each with at least [`GENRE_SHARE`] of
/// the top one's votes, in title case ("alternative rock" → "Alternative Rock").
fn top_genres(genres: &Value) -> Vec<String> {
    let Some(list) = genres.as_array() else {
        return Vec::new();
    };
    let mut g: Vec<(i64, &str)> = list
        .iter()
        .filter_map(|x| Some((x["count"].as_i64().unwrap_or(0), x["name"].as_str()?)))
        .collect();
    g.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    let top = g.first().map(|x| x.0).unwrap_or(0);
    g.into_iter()
        .filter(|(n, _)| *n >= 1 && *n as f64 >= GENRE_SHARE * top as f64)
        .take(MAX_GENRES)
        .map(|(_, name)| title_case(name))
        .collect()
}

fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_mbid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Percent-encode a query parameter's value.
fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Escape Lucene's special characters in a search term.
fn lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn genres_are_the_most_voted() {
        let g = json!([
            {"name": "grunge", "count": 68}, {"name": "alternative rock", "count": 30},
            {"name": "rock", "count": 26}, {"name": "noise rock", "count": 12},
            {"name": "pop", "count": 1}
        ]);
        assert_eq!(top_genres(&g), ["Grunge", "Alternative Rock", "Rock"]);
        assert!(top_genres(&json!([])).is_empty());
    }

    #[test]
    fn a_studio_recording_of_the_right_length_is_taken() {
        let job = Job {
            duration: Some(301.0),
            ..Default::default()
        };
        let found = vec![
            json!({"id": "live", "score": 100, "length": 300000,
                   "releases": [{"release-group": {"primary-type": "Album", "secondary-types": ["Live"]}}]}),
            json!({"id": "studio", "score": 100, "length": 303500,
                   "releases": [{"release-group": {"primary-type": "Album", "secondary-types": []}}]}),
            json!({"id": "edit", "score": 100, "length": 250000,
                   "releases": [{"release-group": {"primary-type": "Single"}}]}),
            json!({"id": "weak", "score": 60, "length": 301000,
                   "releases": [{"release-group": {"primary-type": "Album"}}]}),
        ];
        assert_eq!(best(&job, Some(&found), true).as_deref(), Some("studio"));
        // without a studio one, the closest in length
        assert_eq!(
            best(&job, Some(&found[..1].to_vec()), true).as_deref(),
            Some("live")
        );
        assert_eq!(best(&job, Some(&found[2..3].to_vec()), true), None);
    }

    #[test]
    fn search_terms_are_escaped() {
        assert_eq!(lucene("AC/DC: Live!"), "AC\\/DC\\: Live\\!");
        assert!(is_mbid("c159ebba-f822-4a6e-8909-34705d754ac7"));
        assert!(!is_mbid("nope"));
    }
}
