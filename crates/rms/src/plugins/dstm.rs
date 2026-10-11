//! "Don't Stop The Music" (`Slim::Plugin::DontStopTheMusic`) and "Create Mix".
//!
//! When a player's playlist is about to run out (fewer than two songs after the one being
//! streamed, repeat off), the provider chosen for the player
//! (`playerpref plugin.dontstopthemusic:provider`) adds more.  Providers:
//!
//! * the Random Mix ones Lyrion's RandomPlay plugin registers (`Slim::Plugin::RandomPlay::
//!   DontStopTheMusic`): Song Mix, Song Mix limited to the playlist's genres, Random Album
//!   (also limited to the genres), Random Artist, Random Year;
//! * Similar Songs: songs like the last ones of the playlist, by tags and play history
//!   (`rms_library::mix`);
//! * every favorite, when the Favorites plugin registers them (`plugin.favorites:registerDSTM`).
//!
//! "Create Mix" (`createmix`) plays or adds similar songs for a track, album, artist, genre or
//! year, from the track info menus and the web UI.

use crate::control::dispatch::{self, DispatchTable, Handler};
use crate::control::request::{Request, Source};
use crate::notify::Notification;
use crate::player::Player;
use crate::server::Server;
use crate::strings::s;
use parking_lot::Mutex;
use rms_library::mix::Seed;
use serde_json::json;
use slimproto::MacAddress;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// The per-player pref holding the chosen provider (Lyrion's name).
pub const PREF: &str = "plugin.dontstopthemusic:provider";
/// `MIN_TRACKS_LEFT`: add songs when fewer than this many follow the streaming one.
const MIN_TRACKS_LEFT: i64 = 2;
/// Songs added at a time by the song mixes (Random Mix's `newtracks` default).
const NEW_TRACKS: usize = 10;
/// Songs in a "Create Mix".
const MIX_SIZE: usize = 20;
/// Seeds taken from the end of the playlist for Similar Songs.
const SEEDS: usize = 5;

const SIMILAR: &str = "RMS_DSTM_SIMILAR";
const SONG_MIX: &str = "PLUGIN_RANDOM_TRACK";
const SONG_MIX_GENRES: &str = "PLUGIN_RANDOM_TITLEMIX_WITH_GENRES";
const ALBUM: &str = "PLUGIN_RANDOM_ALBUM_ITEM";
const ALBUM_GENRES: &str = "PLUGIN_RANDOM_ALBUM_MIX_WITH_GENRES";
const ARTIST: &str = "PLUGIN_RANDOM_CONTRIBUTOR_ITEM";
const YEAR: &str = "PLUGIN_RANDOM_YEAR_ITEM";
const BUILTIN: &[&str] = &[
    SIMILAR,
    SONG_MIX,
    SONG_MIX_GENRES,
    ALBUM,
    ALBUM_GENRES,
    ARTIST,
    YEAR,
];

/// What a provider adds.
#[derive(Debug, PartialEq)]
enum Addition {
    Tracks(Vec<i64>),
    Url(String),
}

/// The playlist as the providers see it.
struct Snapshot {
    /// Library ids of the local songs, in play order.
    track_ids: Vec<i64>,
    urls: HashSet<String>,
}

/// Provider tokens, sorted by their names (`getSortedHandlerTokens`); favorites are named by
/// their titles.
pub(crate) fn providers(server: &Arc<Server>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = BUILTIN.iter().map(|t| (t.to_string(), s(t))).collect();
    if favorites_registered(server) {
        for (title, _) in server.favorites.flat_audio() {
            if !v.iter().any(|(t, _)| *t == title) {
                v.push((title.clone(), title));
            }
        }
    }
    v.sort_by_key(|(_, name)| name.to_lowercase());
    v
}

fn favorites_registered(server: &Arc<Server>) -> bool {
    server
        .prefs
        .server
        .get("plugin.favorites:registerDSTM")
        .map(|v| v.as_i64() != Some(0) && v.as_str() != Some("0") && v.as_bool() != Some(false))
        .unwrap_or(true)
}

/// The provider chosen for a player ("" / "0" = disabled).
pub(crate) fn provider(p: &Arc<Player>) -> Option<String> {
    let v = p.prefs.get(PREF)?;
    let v = match v {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    (!v.is_empty() && v != "0").then_some(v)
}

/// Ask the provider for songs (`$handler->($client, $cb)`).
async fn mix(server: &Arc<Server>, token: &str, snap: &Snapshot) -> Option<Addition> {
    let lib = server.library.clone();
    let token = token.to_string();
    let ids = snap.track_ids.clone();
    let favorite = server
        .favorites
        .flat_audio()
        .into_iter()
        .find(|(title, _)| *title == token)
        .map(|(_, url)| url);
    if !BUILTIN.contains(&token.as_str()) {
        return favorite.map(Addition::Url);
    }
    tokio::task::spawn_blocking(move || {
        let exclude: HashSet<i64> = ids.iter().copied().collect();
        let genres = || lib.genres_of(&ids);
        let album_tracks = |album: Option<i64>| album.map(|a| lib.seed_tracks(Seed::Album(a)));
        let tracks = match token.as_str() {
            SIMILAR => {
                let seeds: Vec<i64> = ids.iter().rev().take(SEEDS).copied().collect();
                lib.similar_tracks(&seeds, NEW_TRACKS, &exclude)
            }
            SONG_MIX => lib.random_tracks(NEW_TRACKS, &[], None, &exclude),
            SONG_MIX_GENRES => lib.random_tracks(NEW_TRACKS, &genres(), None, &exclude),
            ALBUM => album_tracks(lib.random_album(&[]))?,
            ALBUM_GENRES => album_tracks(lib.random_album(&genres()))?,
            ARTIST => lib.seed_tracks(Seed::Artist(lib.random_artist()?)),
            YEAR => lib.random_tracks(NEW_TRACKS, &[], Some(lib.random_year()?), &exclude),
            _ => return None,
        };
        Some(Addition::Tracks(tracks))
    })
    .await
    .ok()
    .flatten()
}

/// `deDupePlaylist`: nothing that is already in the playlist.
fn dedupe(add: Addition, snap: &Snapshot) -> Option<Addition> {
    match add {
        Addition::Tracks(t) => {
            let have: HashSet<i64> = snap.track_ids.iter().copied().collect();
            let t: Vec<i64> = t.into_iter().filter(|id| !have.contains(id)).collect();
            (!t.is_empty()).then_some(Addition::Tracks(t))
        }
        Addition::Url(u) => (!snap.urls.contains(&u)).then_some(Addition::Url(u)),
    }
}

/// `dontStopTheMusic`: add songs to the sync group's playlist when it is running out.
async fn dont_stop_the_music(server: Arc<Server>, master: Arc<Player>, active: Active) {
    // don't process multiple requests at the same time
    if !active.lock().insert(master.id) {
        return;
    }
    struct Done(Active, MacAddress);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.lock().remove(&self.1);
        }
    }
    let _done = Done(active, master.id);
    let Some(token) = provider(&master) else {
        return;
    };

    let (remaining, count, snap, mixable) = {
        let c = master.controller();
        let c = c.lock();
        let count = c.playlist.len() as i64;
        let index = c.streaming_song_index() as i64;
        let mut track_ids = Vec::new();
        let mut urls = HashSet::new();
        let mut last = None;
        for (_, e) in c.playlist.iter() {
            urls.insert(e.url.clone());
            if let Some(id) = e.track_id {
                track_ids.push(id);
            }
            last = Some(e.clone());
        }
        // don't continue when the playlist ends with a radio station or the like
        let playing_stream = c
            .playing_song()
            .is_some_and(|s| s.is_remote() && s.duration().is_none());
        let mixable = !playing_stream
            && last.is_some_and(|e| e.track_id.is_some() || is_spotify_track(&e.url));
        (
            count - index - 1,
            count,
            Snapshot { track_ids, urls },
            mixable,
        )
    };
    let min = match server
        .prefs
        .server
        .get_i64("plugin.dontstopthemusic:newtracks")
    {
        n if n > 0 => n,
        _ => MIN_TRACKS_LEFT,
    };
    debug!("{}: DSTM: {remaining} songs remaining", master.id);
    if remaining >= min {
        return;
    }
    if !mixable {
        info!(
            "{}: DSTM: radio station or similar last in the queue - no mix",
            master.id
        );
        return;
    }

    let mut add = mix(&server, &token, &snap)
        .await
        .and_then(|a| dedupe(a, &snap));
    if add.is_none() && !token.starts_with("PLUGIN_RANDOM") {
        // "we couldn't create any reasonable result with your current playlist":
        // play something instead
        info!(
            "{}: DSTM: no songs from {token}; adding a song mix",
            master.id
        );
        add = mix(&server, SONG_MIX, &snap)
            .await
            .and_then(|a| dedupe(a, &snap));
    }
    let Some(add) = add else {
        info!("{}: DSTM: no matching songs found", master.id);
        return;
    };
    let items = match &add {
        Addition::Tracks(t) => t.len() as i64,
        Addition::Url(_) => 1,
    };
    info!("{}: DSTM: adding {items} item(s) from {token}", master.id);

    // keep the playlist under maxPlaylistLength: drop songs from its start
    let max = server.prefs.server.get_i64("maxPlaylistLength");
    if max > 0 && count + items > max {
        for _ in 0..items {
            exec(&server, &master, &["playlist", "delete", "0"]).await;
        }
    }
    match add {
        Addition::Tracks(t) => {
            let ids: Vec<String> = t.iter().map(|id| id.to_string()).collect();
            let what = format!("track.id={}", ids.join(","));
            exec(&server, &master, &["playlist", "addtracks", &what]).await;
        }
        Addition::Url(u) => {
            exec(&server, &master, &["playlist", "add", &u]).await;
        }
    }
}

fn is_spotify_track(url: &str) -> bool {
    matches!(crate::plugins::spotify::parse_url(url), Some((kind, _)) if kind == "track")
}

async fn exec(server: &Arc<Server>, p: &Arc<Player>, tokens: &[&str]) -> Request {
    let tokens = tokens.iter().map(|t| t.to_string()).collect();
    dispatch::execute_tokens(server, Some(p.id), tokens, Source::Internal).await
}

type Active = Arc<Mutex<HashSet<MacAddress>>>;

/// `onPlaylistChange`: watch `playlist newsong / delete / cant_open`.
pub async fn run(server: Arc<Server>) {
    let mut rx = server.notifier.subscribe();
    let active: Active = Default::default();
    let mut timers: HashMap<MacAddress, JoinHandle<()>> = HashMap::new();
    loop {
        let n = match rx.recv().await {
            Ok(n) => n,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        if n.first() != "playlist" || !matches!(n.second(), "newsong" | "delete" | "cant_open") {
            continue;
        }
        on_playlist_change(&server, &n, &active, &mut timers);
    }
}

fn on_playlist_change(
    server: &Arc<Server>,
    n: &Notification,
    active: &Active,
    timers: &mut HashMap<MacAddress, JoinHandle<()>>,
) {
    let Some(p) = n.player.and_then(|m| server.player(&m)) else {
        return;
    };
    let ctrl = p.controller();
    let master = {
        let c = ctrl.lock();
        c.master().unwrap_or_else(|| p.clone())
    };
    if provider(&master).is_none() {
        return;
    }
    if let Some(t) = timers.remove(&master.id) {
        t.abort();
    }
    // Spotify sometimes fails to load tracks and skips them without a newsong event:
    // only its "103: not available in your country" errors count
    if n.second() == "cant_open"
        && !(n.word(2).starts_with("spotify") && n.word(3).starts_with("103"))
    {
        return;
    }
    let (repeat, index, wait) = {
        let c = ctrl.lock();
        let left = c.playing_song_duration().unwrap_or(0.0) - c.playing_song_elapsed();
        (c.playlist.repeat, c.streaming_song_index(), left / 2.0)
    };
    if repeat != 0 {
        return;
    }
    let (server, active) = (server.clone(), active.clone());
    let task = tokio::spawn(async move {
        // Playing a single song: wait, we might be in the middle of adding new songs.
        if index == 0 && wait > 0.0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
        }
        dont_stop_the_music(server, master, active).await;
    });
    if index == 0 {
        timers.insert(p.controller().lock().master_id, task);
    }
}

// ---- commands ------------------------------------------------------------------------------

pub fn register(t: &mut DispatchTable) {
    t.add(
        &["dontstopthemusicsetting"],
        1,
        true,
        false,
        Handler::Sync(setting_menu),
    );
    t.add(
        &["createmix", "_cmd"],
        1,
        false,
        true,
        Handler::Async(|s, r| Box::pin(create_mix(s, r))),
    );
}

/// `dontstopthemusicsetting`: the radio list of providers for the Jive settings menu.
fn setting_menu(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = crate::control::commands::player(server, r) else {
        return r.bad_dispatch();
    };
    let master = p.controller().lock().master().unwrap_or(p);
    let current = provider(&master).unwrap_or_default();
    let item = |text: String, token: &str, on: bool| {
        let mut m = serde_json::Map::new();
        m.insert("text".into(), json!(text));
        m.insert("radio".into(), json!(on as i64));
        m.insert(
            "actions".into(),
            json!({ "do": { "player": 0, "cmd": ["playerpref", PREF, token] } }),
        );
        m
    };
    r.add_result("offset", 0);
    r.add_loop_item("item_loop", item(s("DISABLED"), "0", current.is_empty()));
    let all = providers(server);
    for (token, name) in &all {
        r.add_loop_item("item_loop", item(name.clone(), token, *token == current));
    }
    r.add_result("count", all.len() + 1);
    r.set_done();
}

/// `createmix <play|add|insert> track_id:|album_id:|artist_id:|genre_id:|year:` — songs
/// similar to the item (a track's mix starts with the track itself).
async fn create_mix(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = crate::control::commands::player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    use crate::player::command::Enqueue;
    let how = match r.param("_cmd").unwrap_or("play") {
        "add" => Enqueue::Add,
        "insert" => Enqueue::Next,
        _ => Enqueue::Play {
            start: None,
            fade: None,
        },
    };
    let id = |k: &str| r.param(k).and_then(|v| v.parse::<i64>().ok());
    let seed = if let Some(i) = id("track_id") {
        Seed::Track(i)
    } else if let Some(i) = id("album_id") {
        Seed::Album(i)
    } else if let Some(i) = id("artist_id") {
        Seed::Artist(i)
    } else if let Some(i) = id("genre_id") {
        Seed::Genre(i)
    } else if let Some(y) = id("year") {
        Seed::Year(y)
    } else {
        r.bad_params();
        return r;
    };
    let tracks = mix_tracks(&server, seed).await;
    let count = tracks.len();
    if count > 0 {
        use crate::player::command::{entries_for, run, Item, PlayerCommand};
        let entries = entries_for(&server, &Item::Tracks(tracks)).await;
        let _ = run(&server, &p, PlayerCommand::Enqueue { entries, how });
    }
    r.add_result("count", count);
    r.set_done();
    r
}

/// Create Mix: songs similar to `seed` (a track's mix starts with the track itself).
pub async fn mix_tracks(server: &Arc<Server>, seed: Seed) -> Vec<i64> {
    let lib = server.library.clone();
    tokio::task::spawn_blocking(move || {
        let mut seeds = lib.seed_tracks(seed);
        if seeds.is_empty() {
            return Vec::new();
        }
        // a sample of a big album / artist / genre is seed enough
        if seeds.len() > 10 {
            use rand::seq::SliceRandom;
            seeds.shuffle(&mut rand::rng());
            seeds.truncate(10);
        }
        let mut mix = Vec::new();
        if let Seed::Track(t) = seed {
            mix.push(t);
        }
        let none = HashSet::new();
        mix.extend(lib.similar_tracks(&seeds, MIX_SIZE - mix.len(), &none));
        mix
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_drops_what_the_playlist_has() {
        let snap = Snapshot {
            track_ids: vec![1, 2],
            urls: ["http://radio/x".to_string()].into(),
        };
        assert_eq!(
            dedupe(Addition::Tracks(vec![2, 3, 4]), &snap),
            Some(Addition::Tracks(vec![3, 4]))
        );
        assert_eq!(dedupe(Addition::Tracks(vec![1, 2]), &snap), None);
        assert_eq!(dedupe(Addition::Url("http://radio/x".into()), &snap), None);
        assert_eq!(
            dedupe(Addition::Url("http://radio/y".into()), &snap),
            Some(Addition::Url("http://radio/y".into()))
        );
    }
}
