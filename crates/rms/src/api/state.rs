//! Builds the API's view of the server and its players from the live state.

use super::model::*;
use crate::control::songdata::song_row;
use crate::player::playlist::Entry;
use crate::player::{Player as LivePlayer, Song};
use crate::server::Server;
use serde_json::{Map, Value};
use std::sync::Arc;

/// Tag letters for [`song_row`]: url, artist, artist id, album, album id, cover id,
/// artwork url, duration, track number, year, type, remote flag, station name.
const TRACK_TAGS: &str = "uasleKcdtiyoxN";

pub fn server_info(server: &Server) -> ServerInfo {
    let lib = &server.library;
    let progress = lib.progress();
    let scan = lib.is_scanning().then(|| ScanProgress {
        step: progress
            .step
            .rsplit('|')
            .next()
            .unwrap_or_default()
            .to_string(),
        done: progress.done as u64,
        total: progress.total as u64,
    });
    let t = lib.totals();
    ServerInfo {
        name: server.library_name(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uuid: server.uuid(),
        scan,
        last_scan: progress.last_scan,
        library: LibraryTotals {
            albums: t.albums,
            artists: t.artists,
            genres: t.genres,
            tracks: t.songs,
            duration: t.duration,
        },
        https_port: crate::web::tls::port(),
        local_ca: crate::web::tls::local_ca(),
    }
}

pub fn players(server: &Server) -> Vec<Player> {
    server.players().iter().map(|p| player(server, p)).collect()
}

pub fn player(server: &Server, p: &Arc<LivePlayer>) -> Player {
    let (name, model, model_name, ip, firmware, connected, power, sleep_time, has_digital) = {
        let s = p.state.lock();
        (
            s.name.clone(),
            s.caps.model.clone(),
            s.caps.model_name.clone(),
            s.addr.filter(|_| s.connected).map(|a| a.ip().to_string()),
            s.caps.firmware.clone(),
            s.connected,
            s.power,
            s.sleep_time,
            s.caps.has_digital_out,
        )
    };
    let volume = p.volume();
    let muted = p.is_muted();
    let digital_volume = p.prefs.get_i64("digitalVolumeControl") != 0;

    let c = p.controller();
    let c = c.lock();
    let mode = match c.mode() {
        "play" => PlayMode::Play,
        "pause" => PlayMode::Pause,
        _ => PlayMode::Stop,
    };
    let buffering = c.is_playing(false) && !c.is_playing(true);
    let song = c.playing_song().cloned();
    let length = c.playlist.len();
    let index = (length > 0).then(|| c.playing_song_index());
    let current = index.and_then(|i| c.playlist.song(i).cloned());
    let synced_with = c
        .all_players()
        .iter()
        .filter(|m| m.id != p.id)
        .map(|m| m.id.to_string())
        .collect();
    let queue = QueueInfo {
        version: c.playlist.timestamp,
        length,
        index,
        name: c.playlist.name.clone(),
    };
    let shuffle = Shuffle::from_lms(c.playlist.shuffle);
    let repeat = Repeat::from_lms(c.playlist.repeat);
    let elapsed = if song.is_some() {
        c.playing_song_elapsed()
    } else {
        0.0
    };
    drop(c);

    let now_playing = current.map(|e| {
        let live = song.as_ref().filter(|s| s.url() == e.url);
        track(server, &e, live)
    });
    let duration = song
        .as_ref()
        .and_then(Song::duration)
        .or(now_playing.as_ref().and_then(|t| t.duration));
    let can_seek = song
        .as_ref()
        .map(|s| s.can_seek && s.duration().is_some())
        .unwrap_or(false);
    let sleep = (sleep_time > 0.0).then(|| (sleep_time - crate::util::now()).max(0.0));

    Player {
        id: p.id.to_string(),
        name,
        model,
        model_name,
        ip,
        firmware,
        connected,
        power,
        mode,
        buffering,
        elapsed: round_ms(elapsed),
        duration,
        can_seek,
        volume: volume.abs(),
        muted,
        volume_fixed: has_digital && !digital_volume,
        shuffle,
        repeat,
        queue,
        now_playing,
        synced_with,
        sleep,
    }
}

/// The queue, or `None` for an unknown player.
pub fn queue(server: &Server, id: &str, offset: usize, limit: usize) -> Option<Queue> {
    let p = server.resolve_client(id)?;
    let c = p.controller();
    let c = c.lock();
    let length = c.playlist.len();
    let index = (length > 0).then(|| c.playing_song_index());
    let version = c.playlist.timestamp;
    let playing = c.playing_song().cloned();
    let entries: Vec<(usize, Entry, Option<Song>)> = (offset..length.min(offset + limit))
        .filter_map(|i| {
            let e = c.playlist.song(i)?.clone();
            let s = if Some(i) == index {
                playing.clone()
            } else {
                None
            }
            .or_else(|| c.queue.iter().find(|s| s.index == i).cloned());
            Some((i, e, s))
        })
        .collect();
    drop(c);
    let items = entries
        .iter()
        .map(|(_, e, s)| track(server, e, s.as_ref()))
        .collect();
    Some(Queue {
        version,
        length,
        index,
        offset,
        items,
    })
}

pub fn track(server: &Server, entry: &Entry, song: Option<&Song>) -> Track {
    let row = song_row(server, entry, song, TRACK_TAGS);
    track_from_row(&row, &entry.url)
}

pub(super) fn track_from_row(row: &Map<String, Value>, url: &str) -> Track {
    let remote = int(row.get("remote")) == Some(1) || crate::util::is_remote_url(url);
    let mut title = string(row.get("title")).unwrap_or_default();
    let mut artist = string(row.get("artist"));
    let station = string(row.get("remote_title"));
    // Many stations only send "Artist - Title" as the stream title.
    if remote && artist.is_none() && station.is_some() {
        if let Some((a, t)) = title.split_once(" - ") {
            artist = Some(a.trim().to_string());
            title = t.trim().to_string();
        }
    }
    // a player's Spotify Connect stream: its tracks are the app's; name the stream itself
    let connect = url.starts_with("spotify://connect:")
        || string(row.get("url")).is_some_and(|u| u.starts_with("spotify://connect:"));
    let station = if connect {
        if title.is_empty() {
            title = "Spotify Connect".into();
        }
        Some("Spotify Connect".to_string())
    } else {
        station
    };
    let spotify = spotify_meta(string(row.get("url")).as_deref().unwrap_or(url));
    let artwork = string(row.get("artwork_url"))
        .map(|u| artwork_path(&u))
        .or_else(|| {
            string(row.get("coverid"))
                .filter(|c| c != "0")
                .map(|c| format!("music/{}/cover", c))
        });
    Track {
        id: int(row.get("id")).filter(|&i| i > 0),
        url: string(row.get("url")).unwrap_or_else(|| url.to_string()),
        title,
        artist,
        artist_id: int(row.get("artist_id")),
        album: string(row.get("album")),
        album_id: int(row.get("album_id")),
        duration: float(row.get("duration")).filter(|&d| d > 0.0),
        track_number: int(row.get("tracknum")),
        disc: int(row.get("disc")),
        year: int(row.get("year")).filter(|&y| y > 0),
        artwork,
        spotify_album: spotify.as_ref().and_then(|m| m.album_menu()),
        spotify_artist: spotify.as_ref().and_then(|m| m.artist_menu()),
        remote,
        station,
        format: string(row.get("type")),
        detail: None,
    }
}

/// What browsing remembered of a Spotify track, or of what a Connect stream plays now.
fn spotify_meta(url: &str) -> Option<crate::plugins::spotify::TrackMeta> {
    use crate::plugins::spotify::{connect, get};
    if !url.starts_with("spotify:") {
        return None;
    }
    let url = if url.starts_with("spotify://connect:") {
        connect::audible(url)?.url?
    } else {
        url.to_string()
    };
    get()?.meta.get(&url).map(|m| m.clone())
}

/// Server-relative artwork path without size suffix, so clients can append one.
pub(super) fn artwork_path(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        return url.to_string();
    }
    let u = url.trim_start_matches('/');
    match u.strip_suffix("/image.png") {
        Some(base) if base.starts_with("imageproxy/") => format!("{}/image", base),
        _ => u.to_string(),
    }
}

fn round_ms(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

pub(super) fn string(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Integer from a number or a numeric string; for id lists (`"3,7"`) the first id.
pub(super) fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.split(',').next()?.trim().parse().ok(),
        _ => None,
    }
}

pub(super) fn float(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn local_track() {
        let t = track_from_row(
            &row(
                json!({"id": 12, "title": "Song", "artist": "A", "artist_id": "4,9",
                        "album": "Al", "album_id": 3, "coverid": "ab12", "duration": "61.5",
                        "tracknum": "2", "year": 0, "type": "flc"}),
            ),
            "file:///m/song.flac",
        );
        assert_eq!(t.id, Some(12));
        assert_eq!(t.artist_id, Some(4));
        assert_eq!(t.duration, Some(61.5));
        assert_eq!(t.year, None);
        assert_eq!(t.artwork.as_deref(), Some("music/ab12/cover"));
        assert!(!t.remote);
    }

    #[test]
    fn radio_title_is_split() {
        let t = track_from_row(
            &row(
                json!({"id": -5, "title": "Band - Tune", "remote": 1, "remote_title": "Radio X",
                        "artwork_url": "/imageproxy/http%3A%2F%2Fx%2Fa.jpg/image.png"}),
            ),
            "http://x/stream",
        );
        assert_eq!(t.id, None);
        assert_eq!(t.title, "Tune");
        assert_eq!(t.artist.as_deref(), Some("Band"));
        assert_eq!(t.station.as_deref(), Some("Radio X"));
        assert_eq!(
            t.artwork.as_deref(),
            Some("imageproxy/http%3A%2F%2Fx%2Fa.jpg/image")
        );
    }
}
