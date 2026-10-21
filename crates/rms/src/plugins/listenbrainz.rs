//! Discovery with ListenBrainz (pref `listenbrainz`, off by default: it sends the MusicBrainz
//! ids of what is played to listenbrainz.org).  A share of every Similar Songs and Create Mix
//! batch — the player's *Adventurous* setting, 15% unless changed — comes from outside what
//! was played: recordings ListenBrainz finds played in the same sessions as the seeds, and
//! tracks by artists it finds similar to the seeds'.  Each plays from the library when the
//! library has it (so music owned but never played turns up too), else from Spotify when
//! signed in.  Its sound isn't known yet, so it scores neutral in the mix; played to the end it
//! is learned like any Spotify track, and skipped within 30 seconds it is rejected and not
//! proposed again (nor, after two rejections, its artist): see `rms_library::discover`.
//!
//! The similarities come from ListenBrainz's public "labs" API (no account needed), cached
//! for a month.

use super::scrobble::Account;
use crate::player::playlist::Entry;
use crate::player::Player;
use crate::plugins::spotify::model::Track;
use crate::server::Server;
use rand::Rng;
use rms_library::discover::Proposal;
use rms_library::remote::RemoteTrack;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

pub const PREF: &str = "listenbrainz";
/// Percent of a mix to discover (0–40); the same for every player.
pub const ADVENTUROUS: &str = "adventurous";
pub const DEFAULT_ADVENTUROUS: i64 = 15;
pub const MAX_ADVENTUROUS: i64 = 40;
/// ListenBrainz's similarity datasets (they are named by how they were made).
const RECORDINGS_ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";
const ARTISTS_ALGORITHM: &str = rms_library::discover::LB_ARTISTS_ALGORITHM;
const CACHE_SECS: i64 = 30 * 86400;
/// Recommendations are renewed every few days, the playlists made for a user daily.
const RECS_SECS: i64 = 86400;
const LIST_SECS: i64 = 3600;
/// Recordings listed under "Recommended for".
const MENU_SIZE: usize = 50;
/// Seeds asked about per mix.
const SEED_RECORDINGS: usize = 5;
const SEED_ARTISTS: usize = 3;
/// Similar artists considered per seed artist.
const ARTISTS_PER_SEED: usize = 10;
/// Discovery gives up after this long and the mix goes on without the rest.
const BUDGET: Duration = Duration::from_secs(10);

pub fn enabled(server: &Server) -> bool {
    server.prefs.server.get_bool(PREF)
}

/// The Adventurous setting, in percent.
pub fn adventurous(server: &Server) -> i64 {
    let prefs = &server.prefs.server;
    if prefs.exists(ADVENTUROUS) {
        prefs.get_i64(ADVENTUROUS).clamp(0, MAX_ADVENTUROUS)
    } else {
        DEFAULT_ADVENTUROUS
    }
}

/// How many of `n` songs of a mix for `p` to discover.
pub fn share(server: &Server, n: usize) -> usize {
    if !enabled(server) {
        return 0;
    }
    ((n as f64) * adventurous(server) as f64 / 100.0).round() as usize
}

/// Something proposed, before it is found playable.
#[derive(Debug, Clone)]
struct Candidate {
    /// A recording (`Some`), or any track by an artist (`None`).
    recording: Option<String>,
    title: String,
    artist: String,
    artist_mbid: Option<String>,
    isrc: Option<String>,
    because: String,
}

/// Up to `n` songs to discover for a mix from `seeds` (mix ids), none of `exclude` (URLs):
/// library tracks (positive ids) and Spotify tracks (negative ids, `rms_library::remote`).
pub async fn explore(
    server: &Arc<Server>,
    player: Option<&Player>,
    seeds: &[i64],
    n: usize,
    exclude: &HashSet<String>,
) -> Vec<i64> {
    if n == 0 || !enabled(server) {
        return Vec::new();
    }
    let deadline = Instant::now() + BUDGET;
    let lib = server.library.clone();
    let ids = seeds.to_vec();
    let (recs, artists) = crate::util::spawn_blocking(move || lib.seed_mbids(&ids))
        .await
        .unwrap_or_default();
    // with a ListenBrainz account: what it recommends for its user, too
    let user = player.and_then(|_| super::scrobble::scrobble_to(server).into_iter().next());
    if recs.is_empty() && artists.is_empty() && user.is_none() {
        debug!("discovery: no MusicBrainz ids for the seeds");
        return Vec::new();
    }
    let base = server
        .config
        .listenbrainz_url
        .trim_end_matches('/')
        .to_string();
    let mut by_recording = Vec::new();
    for r in recs.iter().take(SEED_RECORDINGS) {
        let url = format!(
            "{base}/similar-recordings/json?recording_mbids={r}&algorithm={RECORDINGS_ALGORITHM}"
        );
        for x in fetch(server, &url).await {
            if let (Some(mbid), Some(title), Some(artist)) = (
                x["recording_mbid"].as_str(),
                x["recording_name"].as_str(),
                x["artist_credit_name"].as_str(),
            ) {
                by_recording.push((
                    x["score"].as_f64().unwrap_or(0.0),
                    Candidate {
                        recording: Some(mbid.to_string()),
                        title: title.to_string(),
                        artist: artist.to_string(),
                        artist_mbid: None,
                        isrc: None,
                        because: r.clone(),
                    },
                ));
            }
        }
    }
    let mut by_artist = Vec::new();
    for a in artists.iter().take(SEED_ARTISTS) {
        let url =
            format!("{base}/similar-artists/json?artist_mbids={a}&algorithm={ARTISTS_ALGORITHM}");
        for x in fetch(server, &url).await.into_iter().take(ARTISTS_PER_SEED) {
            if let (Some(mbid), Some(name)) = (x["artist_mbid"].as_str(), x["name"].as_str()) {
                by_artist.push((
                    x["score"].as_f64().unwrap_or(0.0),
                    Candidate {
                        recording: None,
                        title: String::new(),
                        artist: name.to_string(),
                        artist_mbid: Some(mbid.to_string()),
                        isrc: None,
                        because: a.clone(),
                    },
                ));
            }
        }
    }
    let mut personal = Vec::new();
    if let Some(a) = &user {
        let recommended = recommendations(server, a).await;
        // a sample, the most recommended likelier, looked up in one go
        let mut sample: Vec<(f64, String)> = {
            let mut rng = rand::rng();
            recommended
                .into_iter()
                .map(|(score, m)| (score * rng.random_range(0.3..1.0), m))
                .collect()
        };
        sample.sort_by(|a, b| b.0.total_cmp(&a.0));
        sample.truncate(3 * n);
        let ids: Vec<String> = sample.iter().map(|(_, m)| m.clone()).collect();
        let meta = recording_metadata(server, &ids).await;
        for (score, m) in sample {
            if let Some(r) = meta.get(&m) {
                personal.push((
                    score,
                    Candidate {
                        recording: Some(m.clone()),
                        title: r.title.clone(),
                        artist: r.artist.clone(),
                        artist_mbid: r.artist_mbid.clone(),
                        isrc: r.isrc.clone(),
                        because: format!("recommended for {}", a.user),
                    },
                ));
            }
        }
    }
    // the most similar first, with some chance for the others; similar recordings, similar
    // artists and recommendations in turn
    let ranked = |mut v: Vec<(f64, Candidate)>| -> Vec<Candidate> {
        let mut rng = rand::rng();
        for c in v.iter_mut() {
            c.0 *= rng.random_range(0.3..1.0);
        }
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        v.into_iter().map(|(_, c)| c).collect()
    };
    let mut streams = [
        ranked(by_recording).into_iter(),
        ranked(by_artist).into_iter(),
        personal
            .into_iter()
            .map(|(_, c)| c)
            .collect::<Vec<_>>()
            .into_iter(),
    ];
    let mut queue = Vec::new();
    loop {
        let before = queue.len();
        for s in streams.iter_mut() {
            queue.extend(s.next());
        }
        if queue.len() == before {
            break;
        }
    }

    let mut picks: Vec<i64> = Vec::new();
    let mut seen_urls = exclude.clone();
    let mut seen_artists = HashSet::new();
    for c in queue {
        if picks.len() >= n || Instant::now() > deadline {
            break;
        }
        // one song per artist in a batch
        if !seen_artists.insert(c.artist.to_lowercase()) {
            continue;
        }
        if let Some((id, url)) = resolve(server, &c, &seen_urls).await {
            seen_urls.insert(url.clone());
            let lib = server.library.clone();
            let p = Proposal {
                url,
                mbid: c.recording.clone(),
                title: c.title.clone(),
                artist: c.artist.clone(),
                artist_mbid: c.artist_mbid.clone(),
                because: Some(c.because.clone()),
            };
            let _ = crate::util::spawn_blocking(move || lib.note_discovered(&p)).await;
            picks.push(id);
        }
    }
    info!("discovery: {} of {n} songs found", picks.len());
    picks
}

/// A playable, unheard track for a candidate: the library's, else Spotify's.
async fn resolve(
    server: &Arc<Server>,
    c: &Candidate,
    exclude: &HashSet<String>,
) -> Option<(i64, String)> {
    let lib = server.library.clone();
    let cand = c.clone();
    let local = crate::util::spawn_blocking(move || {
        if lib.artist_rejected(&cand.artist) {
            return Err(());
        }
        let ids = match &cand.recording {
            Some(m) => lib
                .local_recording(Some(m), &cand.artist, &cand.title)
                .into_iter()
                .collect(),
            None => lib.unplayed_by_artist(cand.artist_mbid.as_deref(), &cand.artist, 3),
        };
        Ok(ids
            .into_iter()
            .filter_map(|id| lib.track_url(id).map(|u| (id, u)))
            .find(|(_, u)| !lib.heard(u)))
    })
    .await
    .ok()?;
    match local {
        Err(()) => return None,
        Ok(Some((id, url))) if !exclude.contains(&url) => return Some((id, url)),
        // the library has it, but it was played: not a discovery
        Ok(_) if c.recording.is_some() && owned(server, c).await => return None,
        Ok(_) => {}
    }
    let sp = crate::plugins::spotify::get()?;
    if !sp.signed_in() {
        return None;
    }
    let tracks = match &c.recording {
        Some(_) => find_track(sp, &c.title, &c.artist, c.isrc.as_deref())
            .await
            .into_iter()
            .collect(),
        None => top_tracks(sp, &c.artist).await,
    };
    for t in tracks {
        let remote = remote_track(&t);
        if exclude.contains(&remote.url) {
            continue;
        }
        sp.remember(&t);
        let lib = server.library.clone();
        let r = crate::util::spawn_blocking(move || {
            if lib.heard(&remote.url) {
                return None;
            }
            let id = lib.remember_remote(&remote).ok()?;
            Some((rms_library::remote::remote_id(id), remote.url))
        })
        .await
        .ok()
        .flatten();
        if r.is_some() {
            return r;
        }
    }
    None
}

/// Whether the library has the candidate's recording at all (played or not).
async fn owned(server: &Arc<Server>, c: &Candidate) -> bool {
    let lib = server.library.clone();
    let c = c.clone();
    crate::util::spawn_blocking(move || {
        lib.local_recording(c.recording.as_deref(), &c.artist, &c.title)
            .is_some()
    })
    .await
    .unwrap_or(false)
}

/// A ListenBrainz labs answer (a JSON array), from the cache when it is fresh.
async fn fetch(server: &Arc<Server>, url: &str) -> Vec<Value> {
    fetch_json(server, url, CACHE_SECS, None)
        .await
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A ListenBrainz answer, from the cache when no older than `max_age` seconds (`Null` when
/// it can't be had).  With a token, as its user.
async fn fetch_json(server: &Arc<Server>, url: &str, max_age: i64, token: Option<&str>) -> Value {
    let lib = server.library.clone();
    let key = url.to_string();
    if let Ok(Some(body)) = crate::util::spawn_blocking(move || lib.cached(&key, max_age)).await {
        return serde_json::from_str(&body).unwrap_or_default();
    }
    let mut req = server
        .http
        .get(url)
        .header("User-Agent", super::musicbrainz::user_agent())
        .timeout(Duration::from_secs(10));
    if let Some(t) = token {
        req = req.header("Authorization", format!("Token {t}"));
    }
    let body = match req.send().await {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        Ok(r) => {
            debug!("ListenBrainz: HTTP {} for {url}", r.status());
            return Value::Null;
        }
        Err(e) => {
            debug!("ListenBrainz: {e}");
            return Value::Null;
        }
    };
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return Value::Null;
    };
    let lib = server.library.clone();
    let key = url.to_string();
    let _ = crate::util::spawn_blocking(move || lib.cache(&key, &body)).await;
    v
}

/// Spotify's track for a recording: the first search result by that artist with that title,
/// a live version only when the title asks for one.
async fn find_track(
    sp: &crate::plugins::spotify::Spotify,
    title: &str,
    artist: &str,
    isrc: Option<&str>,
) -> Option<Track> {
    // the same recording by its ISRC, when known (and the result has it: a search that
    // doesn't take `isrc:` finds something else)
    if let Some(i) = isrc {
        if let Some(t) = sp
            .search_tracks(&format!("isrc:{i}"), 1)
            .await
            .ok()
            .and_then(|mut t| t.drain(..).next())
            .filter(|t| t.isrc.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(i)))
        {
            return Some(t);
        }
    }
    let q = format!(
        "track:\"{}\" artist:\"{}\"",
        title.replace('"', ""),
        artist.replace('"', "")
    );
    let found = sp.search_tracks(&q, 10).await.ok()?;
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let want_live = norm(title).contains("live");
    found
        .into_iter()
        .filter(|t| t.artists.iter().any(|a| norm(&a.name) == norm(artist)))
        .filter(|t| want_live || !norm(&t.name).contains("live"))
        .find(|t| norm(&t.name).starts_with(&norm(title)))
}

/// An artist's most played tracks on Spotify.
async fn top_tracks(sp: &crate::plugins::spotify::Spotify, artist: &str) -> Vec<Track> {
    let q = format!("artist:\"{}\"", artist.replace('"', ""));
    let Ok(found) = sp.search_artists(&q, 5).await else {
        return Vec::new();
    };
    let Some(a) = found.iter().find(|a| a.name.eq_ignore_ascii_case(artist)) else {
        return Vec::new();
    };
    sp.artist_top_tracks(&a.id).await.unwrap_or_default()
}

/// A Spotify track as a streamed track.
fn remote_track(t: &Track) -> RemoteTrack {
    RemoteTrack {
        source: "spotify".into(),
        source_id: t.id.clone(),
        url: t.url(),
        title: t.name.clone(),
        artists: t.artists.iter().map(|a| a.name.clone()).collect(),
        album: Some(t.album.clone()).filter(|a| !a.is_empty()),
        year: t.year.map(i64::from),
        duration: Some(f64::from(t.duration_ms) / 1000.0),
        isrc: t.isrc.clone(),
        cover: t.cover.clone(),
    }
}

/// Put `discovered` among `mix` at random places (the first song of a track's mix stays
/// first).
pub fn blend(mut mix: Vec<i64>, discovered: Vec<i64>, keep_first: bool) -> Vec<i64> {
    let mut rng = rand::rng();
    let from = usize::from(keep_first && !mix.is_empty());
    for d in discovered {
        let at = rng.random_range(from..=mix.len());
        mix.insert(at, d);
    }
    mix
}

/// Artists ListenBrainz finds similar to one (by MusicBrainz id): (name, MusicBrainz id),
/// most similar first.  Only with discovery on or a ListenBrainz account: it tells
/// ListenBrainz what is being looked at.
pub async fn similar_artists(server: &Arc<Server>, mbid: &str) -> Vec<(String, String)> {
    if !enabled(server) && super::scrobble::accounts(server).is_empty() {
        return Vec::new();
    }
    let base = server
        .config
        .listenbrainz_url
        .trim_end_matches('/')
        .to_string();
    let url =
        format!("{base}/similar-artists/json?artist_mbids={mbid}&algorithm={ARTISTS_ALGORITHM}");
    fetch(server, &url)
        .await
        .into_iter()
        .filter_map(|x| {
            Some((
                x["name"].as_str()?.to_string(),
                x["artist_mbid"].as_str()?.to_string(),
            ))
        })
        .take(12)
        .collect()
}

// ---------------------------------------------------------------- with an account

/// What ListenBrainz recommends for an account's user: (score, recording) pairs.  (Its
/// collaborative filtering, renewed every few days.)
async fn recommendations(server: &Arc<Server>, a: &Account) -> Vec<(f64, String)> {
    let url = format!(
        "{}/1/cf/recommendation/user/{}/recording?count=200",
        api(server),
        super::musicbrainz::percent(&a.user)
    );
    let v = fetch_json(server, &url, RECS_SECS, Some(&a.token)).await;
    v["payload"]["mbids"]
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|x| {
                    Some((
                        x["score"].as_f64().unwrap_or(0.0),
                        x["recording_mbid"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a recommendation or playlist names: a recording.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recording {
    pub mbid: String,
    pub title: String,
    pub artist: String,
    pub artist_mbid: Option<String>,
    pub isrc: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
}

/// Recordings' titles, artists and ISRCs, by MusicBrainz id (one request for those not
/// cached).
async fn recording_metadata(
    server: &Arc<Server>,
    mbids: &[String],
) -> std::collections::HashMap<String, Recording> {
    let mut out = std::collections::HashMap::new();
    let mut missing = Vec::new();
    for m in mbids {
        let lib = server.library.clone();
        let key = format!("lb-recording:{m}");
        let cached = crate::util::spawn_blocking(move || lib.cached(&key, CACHE_SECS))
            .await
            .ok()
            .flatten();
        match cached.and_then(|b| serde_json::from_str::<Value>(&b).ok()) {
            Some(v) => {
                if let Some(r) = recording_from(m, &v) {
                    out.insert(m.clone(), r);
                }
            }
            None => missing.push(m.clone()),
        }
    }
    for chunk in missing.chunks(50) {
        let url = format!(
            "{}/1/metadata/recording/?recording_mbids={}&inc=artist",
            api(server),
            chunk.join(",")
        );
        let v = fetch_json(server, &url, 0, None).await;
        for m in chunk {
            let Some(x) = v.get(m) else { continue };
            let lib = server.library.clone();
            let (key, body) = (format!("lb-recording:{m}"), x.to_string());
            let _ = crate::util::spawn_blocking(move || lib.cache(&key, &body)).await;
            if let Some(r) = recording_from(m, x) {
                out.insert(m.clone(), r);
            }
        }
    }
    out
}

fn recording_from(mbid: &str, v: &Value) -> Option<Recording> {
    Some(Recording {
        mbid: mbid.to_string(),
        title: v["recording"]["name"].as_str()?.to_string(),
        artist: v["artist"]["name"].as_str()?.to_string(),
        artist_mbid: v["artist"]["artists"][0]["artist_mbid"]
            .as_str()
            .map(str::to_string),
        isrc: v["recording"]["isrcs"][0].as_str().map(str::to_string),
        duration: v["recording"]["length"].as_f64().map(|ms| ms / 1000.0),
    })
}

fn api(server: &Server) -> String {
    server
        .config
        .listenbrainz_api_url
        .trim_end_matches('/')
        .to_string()
}

/// An entry of the ListenBrainz menu.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub name: String,
    pub folder: bool,
}

/// The ListenBrainz menu: at the top, for each account, its recommendations and the
/// playlists ListenBrainz made for its user (Daily Jams, Weekly Jams, Weekly Exploration, …);
/// below, their recordings (`rec:<mbid>`).
pub async fn menu(server: &Arc<Server>, id: Option<&str>) -> Result<Vec<MenuItem>, String> {
    let accounts = super::scrobble::accounts(server);
    let Some(id) = id.filter(|i| !i.is_empty()) else {
        let mut items = Vec::new();
        for a in &accounts {
            items.push(MenuItem {
                id: format!("recs:{}", a.user),
                name: format!("Recommended for {}", a.user),
                folder: true,
            });
            for (pid, title) in playlists(server, a).await {
                items.push(MenuItem {
                    id: format!("pl:{pid}"),
                    name: title,
                    folder: true,
                });
            }
        }
        return Ok(items);
    };
    let recordings = recordings_of(server, id).await?;
    Ok(recordings
        .into_iter()
        .map(|r| MenuItem {
            id: format!("rec:{}", r.mbid),
            name: format!("{} — {}", r.title, r.artist),
            folder: false,
        })
        .collect())
}

/// The playlists ListenBrainz made for an account's user, newest first: (id, title).
async fn playlists(server: &Arc<Server>, a: &Account) -> Vec<(String, String)> {
    let url = format!(
        "{}/1/user/{}/playlists/createdfor?count=25",
        api(server),
        super::musicbrainz::percent(&a.user)
    );
    let v = fetch_json(server, &url, LIST_SECS, Some(&a.token)).await;
    v["playlists"]
        .as_array()
        .map(|l| {
            l.iter()
                .filter_map(|x| {
                    let p = &x["playlist"];
                    let id = p["identifier"].as_str()?.rsplit('/').next()?.to_string();
                    Some((id, p["title"].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The recordings a menu id stands for.
async fn recordings_of(server: &Arc<Server>, id: &str) -> Result<Vec<Recording>, String> {
    let accounts = super::scrobble::accounts(server);
    if let Some(user) = id.strip_prefix("recs:") {
        let a = accounts
            .iter()
            .find(|a| a.user == user)
            .ok_or("no such ListenBrainz account")?;
        let ids: Vec<String> = recommendations(server, a)
            .await
            .into_iter()
            .take(MENU_SIZE)
            .map(|(_, m)| m)
            .collect();
        let meta = recording_metadata(server, &ids).await;
        return Ok(ids.iter().filter_map(|m| meta.get(m).cloned()).collect());
    }
    if let Some(pid) = id.strip_prefix("pl:") {
        if !is_mbid(pid) {
            return Err("no such playlist".into());
        }
        let url = format!("{}/1/playlist/{pid}", api(server));
        let token = accounts.first().map(|a| a.token.as_str());
        let v = fetch_json(server, &url, LIST_SECS, token).await;
        let tracks = v["playlist"]["track"]
            .as_array()
            .ok_or("ListenBrainz has no such playlist")?;
        return Ok(tracks
            .iter()
            .filter_map(|t| {
                let mbid = t["identifier"]
                    .as_array()
                    .and_then(|i| i.first())
                    .or(Some(&t["identifier"]))?
                    .as_str()?
                    .rsplit('/')
                    .next()?
                    .to_string();
                Some(Recording {
                    title: t["title"].as_str()?.to_string(),
                    artist: t["creator"].as_str()?.to_string(),
                    duration: t["duration"].as_f64().map(|ms| ms / 1000.0),
                    artist_mbid: t["extension"]["https://musicbrainz.org/doc/jspf#track"]
                        ["additional_metadata"]["artists"][0]["artist_mbid"]
                        .as_str()
                        .map(str::to_string),
                    isrc: None,
                    mbid,
                })
            })
            .collect());
    }
    if let Some(m) = id.strip_prefix("rec:") {
        if !is_mbid(m) {
            return Err("no such recording".into());
        }
        let meta = recording_metadata(server, &[m.to_string()]).await;
        return meta
            .get(m)
            .cloned()
            .map(|r| vec![r])
            .ok_or_else(|| "ListenBrainz doesn't know this recording".into());
    }
    Err(format!("unknown ListenBrainz item {id}"))
}

/// What a menu id plays, as queue entries: each recording from the library when it has it,
/// else from Spotify when signed in (in the menu's order; what can't be found is left out).
pub async fn entries(server: &Arc<Server>, id: &str) -> Result<Vec<Entry>, String> {
    use futures::StreamExt;
    let mut recordings = recordings_of(server, id).await?;
    // ISRCs make Spotify's copy certain
    let need: Vec<String> = recordings
        .iter()
        .filter(|r| r.isrc.is_none())
        .map(|r| r.mbid.clone())
        .collect();
    if !need.is_empty() && crate::plugins::spotify::get().is_some_and(|s| s.signed_in()) {
        let meta = recording_metadata(server, &need).await;
        for r in recordings.iter_mut() {
            if let Some(m) = meta.get(&r.mbid) {
                r.isrc = r.isrc.take().or(m.isrc.clone());
            }
        }
    }
    let found: Vec<Option<Item>> = futures::stream::iter(recordings)
        .map(|r| {
            let server = server.clone();
            async move { playable(&server, &r).await }
        })
        .buffered(4)
        .collect()
        .await;
    let mut out = Vec::new();
    for item in found.into_iter().flatten() {
        out.extend(match item {
            Item::Local(id) => {
                crate::player::command::entries_for(
                    server,
                    &crate::player::command::Item::Track(id),
                )
                .await
            }
            Item::Url(u) => crate::player::command::url_entries(server, &u).await,
        });
    }
    if out.is_empty() {
        return Err("none of it is in the library, or on Spotify".into());
    }
    Ok(out)
}

enum Item {
    Local(i64),
    Url(String),
}

/// Where a recording plays from (remembered for a week; a miss for a day).
async fn playable(server: &Arc<Server>, r: &Recording) -> Option<Item> {
    let lib = server.library.clone();
    let rec = r.clone();
    let local = crate::util::spawn_blocking(move || {
        lib.local_recording(Some(&rec.mbid), &rec.artist, &rec.title)
    })
    .await
    .ok()
    .flatten();
    if let Some(id) = local {
        return Some(Item::Local(id));
    }
    let key = format!("lb-spotify:{}", r.mbid);
    let lib = server.library.clone();
    let k = key.clone();
    if let Ok(Some(hit)) = crate::util::spawn_blocking(move || lib.cached(&k, 7 * 86400)).await {
        return (!hit.is_empty()).then_some(Item::Url(hit));
    }
    let sp = crate::plugins::spotify::get().filter(|s| s.signed_in())?;
    let url = match find_track(sp, &r.title, &r.artist, r.isrc.as_deref()).await {
        Some(t) => {
            sp.remember(&t);
            Some(t.url())
        }
        None => None,
    };
    let lib = server.library.clone();
    let body = url.clone().unwrap_or_default();
    let _ = crate::util::spawn_blocking(move || lib.cache(&key, &body)).await;
    url.map(Item::Url)
}

fn is_mbid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_tracks_become_streamed_tracks() {
        let t = Track {
            id: "abc".into(),
            name: "Song 2".into(),
            artists: vec![crate::plugins::spotify::model::ArtistRef {
                id: "b".into(),
                name: "Blur".into(),
            }],
            album: "Blur".into(),
            year: Some(1997),
            duration_ms: 121000,
            isrc: Some("GBAYE9700097".into()),
            ..Default::default()
        };
        let r = remote_track(&t);
        assert_eq!(r.url, "spotify://track:abc");
        assert_eq!(r.year, Some(1997));
        assert_eq!(r.isrc.as_deref(), Some("GBAYE9700097"));
        assert_eq!(r.duration, Some(121.0));
    }

    #[test]
    fn discoveries_are_blended_in() {
        let m = blend(vec![1, 2, 3], vec![-7, 9], true);
        assert_eq!(m.len(), 5);
        assert_eq!(m[0], 1);
        assert!(m.contains(&-7) && m.contains(&9));
        assert_eq!(blend(vec![], vec![4], true), vec![4]);
    }
}
