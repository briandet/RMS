//! Scrobbling to ListenBrainz: what plays on RMS's players (and in Subsonic apps) is sent as
//! listens to the ListenBrainz accounts added in Settings, each with its user token.
//!
//! * *Now playing* goes out when a song starts; a *listen* once it has really played (paused
//!   time and what a seek skips don't count) for half its length or 4 minutes, whichever is
//!   first, if it is longer than 30 seconds: ListenBrainz's (and Last.fm's) rule.
//! * A synced group scrobbles once, as its master.  Players send to the accounts the
//!   `scrobbleTo` pref names (the same for every player), or else to the accounts marked
//!   default; Subsonic apps to the default accounts.
//! * Library files are scrobbled with the MusicBrainz ids and ISRC from their tags or
//!   identification, so ListenBrainz links them to the right recordings.  Spotify plays only
//!   with the `scrobbleSpotify` pref (ListenBrainz can import them from Spotify itself);
//!   radio and Spotify Connect are not scrobbled.
//! * Listens wait in the library's queue (`rms_library::scrobble`) until ListenBrainz takes
//!   them, so none are lost while it or the network is down.

use crate::server::Server;
use rms_library::scrobble::ListenInfo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use slimproto::MacAddress;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Server pref: the accounts (`[Account]`).
pub const ACCOUNTS: &str = "listenbrainzAccounts";
/// Player pref: the user names of the accounts it scrobbles to.
pub const SCROBBLE_TO: &str = "scrobbleTo";
/// Server pref: scrobble Spotify plays too.
pub const SPOTIFY: &str = "scrobbleSpotify";
const TICK: Duration = Duration::from_secs(2);
/// Listens sent per request.
const BATCH: usize = 100;
/// After a failed send, wait this long (doubling up to [`MAX_BACKOFF`]).
const BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(1800);

static WAKE: Notify = Notify::const_new();

/// A ListenBrainz account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub user: String,
    pub token: String,
    /// Used by players without their own choice, and by Subsonic apps.
    pub default: bool,
}

impl Account {
    fn key(&self) -> String {
        format!("listenbrainz:{}", self.user)
    }
}

pub fn accounts(server: &Server) -> Vec<Account> {
    server
        .prefs
        .server
        .get(ACCOUNTS)
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn save(server: &Server, accounts: &[Account]) {
    server.prefs.server.set(ACCOUNTS, json!(accounts));
}

fn api(server: &Server) -> String {
    server
        .config
        .listenbrainz_api_url
        .trim_end_matches('/')
        .to_string()
}

/// Add (or update) the account a user token belongs to.
pub async fn add_account(server: &Server, token: &str) -> Result<Account, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("no token".into());
    }
    let v: Value = server
        .http
        .get(format!("{}/1/validate-token", api(server)))
        .header("Authorization", format!("Token {token}"))
        .header("User-Agent", super::musicbrainz::user_agent())
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("ListenBrainz: {e}"))?
        .json()
        .await
        .map_err(|e| format!("ListenBrainz: {e}"))?;
    let Some(user) = v["user_name"].as_str().filter(|_| v["valid"] == true) else {
        return Err("ListenBrainz doesn't accept this token".into());
    };
    let mut all = accounts(server);
    let account = Account {
        user: user.to_string(),
        token: token.to_string(),
        default: all
            .iter()
            .find(|a| a.user == user)
            .is_none_or(|a| a.default),
    };
    all.retain(|a| a.user != user);
    all.push(account.clone());
    save(server, &all);
    info!("ListenBrainz: added {user}");
    Ok(account)
}

pub fn remove_account(server: &Server, user: &str) -> bool {
    let mut all = accounts(server);
    let n = all.len();
    all.retain(|a| a.user != user);
    if all.len() == n {
        return false;
    }
    save(server, &all);
    server.library.drop_listens(&format!("listenbrainz:{user}"));
    for p in server.players() {
        let to = p.prefs.get_string_list(SCROBBLE_TO);
        if to.iter().any(|u| u == user) {
            let to: Vec<String> = to.into_iter().filter(|u| u != user).collect();
            p.prefs.set(SCROBBLE_TO, json!(to));
        }
    }
    true
}

pub fn set_default(server: &Server, user: &str, default: bool) -> bool {
    let mut all = accounts(server);
    let Some(a) = all.iter_mut().find(|a| a.user == user) else {
        return false;
    };
    a.default = default;
    save(server, &all);
    true
}

/// The accounts players scrobble to (the same for every player).
pub fn scrobble_to(server: &Server) -> Vec<Account> {
    let all = accounts(server);
    let prefs = &server.prefs.server;
    if prefs.exists(SCROBBLE_TO) {
        let to = prefs.get_string_list(SCROBBLE_TO);
        all.into_iter().filter(|a| to.contains(&a.user)).collect()
    } else {
        all.into_iter().filter(|a| a.default).collect()
    }
}

pub fn default_accounts(server: &Server) -> Vec<Account> {
    accounts(server).into_iter().filter(|a| a.default).collect()
}

/// Listens waiting, over all accounts, and the latest error sending them.
pub fn pending(server: &Server) -> (i64, Option<String>) {
    accounts(server)
        .iter()
        .map(|a| server.library.listens_pending(&a.key()))
        .fold((0, None), |(n, e), (m, f)| (n + m, e.or(f)))
}

// ---------------------------------------------------------------- what is a listen

/// One play of a song on a player, from start to the next song.
#[derive(Debug, Clone, PartialEq)]
struct Play {
    url: String,
    /// When it started (Unix time).
    started: f64,
    duration: Option<f64>,
    /// Seconds really played.
    played: f64,
    last_elapsed: Option<f64>,
    announced: bool,
    scrobbled: bool,
}

impl Play {
    fn new(url: String, started: f64, duration: Option<f64>) -> Play {
        Play {
            url,
            started,
            duration,
            played: 0.0,
            last_elapsed: None,
            announced: false,
            scrobbled: false,
        }
    }

    /// Account for the position now; true when it has just become a listen.
    fn advance(&mut self, elapsed: f64, playing: bool, tick: f64) -> bool {
        if let Some(last) = self.last_elapsed {
            let step = elapsed - last;
            // a seek forward (or a stall reported late) doesn't count beyond a tick
            if playing && step > 0.0 {
                self.played += step.min(tick * 1.5);
            }
        }
        self.last_elapsed = Some(elapsed);
        if self.scrobbled || !counts(self.duration, self.played) {
            return false;
        }
        self.scrobbled = true;
        true
    }
}

/// Whether a song of `duration` seconds played for `played` seconds is a listen.
fn counts(duration: Option<f64>, played: f64) -> bool {
    match duration {
        Some(d) if d > 30.0 => played >= (d / 2.0).min(240.0),
        Some(_) => false,
        // a stream of unknown length: 4 minutes
        None => played >= 240.0,
    }
}

/// Which plays scrobble.
fn scrobbles(server: &Server, url: &str) -> bool {
    if url.starts_with("spotify://track:") {
        return server.prefs.server.get_bool(SPOTIFY);
    }
    !crate::util::is_remote_url(url)
}

/// Follow the players and scrobble what they play, for as long as the server runs.
pub async fn run(server: Arc<Server>) {
    crate::util::spawn(send_loop(server.clone()));
    let mut plays: HashMap<MacAddress, Play> = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        if accounts(&server).is_empty() {
            plays.clear();
            continue;
        }
        let mut seen = Vec::new();
        for p in server.players() {
            let ctl = p.controller();
            let (song, elapsed, playing) = {
                let c = ctl.lock();
                if !c.is_master(p.id) {
                    continue;
                }
                let Some(s) = c.playing_song() else { continue };
                (
                    (s.track.clone(), s.started_at, s.duration()),
                    c.playing_song_elapsed(),
                    c.is_playing(true),
                )
            };
            seen.push(p.id);
            let (track, started, duration) = song;
            let Some(started) = started else { continue };
            let play = plays
                .entry(p.id)
                .or_insert_with(|| Play::new(track.url.clone(), started, duration));
            if play.url != track.url || play.started != started {
                *play = Play::new(track.url.clone(), started, duration);
            }
            if !scrobbles(&server, &track.url) {
                continue;
            }
            let listen = play.advance(elapsed, playing, TICK.as_secs_f64());
            let announce = playing && !play.announced;
            if !(listen || announce) {
                continue;
            }
            play.announced = true;
            let to = scrobble_to(&server);
            if to.is_empty() {
                continue;
            }
            let lib = server.library.clone();
            let url = track.url.clone();
            let info = crate::util::spawn_blocking(move || lib.listen_info(&url))
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| ListenInfo {
                    title: track.title.clone().unwrap_or_default(),
                    artist: track.artist.clone().unwrap_or_default(),
                    album: track.album.clone(),
                    duration,
                    tracknumber: track.tracknum,
                    ..Default::default()
                });
            if info.title.is_empty() || info.artist.is_empty() {
                continue;
            }
            let meta = track_metadata(&info, &track.url);
            if announce {
                now_playing(&server, &to, &meta);
            }
            if listen {
                debug!(
                    "scrobble: \"{}\" by {} on {}",
                    info.title, info.artist, p.id
                );
                queue(&server, &to, started as i64, &meta);
            }
        }
        plays.retain(|id, _| seen.contains(id));
    }
}

/// A listen's `track_metadata`.
fn track_metadata(info: &ListenInfo, url: &str) -> Value {
    let mut extra = serde_json::Map::new();
    let mut put = |k: &str, v: Value| {
        extra.insert(k.into(), v);
    };
    put("media_player", json!("Rusty Music Server"));
    put("media_player_version", json!(env!("CARGO_PKG_VERSION")));
    put("submission_client", json!("Rusty Music Server"));
    put(
        "submission_client_version",
        json!(env!("CARGO_PKG_VERSION")),
    );
    if let Some(m) = &info.recording_mbid {
        put("recording_mbid", json!(m));
    }
    if !info.artist_mbids.is_empty() {
        put("artist_mbids", json!(info.artist_mbids));
    }
    if let Some(m) = &info.release_mbid {
        put("release_mbid", json!(m));
    }
    if let Some(i) = &info.isrc {
        put("isrc", json!(i));
    }
    if let Some(d) = info.duration {
        put("duration_ms", json!((d * 1000.0).round() as i64));
    }
    if let Some(n) = info.tracknumber.filter(|n| *n > 0) {
        put("tracknumber", json!(n));
    }
    if let Some(id) = url.strip_prefix("spotify://track:") {
        put("music_service", json!("spotify.com"));
        put(
            "spotify_id",
            json!(format!("https://open.spotify.com/track/{id}")),
        );
        put(
            "origin_url",
            json!(format!("https://open.spotify.com/track/{id}")),
        );
    }
    let mut m = json!({
        "artist_name": info.artist,
        "track_name": info.title,
        "additional_info": extra,
    });
    if let Some(a) = &info.album {
        m["release_name"] = json!(a);
    }
    m
}

/// Queue a listen for these accounts and wake the sender.
fn queue(server: &Server, to: &[Account], listened_at: i64, meta: &Value) {
    let payload = json!({ "track_metadata": meta }).to_string();
    for a in to {
        if let Err(e) = server.library.queue_listen(&a.key(), listened_at, &payload) {
            warn!("scrobble: queueing a listen: {e}");
        }
    }
    WAKE.notify_one();
}

/// Tell these accounts what plays now (best effort: not queued).
fn now_playing(server: &Server, to: &[Account], meta: &Value) {
    let body = json!({ "listen_type": "playing_now", "payload": [{ "track_metadata": meta }] });
    for a in to {
        let req = submit(server, &a.token, &body);
        crate::util::spawn(async move {
            if let Err(e) = req.await {
                debug!("now playing: {e:?}");
            }
        });
    }
}

/// A listen from a Subsonic app (`submission`), or what it plays now.
pub async fn subsonic(server: &Arc<Server>, url: &str, at: Option<i64>, submission: bool) {
    let to = default_accounts(server);
    if to.is_empty() || !scrobbles(server, url) {
        return;
    }
    let lib = server.library.clone();
    let u = url.to_string();
    let Some(info) = crate::util::spawn_blocking(move || lib.listen_info(&u))
        .await
        .ok()
        .flatten()
    else {
        return;
    };
    let meta = track_metadata(&info, url);
    if submission {
        let at = at.unwrap_or_else(|| crate::util::now() as i64);
        queue(server, &to, at, &meta);
    } else {
        now_playing(server, &to, &meta);
    }
}

// ---------------------------------------------------------------- sending

#[derive(Debug)]
enum SendError {
    /// Worth trying again later.
    Later(String, Option<Duration>),
    /// The listens themselves are refused.
    Refused(String),
}

fn submit(
    server: &Server,
    token: &str,
    body: &Value,
) -> impl std::future::Future<Output = Result<(), SendError>> + Send + 'static {
    let req = server
        .http
        .post(format!("{}/1/submit-listens", api(server)))
        .header("Authorization", format!("Token {token}"))
        .header("User-Agent", super::musicbrainz::user_agent())
        .json(body)
        .timeout(Duration::from_secs(30));
    async move {
        let resp = req
            .send()
            .await
            .map_err(|e| SendError::Later(e.to_string(), None))?;
        let status = resp.status();
        let reset = resp
            .headers()
            .get("X-RateLimit-Reset-In")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {status}"));
        match status.as_u16() {
            400 => Err(SendError::Refused(msg)),
            401 => Err(SendError::Later(format!("token refused: {msg}"), None)),
            429 => Err(SendError::Later("rate limited".into(), reset)),
            _ => Err(SendError::Later(msg, None)),
        }
    }
}

/// Send queued listens as they come, backing off per account when ListenBrainz can't take
/// them.
async fn send_loop(server: Arc<Server>) {
    let mut next_try: HashMap<String, (Instant, Duration)> = HashMap::new();
    loop {
        let _ = tokio::time::timeout(BACKOFF, WAKE.notified()).await;
        for a in accounts(&server) {
            let key = a.key();
            if next_try.get(&key).is_some_and(|(t, _)| *t > Instant::now()) {
                continue;
            }
            loop {
                let lib = server.library.clone();
                let k = key.clone();
                let batch = crate::util::spawn_blocking(move || lib.queued_listens(&k, BATCH))
                    .await
                    .unwrap_or_default();
                if batch.is_empty() {
                    next_try.remove(&key);
                    break;
                }
                match send_batch(&server, &a, &batch).await {
                    Ok(()) => {
                        next_try.remove(&key);
                        if batch.len() < BATCH {
                            break;
                        }
                    }
                    Err((e, wait)) => {
                        let back = next_try.get(&key).map(|(_, b)| *b * 2).unwrap_or(BACKOFF);
                        let back = wait.unwrap_or(back).min(MAX_BACKOFF);
                        warn!("ListenBrainz ({}): {e}; trying again in {back:?}", a.user);
                        next_try.insert(key.clone(), (Instant::now() + back, back));
                        break;
                    }
                }
            }
        }
    }
}

async fn send_batch(
    server: &Arc<Server>,
    a: &Account,
    batch: &[rms_library::scrobble::Queued],
) -> Result<(), (String, Option<Duration>)> {
    let listen = |q: &rms_library::scrobble::Queued| {
        let mut l: Value = serde_json::from_str(&q.payload).unwrap_or_default();
        l["listened_at"] = json!(q.listened_at);
        l
    };
    let ids: Vec<i64> = batch.iter().map(|q| q.id).collect();
    let body = json!({
        "listen_type": if batch.len() == 1 { "single" } else { "import" },
        "payload": batch.iter().map(listen).collect::<Vec<_>>(),
    });
    match submit(server, &a.token, &body).await {
        Ok(()) => {
            debug!("ListenBrainz ({}): {} listens sent", a.user, ids.len());
            server.library.listens_done(&ids);
            Ok(())
        }
        // one bad listen spoils a batch: send them one by one, dropping the refused
        Err(SendError::Refused(_)) if batch.len() > 1 => {
            for q in batch {
                Box::pin(send_batch(server, a, std::slice::from_ref(q))).await?;
            }
            Ok(())
        }
        Err(SendError::Refused(e)) => {
            warn!("ListenBrainz ({}): listen refused, dropped: {e}", a.user);
            server.library.listens_done(&ids);
            Ok(())
        }
        Err(SendError::Later(e, wait)) => {
            server.library.listens_failed(&ids, &e);
            Err((e, wait))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listen_is_half_the_song_or_four_minutes() {
        assert!(!counts(Some(25.0), 25.0), "too short");
        assert!(!counts(Some(200.0), 99.0));
        assert!(counts(Some(200.0), 100.0));
        assert!(!counts(Some(600.0), 239.0));
        assert!(counts(Some(600.0), 240.0), "four minutes");
        assert!(counts(None, 240.0));
    }

    #[test]
    fn only_time_really_played_counts() {
        let mut p = Play::new("file:///a".into(), 0.0, Some(100.0));
        let mut listens = 0;
        // 20 s playing, a seek to 70 s, then on
        for e in [
            0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 70.0,
        ] {
            listens += p.advance(e, true, 2.0) as usize;
        }
        assert!(p.played < 24.0, "the seek isn't played: {}", p.played);
        // paused: the position may still be reported, nothing counts
        for _ in 0..20 {
            listens += p.advance(70.0, false, 2.0) as usize;
        }
        assert_eq!(listens, 0);
        let mut e = 70.0;
        while e < 100.0 {
            e += 2.0;
            listens += p.advance(e, true, 2.0) as usize;
        }
        assert_eq!(listens, 1, "once, at 50 s played");
        assert!(p.scrobbled);
    }

    #[test]
    fn listens_carry_the_musicbrainz_ids() {
        let info = ListenInfo {
            title: "Awake".into(),
            artist: "Tycho".into(),
            album: Some("Awake".into()),
            duration: Some(283.6),
            tracknumber: Some(1),
            recording_mbid: Some("5b44c236-cbb0-4e54-a8b1-c344b76e1b8f".into()),
            isrc: Some("US2J71309901".into()),
            ..Default::default()
        };
        let m = track_metadata(&info, "file:///x.flac");
        assert_eq!(m["track_name"], "Awake");
        assert_eq!(m["release_name"], "Awake");
        let x = &m["additional_info"];
        assert_eq!(x["recording_mbid"], "5b44c236-cbb0-4e54-a8b1-c344b76e1b8f");
        assert_eq!(x["duration_ms"], 283600);
        assert_eq!(x["isrc"], "US2J71309901");
        assert!(x.get("music_service").is_none());
        let s = track_metadata(&info, "spotify://track:abc");
        assert_eq!(s["additional_info"]["music_service"], "spotify.com");
    }
}
