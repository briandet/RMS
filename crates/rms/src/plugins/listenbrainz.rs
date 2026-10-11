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

use crate::player::Player;
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
/// Player pref: percent of a mix to discover (0–40).
pub const ADVENTUROUS: &str = "adventurous";
pub const DEFAULT_ADVENTUROUS: i64 = 15;
pub const MAX_ADVENTUROUS: i64 = 40;
/// ListenBrainz's similarity datasets (they are named by how they were made).
const RECORDINGS_ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";
const ARTISTS_ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30";
const CACHE_SECS: i64 = 30 * 86400;
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

/// The player's Adventurous setting, in percent.
pub fn adventurous(p: &Player) -> i64 {
    if p.prefs.exists(ADVENTUROUS) {
        p.prefs.get_i64(ADVENTUROUS).clamp(0, MAX_ADVENTUROUS)
    } else {
        DEFAULT_ADVENTUROUS
    }
}

/// How many of `n` songs of a mix for `p` to discover.
pub fn share(server: &Server, p: &Player, n: usize) -> usize {
    if !enabled(server) {
        return 0;
    }
    ((n as f64) * adventurous(p) as f64 / 100.0).round() as usize
}

/// Something proposed, before it is found playable.
#[derive(Debug, Clone)]
struct Candidate {
    /// A recording (`Some`), or any track by an artist (`None`).
    recording: Option<String>,
    title: String,
    artist: String,
    artist_mbid: Option<String>,
    because: String,
}

/// Up to `n` songs to discover for a mix from `seeds` (mix ids), none of `exclude` (URLs):
/// library tracks (positive ids) and Spotify tracks (negative ids, `rms_library::remote`).
pub async fn explore(
    server: &Arc<Server>,
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
    let (recs, artists) = tokio::task::spawn_blocking(move || lib.seed_mbids(&ids))
        .await
        .unwrap_or_default();
    if recs.is_empty() && artists.is_empty() {
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
                        because: a.clone(),
                    },
                ));
            }
        }
    }
    // the most similar first, with some chance for the others; similar recordings and
    // similar artists in turn
    let ranked = |mut v: Vec<(f64, Candidate)>| -> Vec<Candidate> {
        let mut rng = rand::rng();
        for c in v.iter_mut() {
            c.0 *= rng.random_range(0.3..1.0);
        }
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        v.into_iter().map(|(_, c)| c).collect()
    };
    let (recs, arts) = (ranked(by_recording), ranked(by_artist));
    let mut queue = Vec::new();
    let mut ri = recs.into_iter();
    let mut ai = arts.into_iter();
    loop {
        match (ri.next(), ai.next()) {
            (None, None) => break,
            (r, a) => queue.extend(r.into_iter().chain(a)),
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
            let _ = tokio::task::spawn_blocking(move || lib.note_discovered(&p)).await;
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
    let local = tokio::task::spawn_blocking(move || {
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
        Some(_) => find_track(sp, &c.title, &c.artist)
            .await
            .into_iter()
            .collect(),
        None => top_tracks(sp, &c.artist).await,
    };
    for t in tracks {
        let Some(remote) = remote_track(&t) else {
            continue;
        };
        if exclude.contains(&remote.url) {
            continue;
        }
        sp.remember(&t);
        let lib = server.library.clone();
        let r = tokio::task::spawn_blocking(move || {
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
    tokio::task::spawn_blocking(move || {
        lib.local_recording(c.recording.as_deref(), &c.artist, &c.title)
            .is_some()
    })
    .await
    .unwrap_or(false)
}

/// A ListenBrainz answer (a JSON array), from the cache when it is fresh.
async fn fetch(server: &Arc<Server>, url: &str) -> Vec<Value> {
    let lib = server.library.clone();
    let key = url.to_string();
    if let Ok(Some(body)) = tokio::task::spawn_blocking(move || lib.cached(&key, CACHE_SECS)).await
    {
        return serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
    }
    let resp = server
        .http
        .get(url)
        .header("User-Agent", super::musicbrainz::user_agent())
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let body = match resp {
        Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
        Ok(r) => {
            debug!("ListenBrainz: HTTP {} for {url}", r.status());
            return Vec::new();
        }
        Err(e) => {
            debug!("ListenBrainz: {e}");
            return Vec::new();
        }
    };
    let list: Vec<Value> = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let lib = server.library.clone();
    let key = url.to_string();
    let _ = tokio::task::spawn_blocking(move || lib.cache(&key, &body)).await;
    list
}

/// Spotify's track for a recording: the first search result by that artist with that title,
/// a live version only when the title asks for one.
async fn find_track(
    sp: &crate::plugins::spotify::Spotify,
    title: &str,
    artist: &str,
) -> Option<Value> {
    let q = format!(
        "track:\"{}\" artist:\"{}\"",
        title.replace('"', ""),
        artist.replace('"', "")
    );
    let v = sp
        .api(&format!(
            "/search?type=track&limit=10&q={}",
            super::musicbrainz::percent(&q)
        ))
        .await
        .ok()?;
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let want_live = norm(title).contains("live");
    v["tracks"]["items"]
        .as_array()?
        .iter()
        .filter(|t| {
            t["artists"].as_array().is_some_and(|a| {
                a.iter()
                    .any(|x| x["name"].as_str().map(norm) == Some(norm(artist)))
            })
        })
        .filter(|t| {
            want_live
                || !t["name"]
                    .as_str()
                    .map(norm)
                    .unwrap_or_default()
                    .contains("live")
        })
        .find(|t| {
            t["name"]
                .as_str()
                .map(norm)
                .is_some_and(|n| n.starts_with(&norm(title)))
        })
        .cloned()
}

/// An artist's most played tracks on Spotify.
async fn top_tracks(sp: &crate::plugins::spotify::Spotify, artist: &str) -> Vec<Value> {
    let q = format!("artist:\"{}\"", artist.replace('"', ""));
    let Ok(v) = sp
        .api(&format!(
            "/search?type=artist&limit=5&q={}",
            super::musicbrainz::percent(&q)
        ))
        .await
    else {
        return Vec::new();
    };
    let Some(id) = v["artists"]["items"].as_array().and_then(|a| {
        a.iter()
            .find(|x| {
                x["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(artist))
            })
            .and_then(|x| x["id"].as_str())
    }) else {
        return Vec::new();
    };
    sp.api(&format!("/artists/{id}/top-tracks?market=from_token"))
        .await
        .ok()
        .and_then(|v| v["tracks"].as_array().cloned())
        .unwrap_or_default()
}

/// A Web API track as a streamed track.
fn remote_track(t: &Value) -> Option<RemoteTrack> {
    let id = t["id"].as_str()?;
    Some(RemoteTrack {
        source: "spotify".into(),
        source_id: id.to_string(),
        url: crate::plugins::spotify::url_for("track", id),
        title: t["name"].as_str()?.to_string(),
        artists: t["artists"]
            .as_array()?
            .iter()
            .filter_map(|a| a["name"].as_str().map(str::to_string))
            .collect(),
        album: t["album"]["name"].as_str().map(str::to_string),
        year: t["album"]["release_date"]
            .as_str()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok()),
        duration: t["duration_ms"].as_f64().map(|ms| ms / 1000.0),
        isrc: t["external_ids"]["isrc"].as_str().map(str::to_string),
        cover: crate::plugins::spotify::best_image(&t["album"]["images"]),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_api_tracks_become_streamed_tracks() {
        let t = json!({"id": "abc", "name": "Song 2", "duration_ms": 121000,
            "artists": [{"name": "Blur"}], "external_ids": {"isrc": "GBAYE9700097"},
            "album": {"name": "Blur", "release_date": "1997-02-10", "images": []}});
        let r = remote_track(&t).unwrap();
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
