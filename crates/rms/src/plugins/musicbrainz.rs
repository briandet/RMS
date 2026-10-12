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
//!
//! Once the recordings are looked up, each album is matched to the release it is: the one its
//! tags name, else the best fit among the releases holding its recordings (or, without
//! recording ids, found by title and artist), scored by `rms_library::releases`.

use crate::server::Server;
use rms_library::musicbrainz::{Found, Job};
use rms_library::releases::{self, AlbumJob, Release, Scored};
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
/// Albums matched per round.
const ALBUM_BATCH: usize = 5;
/// Releases fetched and scored per album, at most.
const MAX_CANDIDATES: usize = 5;
/// A release search result must score this (0–100).
const MIN_RELEASE_SCORE: i64 = 80;
/// Releases are fetched again after this long (seconds; corrections on MusicBrainz).
const RELEASE_MAX_AGE: i64 = 30 * 86400;

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
        client.key = server.prefs.server.get_str(KEY_PREF).trim().to_string();
        if jobs.is_empty() {
            // recordings done: their albums, then the covers of those without art
            if !match_albums(&server, &mut client).await && !fetch_release_art(&server).await {
                tokio::time::sleep(IDLE).await;
            }
            continue;
        }
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

/// Match a batch of albums to their releases.  `false` when there was nothing to match (or
/// MusicBrainz can't be asked).
async fn match_albums(server: &Arc<Server>, client: &mut Client) -> bool {
    let lib = server.library.clone();
    let jobs = tokio::task::spawn_blocking(move || lib.release_pending(ALBUM_BATCH))
        .await
        .unwrap_or_default();
    if jobs.is_empty() {
        return false;
    }
    let mut matched = 0;
    for job in &jobs {
        if !enabled(server) {
            break;
        }
        let m = match client.match_album(&server.library, job).await {
            Ok(m) => m,
            Err(e) => {
                warn!("MusicBrainz: {e}");
                return false;
            }
        };
        if m.best.is_some() {
            matched += 1;
        }
        debug!(
            "MusicBrainz: album \"{}\" by {}: {}",
            job.title,
            job.artist,
            match &m.best {
                Some((r, s, how)) => format!("{} \"{}\" ({:.2}, {how})", r.mbid, r.title, s.score),
                None => "no release".into(),
            }
        );
        if let Err(e) = m.store(&server.library, job) {
            warn!("MusicBrainz: storing album {}: {e}", job.album_id);
        }
    }
    let lib = server.library.clone();
    let changed = tokio::task::spawn_blocking(move || lib.apply_musicbrainz())
        .await
        .unwrap_or(0);
    info!(
        "MusicBrainz: matched {matched} of {} albums to releases; {changed} tracks and albums filled in",
        jobs.len()
    );
    true
}

/// Front covers from the Cover Art Archive (1200 pixels) for a batch of confidently matched
/// albums without art.  `false` when there was nothing to fetch (or the archive can't be
/// asked).
async fn fetch_release_art(server: &Arc<Server>) -> bool {
    let lib = server.library.clone();
    let jobs = tokio::task::spawn_blocking(move || lib.release_art_pending(ALBUM_BATCH))
        .await
        .unwrap_or_default();
    if jobs.is_empty() {
        return false;
    }
    let mut found = 0;
    for job in &jobs {
        if !enabled(server) {
            break;
        }
        let images =
            match super::coverart::images(server, &job.release_mbid, job.release_group.as_deref())
                .await
            {
                Ok(i) => i,
                Err(e) => {
                    warn!("{e}");
                    return false;
                }
            };
        // the front cover only: another image isn't the album's picture
        let front = images
            .iter()
            .find(|i| i.front || i.types.iter().any(|t| t == "Front"));
        let image = match front {
            Some(i) => match super::coverart::download(server, i).await {
                Ok(c) => Some(c),
                Err(e) => {
                    warn!("cover art for {}: {e}", job.release_mbid);
                    None
                }
            },
            None => None,
        };
        found += image.is_some() as usize;
        let res = server.library.store_release_art(
            &job.release_mbid,
            image.as_ref().map(|(d, m)| (d.as_slice(), m.as_str())),
        );
        if let Err(e) = res {
            warn!("cover art for {}: {e}", job.release_mbid);
        }
        // (the archive asks for no limit; one album a second all the same)
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let lib = server.library.clone();
    let changed = tokio::task::spawn_blocking(move || lib.apply_release_art())
        .await
        .unwrap_or(0);
    info!(
        "Cover Art Archive: front covers for {found} of {} releases; {changed} tracks have one now",
        jobs.len()
    );
    if changed > 0 {
        server.notifier.notify(None, &["library", "changed", "1"]);
    }
    true
}

/// Match the album to the release the user chose (`Some`), or forget its match so it is
/// matched again (`None`).
pub async fn choose_release(
    server: &Arc<Server>,
    album_id: i64,
    mbid: Option<&str>,
) -> Result<(), ChooseError> {
    let lib = server.library.clone();
    let Some(mut job) = tokio::task::spawn_blocking(move || lib.album_job(album_id))
        .await
        .ok()
        .flatten()
    else {
        return Err(ChooseError::NotFound);
    };
    let Some(mbid) = mbid else {
        server
            .library
            .forget_album_match(album_id)
            .map_err(|e| ChooseError::Failed(e.to_string()))?;
        return Ok(());
    };
    if !is_mbid(mbid) {
        return Err(ChooseError::Invalid(format!(
            "not a MusicBrainz id: {mbid}"
        )));
    }
    job.chosen = Some(mbid.to_string());
    let mut client = Client::new(server);
    let mut m = client
        .match_album(&server.library, &job)
        .await
        .map_err(ChooseError::Failed)?;
    if m.best.is_none() {
        return Err(ChooseError::Invalid(format!("no such release: {mbid}")));
    }
    // keep the other releases considered before
    if let Some(before) = server.library.album_match(album_id) {
        for (id, score, _) in before.candidates {
            if !m.candidates.iter().any(|(c, _)| *c == id) {
                m.candidates.push((id, score));
            }
        }
    }
    m.store(&server.library, &job)
        .map_err(|e| ChooseError::Failed(e.to_string()))?;
    let lib = server.library.clone();
    tokio::task::spawn_blocking(move || lib.apply_musicbrainz())
        .await
        .ok();
    Ok(())
}

#[derive(Debug)]
pub enum ChooseError {
    NotFound,
    Invalid(String),
    Failed(String),
}

/// What matching an album found: the release (how it scored, and `tag`, `auto` or `user`), and
/// every release considered.
#[derive(Debug)]
pub struct AlbumMatchResult {
    pub best: Option<(Release, Scored, &'static str)>,
    pub candidates: Vec<(String, f64)>,
}

impl AlbumMatchResult {
    fn store(&self, lib: &rms_library::Library, job: &AlbumJob) -> anyhow::Result<()> {
        lib.store_album_match(
            job,
            self.best.as_ref().map(|(r, s, _)| (r, s)),
            self.best.as_ref().map(|b| b.2).unwrap_or("none"),
            &self.candidates,
        )
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

    /// Find the album's release: the one its tags name, else among the releases holding its
    /// recordings (or found by title and artist), the one its tracks fit best.  With
    /// `job.chosen`, only that one.  `Err` only when the service can't be asked.
    pub async fn match_album(
        &mut self,
        lib: &rms_library::Library,
        job: &AlbumJob,
    ) -> Result<AlbumMatchResult, String> {
        let mut ids: Vec<String> = Vec::new();
        if let Some(c) = &job.chosen {
            ids.push(c.clone());
        } else {
            if let Some(t) = job.release_tag.as_deref().filter(|t| is_mbid(t)) {
                ids.push(t.to_string());
            }
            for id in self.candidates(job).await? {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        let mut scored: Vec<(Release, Scored)> = Vec::new();
        for id in ids.iter().take(MAX_CANDIDATES + 1) {
            if let Some(r) = self.release(lib, id).await? {
                let s = releases::score(job, &r);
                scored.push((r, s));
            }
        }
        let candidates = scored
            .iter()
            .map(|(r, s)| (r.mbid.clone(), s.score))
            .collect();
        let pick = |how: &'static str, i: usize, scored: &mut Vec<(Release, Scored)>| {
            let (r, s) = scored.swap_remove(i);
            Some((r, s, how))
        };
        let best = if job.chosen.is_some() {
            if scored.is_empty() {
                None
            } else {
                pick("user", 0, &mut scored)
            }
        } else if let Some(i) = scored.iter().position(|(r, s)| {
            job.release_tag.as_deref() == Some(r.mbid.as_str()) && s.score >= releases::MIN_SCORE
        }) {
            pick("tag", i, &mut scored)
        } else {
            // the best fit; of equal ones (the same tracks, reissued) the earliest
            let date = |r: &Release| (r.date.is_empty(), r.date.clone());
            let i = (0..scored.len())
                .filter(|&i| scored[i].1.score >= releases::MIN_SCORE)
                .min_by(|&a, &b| {
                    let ((ra, sa), (rb, sb)) = (&scored[a], &scored[b]);
                    sb.score
                        .total_cmp(&sa.score)
                        .then_with(|| date(ra).cmp(&date(rb)))
                });
            match i {
                Some(i) => pick("auto", i, &mut scored),
                None => None,
            }
        };
        Ok(AlbumMatchResult { best, candidates })
    }

    /// Releases that might be the album's, likeliest first: those holding several of its
    /// recordings with about its number of tracks; without recording ids, a search by title
    /// and artist.
    async fn candidates(&mut self, job: &AlbumJob) -> Result<Vec<String>, String> {
        let n = job.tracks.len() as i64;
        // id → (recordings held, track count, official)
        let mut found: Vec<(String, usize, i64, bool)> = Vec::new();
        for rec in job.recordings() {
            let v = self
                .mb_get(&format!(
                    "/ws/2/release?recording={rec}&inc=media&limit=100&fmt=json"
                ))
                .await?;
            for r in v["releases"].as_array().into_iter().flatten() {
                let Some(id) = r["id"].as_str() else { continue };
                match found.iter_mut().find(|f| f.0 == id) {
                    Some(f) => f.1 += 1,
                    None => found.push((id.to_string(), 1, track_count(r), official(r))),
                }
            }
        }
        // a popular song is on more releases than one page lists, so its album may be missing:
        // unless a release holds two of the album's recordings, search by title and artist too
        let recordings = job.recordings().len();
        let held = found.iter().map(|f| f.1).max().unwrap_or(0);
        if (found.is_empty() || (recordings >= 2 && held < 2)) && !job.title.is_empty() {
            let mut q = format!("release:\"{}\"", lucene(&job.title));
            if !job.artist.is_empty() && !job.compilation {
                q.push_str(&format!(" AND artist:\"{}\"", lucene(&job.artist)));
            }
            let v = self
                .mb_get(&format!(
                    "/ws/2/release?query={}&limit=25&fmt=json",
                    percent(&q)
                ))
                .await?;
            for r in v["releases"].as_array().into_iter().flatten() {
                if r["score"].as_i64().unwrap_or(0) < MIN_RELEASE_SCORE {
                    continue;
                }
                let Some(id) = r["id"].as_str() else { continue };
                // as likely as a release holding one recording; found both ways, likelier
                match found.iter_mut().find(|f| f.0 == id) {
                    Some(f) => f.1 += 1,
                    None => found.push((id.to_string(), 1, track_count(r), official(r))),
                }
            }
        }
        // a release can't hold more of the album than it has tracks; one with fewer tracks
        // than the album is still possible (a file too many), just less likely
        found.sort_by_key(|(_, held, tracks, official)| {
            (
                std::cmp::Reverse(*held),
                (tracks - n).abs() + if *tracks < n { n } else { 0 },
                !official,
            )
        });
        Ok(found
            .into_iter()
            .take(MAX_CANDIDATES)
            .map(|f| f.0)
            .collect())
    }

    /// A release with its tracks, from the library's cache or MusicBrainz.
    async fn release(
        &mut self,
        lib: &rms_library::Library,
        id: &str,
    ) -> Result<Option<Release>, String> {
        if let Some(r) = lib.cached_release(id, RELEASE_MAX_AGE) {
            return Ok(Some(r));
        }
        let v = self
            .mb_get(&format!(
                "/ws/2/release/{id}?inc=recordings+artist-credits+release-groups+labels&fmt=json"
            ))
            .await?;
        let Some(r) = Release::from_json(&v) else {
            return Ok(None);
        };
        if let Err(e) = lib.store_release(&r) {
            warn!("MusicBrainz: storing release {id}: {e}");
        }
        Ok(Some(r))
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
pub(crate) fn user_agent() -> String {
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

/// Tracks on a release, as MusicBrainz lists releases.
fn track_count(r: &Value) -> i64 {
    r["track-count"].as_i64().unwrap_or_else(|| {
        r["media"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["track-count"].as_i64())
            .sum()
    })
}

fn official(r: &Value) -> bool {
    r["status"].as_str() == Some("Official")
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
pub(crate) fn percent(s: &str) -> String {
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
