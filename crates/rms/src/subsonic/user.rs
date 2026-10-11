//! What users do: playlists, stars (RMS favorites), ratings, scrobbles, scans, the user
//! itself, and the jukebox (a Squeezebox playing what the app chooses).

use super::browse::{parse_id, require_id, song, Ctx};
use super::{Error, Params, Reply, Result};
use crate::api::commands::{self, exec_for, CommandError};
use crate::api::model::{Command, EnqueueMode, Item, PlayMode, Shuffle};
use crate::player::Player;
use crate::server::Server;
use serde_json::{json, Value};
use std::sync::Arc;

// ---------------------------------------------------------------- playlists

fn playlist_value(
    server: &Server,
    id: i64,
    with_entries: bool,
    ctx: &Ctx,
    owner: &str,
) -> Option<Value> {
    let (name, url, urls) = server.library.playlist_by_id(id)?;
    let tracks: Vec<rms_library::Track> = urls
        .iter()
        .filter_map(|u| server.library.track_by_url(u))
        .collect();
    let duration: f64 = tracks.iter().filter_map(|t| t.secs).sum();
    let changed = crate::util::path_from_file_url(&url)
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .map(chrono::DateTime::<chrono::Utc>::from)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut v = json!({
        "id": format!("pl-{id}"), "name": name, "owner": owner, "public": false,
        "songCount": tracks.len(), "duration": duration.round() as i64,
        "created": changed, "changed": changed,
    });
    if let Some(c) = tracks.iter().find_map(super::browse::cover_art_of_track) {
        v["coverArt"] = json!(c);
    }
    if with_entries {
        v["entry"] = Value::Array(tracks.iter().map(|t| song(t, ctx)).collect());
    }
    Some(v)
}

fn saved_playlist_ids(server: &Server) -> Vec<i64> {
    let q = rms_library::LibQuery::new(rms_library::LibQueryName::Playlists, 0, 100_000);
    server
        .library
        .query(&q)
        .ok()
        .and_then(|r| r.get("playlists_loop").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p["id"].as_i64())
        .collect()
}

pub fn playlists(server: &Server) -> Result<Reply> {
    let ctx = Ctx::new(server);
    let owner = super::ANY_USER;
    let list: Vec<Value> = saved_playlist_ids(server)
        .into_iter()
        .filter_map(|id| playlist_value(server, id, false, &ctx, owner))
        .collect();
    Ok(Reply::one("playlists", json!({ "playlist": list })))
}

pub fn playlist(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "id", "pl", "Playlist")?;
    let ctx = Ctx::new(server);
    let v = playlist_value(server, id, true, &ctx, super::ANY_USER)
        .ok_or_else(|| Error::not_found("Playlist"))?;
    Ok(Reply::one("playlist", v))
}

fn song_urls(server: &Server, ids: &[&str]) -> Result<Vec<String>> {
    ids.iter()
        .map(|raw| {
            parse_id(raw, "tr")
                .and_then(|id| server.library.track(id))
                .map(|t| t.url)
                .ok_or_else(|| Error::not_found("Song"))
        })
        .collect()
}

fn failed(e: anyhow::Error) -> Error {
    Error::new(0, e.to_string())
}

pub fn create_playlist(server: &Server, p: &Params) -> Result<Reply> {
    let urls = song_urls(server, &p.all("songId"))?;
    let id = match p.get("playlistId") {
        Some(raw) => {
            let id = parse_id(raw, "pl").ok_or_else(|| Error::not_found("Playlist"))?;
            let (name, _, _) = server
                .library
                .playlist_by_id(id)
                .ok_or_else(|| Error::not_found("Playlist"))?;
            server.library.save_playlist(&name, &urls).map_err(failed)?
        }
        None => {
            let name = p.require("name")?.trim();
            if name.is_empty() {
                return Err(Error::new(10, "Required parameter is missing: name"));
            }
            server.library.save_playlist(name, &urls).map_err(failed)?
        }
    };
    let ctx = Ctx::new(server);
    let v = playlist_value(server, id, true, &ctx, super::ANY_USER)
        .ok_or_else(|| Error::not_found("Playlist"))?;
    Ok(Reply::one("playlist", v))
}

pub fn update_playlist(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "playlistId", "pl", "Playlist")?;
    let (mut name, _, mut urls) = server
        .library
        .playlist_by_id(id)
        .ok_or_else(|| Error::not_found("Playlist"))?;
    if let Some(new) = p.get("name").map(str::trim).filter(|n| !n.is_empty()) {
        if new != name {
            server.library.rename_playlist(id, new).map_err(failed)?;
            name = new.to_string();
        }
    }
    let mut remove: Vec<usize> = p
        .all("songIndexToRemove")
        .iter()
        .filter_map(|i| i.parse().ok())
        .collect();
    remove.sort_unstable();
    remove.dedup();
    for i in remove.into_iter().rev() {
        if i < urls.len() {
            urls.remove(i);
        }
    }
    urls.extend(song_urls(server, &p.all("songIdToAdd"))?);
    server.library.save_playlist(&name, &urls).map_err(failed)?;
    Ok(Reply::empty())
}

pub fn delete_playlist(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "id", "pl", "Playlist")?;
    server
        .library
        .playlist_by_id(id)
        .ok_or_else(|| Error::not_found("Playlist"))?;
    server.library.delete_playlist(id).map_err(failed)?;
    Ok(Reply::empty())
}

// ---------------------------------------------------------------- stars, ratings, plays

fn esc(s: &str) -> String {
    crate::control::jive::xmlbrowser::escape_search(s)
}

/// The favorites URLs that stand for an album or artist (as Jive and the player menus save
/// them), the first being the one to save.
fn album_urls(a: &rms_library::catalog::AlbumInfo) -> Vec<String> {
    let base = format!("db:album.title={}", esc(&a.title));
    match &a.artist {
        Some(ar) => vec![format!("{base}&contributor.name={}", esc(ar)), base],
        None => vec![base],
    }
}

fn command_failed(e: CommandError) -> Error {
    match e {
        CommandError::Invalid(m) | CommandError::Failed(m) => Error::new(0, m),
    }
}

pub async fn star(server: &Arc<Server>, p: &Params, on: bool) -> Result<Reply> {
    // (favorite URLs, title) of everything named
    let mut items: Vec<(Vec<String>, String)> = Vec::new();
    for raw in p.all("id") {
        if raw.starts_with("al-") {
            items.push(album_item(server, raw)?);
        } else if raw.starts_with("ar-") {
            items.push(artist_item(server, raw)?);
        } else {
            let t = parse_id(raw, "tr")
                .and_then(|id| server.library.track(id))
                .ok_or_else(|| Error::not_found("Song"))?;
            items.push((vec![t.url.clone()], t.title.clone()));
        }
    }
    for raw in p.all("albumId") {
        items.push(album_item(server, raw)?);
    }
    for raw in p.all("artistId") {
        items.push(artist_item(server, raw)?);
    }
    for (urls, title) in items {
        let existing = urls.iter().find_map(|u| server.favorites.find_url(u));
        let words = match (on, existing) {
            (true, None) => vec![
                "favorites".to_string(),
                "add".into(),
                format!("url:{}", urls[0]),
                format!("title:{}", title),
            ],
            (false, Some(id)) => vec!["favorites".into(), "delete".into(), format!("item_id:{id}")],
            _ => continue,
        };
        exec_for(server, None, words)
            .await
            .map_err(command_failed)?;
    }
    Ok(Reply::empty())
}

fn album_item(server: &Server, raw: &str) -> Result<(Vec<String>, String)> {
    let a = parse_id(raw, "al")
        .and_then(|id| server.library.album_info(id))
        .ok_or_else(|| Error::not_found("Album"))?;
    Ok((album_urls(&a), a.title.clone()))
}

fn artist_item(server: &Server, raw: &str) -> Result<(Vec<String>, String)> {
    let a = parse_id(raw, "ar")
        .and_then(|id| server.library.artist_info(id))
        .ok_or_else(|| Error::not_found("Artist"))?;
    Ok((
        vec![format!("db:contributor.name={}", esc(&a.name))],
        a.name.clone(),
    ))
}

pub fn set_rating(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "id", "tr", "Song")?;
    let rating = p
        .int("rating")?
        .ok_or_else(|| Error::new(10, "Required parameter is missing: rating"))?;
    if !(0..=5).contains(&rating) {
        return Err(Error::new(0, "rating must be 0–5"));
    }
    server
        .library
        .track(id)
        .ok_or_else(|| Error::not_found("Song"))?;
    server
        .library
        .set_rating(&id.to_string(), (rating * 20) as u8);
    Ok(Reply::empty())
}

/// A finished play (`submission`, the default) counts it and is scrobbled to the default
/// ListenBrainz accounts; a "now playing" notice is passed on to them (`getNowPlaying` shows
/// the players).
pub async fn scrobble(server: &Arc<Server>, p: &Params) -> Result<Reply> {
    let ids = p.all("id");
    if ids.is_empty() {
        return Err(Error::new(10, "Required parameter is missing: id"));
    }
    let submission = p.flag("submission", true);
    // when each was played (ms), for listens sent late
    let times: Vec<Option<i64>> = p
        .all("time")
        .iter()
        .map(|t| t.parse::<i64>().ok().map(|ms| ms / 1000))
        .collect();
    for (i, url) in song_urls(server, &ids)?.into_iter().enumerate() {
        if submission {
            server.library.mark_played(&url);
        }
        let at = times.get(i).copied().flatten();
        crate::plugins::scrobble::subsonic(server, &url, at, submission).await;
    }
    Ok(Reply::empty())
}

// ---------------------------------------------------------------- scanning, users

fn scan_status_value(server: &Server) -> Value {
    json!({ "scanning": server.library.is_scanning(), "count": server.library.totals().songs })
}

pub fn scan_status(server: &Server) -> Result<Reply> {
    Ok(Reply::one("scanStatus", scan_status_value(server)))
}

pub async fn start_scan(server: &Arc<Server>) -> Result<Reply> {
    crate::api::settings::scan(server, Default::default())
        .await
        .map_err(command_failed)?;
    Ok(Reply::one(
        "scanStatus",
        json!({ "scanning": true, "count": server.library.totals().songs }),
    ))
}

fn user_value(name: &str) -> Value {
    json!({
        "username": name, "scrobblingEnabled": true, "adminRole": true, "settingsRole": true,
        "downloadRole": true, "uploadRole": false, "playlistRole": true, "coverArtRole": true,
        "commentRole": false, "podcastRole": false, "streamRole": true, "jukeboxRole": true,
        "shareRole": false, "videoConversionRole": false, "folder": [1],
    })
}

pub fn user(server: &Server, p: &Params) -> Result<Reply> {
    Ok(Reply::one("user", user_value(&super::user_name(server, p))))
}

pub fn users(server: &Server) -> Result<Reply> {
    let name = server.prefs.server.get_str("username");
    let name = if name.is_empty() {
        super::ANY_USER.to_string()
    } else {
        name
    };
    Ok(Reply::one("users", json!({ "user": [user_value(&name)] })))
}

// ---------------------------------------------------------------- jukebox

/// The jukebox: the player named by the `subsonicJukebox` pref, else the first connected
/// one.
fn jukebox_player(server: &Server) -> Result<Arc<Player>> {
    let chosen = server.prefs.server.get_str("subsonicJukebox");
    if !chosen.is_empty() {
        if let Some(p) = server.resolve_client(&chosen) {
            return Ok(p);
        }
    }
    server
        .players()
        .into_iter()
        .find(|p| crate::api::state::player(server, p).connected)
        .ok_or_else(|| Error::new(0, "No player is connected to use as the jukebox"))
}

async fn run(server: &Arc<Server>, p: &Arc<Player>, cmd: Command) -> Result<()> {
    commands::run(server, p, cmd)
        .await
        .map_err(command_failed)?;
    Ok(())
}

fn status_value(server: &Server, p: &Arc<Player>) -> Value {
    let s = crate::api::state::player(server, p);
    json!({
        "currentIndex": s.queue.index.map(|i| i as i64).unwrap_or(-1),
        "playing": s.mode == PlayMode::Play,
        "gain": s.volume as f64 / 100.0,
        "position": s.elapsed.floor() as i64,
    })
}

pub async fn jukebox(server: &Arc<Server>, p: &Params) -> Result<Reply> {
    let action = p.require("action")?;
    let player = jukebox_player(server)?;
    let track_items = |ids: Vec<&str>| -> Result<Vec<Item>> {
        ids.iter()
            .map(|raw| {
                parse_id(raw, "tr")
                    .filter(|id| server.library.track(*id).is_some())
                    .map(|id| Item::Track { id })
                    .ok_or_else(|| Error::not_found("Song"))
            })
            .collect()
    };
    match action {
        "get" | "status" => {}
        "set" | "add" => {
            let items = track_items(p.all("id"))?;
            if action == "set" {
                run(server, &player, Command::Clear).await?;
            }
            for item in items {
                let cmd = Command::Enqueue {
                    item,
                    mode: EnqueueMode::Add,
                    start: None,
                };
                run(server, &player, cmd).await?;
            }
        }
        "start" => run(server, &player, Command::Play).await?,
        "stop" => run(server, &player, Command::Pause).await?,
        "skip" => {
            let index = p
                .int("index")?
                .ok_or_else(|| Error::new(10, "Required parameter is missing: index"))?;
            run(
                server,
                &player,
                Command::Jump {
                    index: index.max(0) as usize,
                },
            )
            .await?;
            if let Some(offset) = p.int("offset")?.filter(|o| *o > 0) {
                run(
                    server,
                    &player,
                    Command::Seek {
                        position: offset as f64,
                    },
                )
                .await?;
            }
        }
        "clear" => run(server, &player, Command::Clear).await?,
        "remove" => {
            let index = p
                .int("index")?
                .ok_or_else(|| Error::new(10, "Required parameter is missing: index"))?;
            run(
                server,
                &player,
                Command::Remove {
                    index: index.max(0) as usize,
                },
            )
            .await?;
        }
        "shuffle" => {
            run(
                server,
                &player,
                Command::Shuffle {
                    mode: Shuffle::Songs,
                },
            )
            .await?
        }
        "setGain" => {
            let gain: f64 = p
                .get("gain")
                .and_then(|g| g.parse().ok())
                .ok_or_else(|| Error::new(10, "Required parameter is missing: gain"))?;
            let value = (gain.clamp(0.0, 1.0) * 100.0).round() as i32;
            run(server, &player, Command::Volume { value }).await?;
        }
        other => return Err(Error::new(0, format!("Unknown jukebox action {other}"))),
    }
    let mut status = status_value(server, &player);
    if action == "get" {
        let ctx = Ctx::new(server);
        let queue = crate::api::state::queue(server, &player.id.to_string(), 0, usize::MAX);
        let entries: Vec<Value> = queue
            .map(|q| q.items)
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.id.and_then(|id| server.library.track(id)))
            .map(|t| song(&t, &ctx))
            .collect();
        status["entry"] = Value::Array(entries);
        return Ok(Reply::one("jukeboxPlaylist", status));
    }
    Ok(Reply::one("jukeboxStatus", status))
}
