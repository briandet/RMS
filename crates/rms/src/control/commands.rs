//! Commands (`Slim::Control::Commands`).

use super::request::{Request, Status};
use crate::player::command::{
    playlist_changed, remove_track, run, Change, Enqueue, PlayerCommand, Position, Repeat, Shuffle,
};
use crate::player::playlist::Entry;
use crate::player::Player;
use crate::server::Server;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{info, warn};

pub fn done(_: &Arc<Server>, r: &mut Request) {
    r.set_done();
}

pub fn done_query(_: &Arc<Server>, r: &mut Request) {
    r.set_done();
}

pub(crate) fn player(server: &Arc<Server>, r: &Request) -> Option<Arc<Player>> {
    r.client.and_then(|c| server.player(&c))
}

fn ctrl(p: &Player) -> Arc<Mutex<crate::player::controller::Controller>> {
    p.controller()
}

// ---- play control ------------------------------------------------------------------------

/// `play`, `pause`, `stop`, `mode <x>` (`playcontrolCommand`).
pub fn playcontrol(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let fade = r.param_f64("_fadein");
    let cmd = match r.term(0) {
        "play" => PlayerCommand::Play { fade },
        "stop" => PlayerCommand::Stop,
        "mode" => match r.term(1) {
            "play" => PlayerCommand::Play { fade },
            "pause" => PlayerCommand::Pause,
            "stop" => PlayerCommand::Stop,
            _ => return r.set_done(),
        },
        "pause" => match r.param("_newvalue") {
            Some("1") => PlayerCommand::Pause,
            Some("0") => PlayerCommand::Play { fade },
            _ => PlayerCommand::TogglePause { fade },
        },
        _ => return r.bad_dispatch(),
    };
    let _ = run(server, &p, cmd);
    r.set_done();
}

/// `time <secs|+secs|-secs>`.
pub fn time(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(t) = r.param("_newvalue").map(str::to_string) else {
        return r.bad_params();
    };
    let Ok(secs) = t.parse::<f64>() else {
        return r.bad_params();
    };
    let change = if t.starts_with('+') || t.starts_with('-') {
        Change::By(secs)
    } else {
        Change::To(secs)
    };
    match run(server, &p, PlayerCommand::Seek(change)) {
        Ok(_) => r.set_done(),
        Err(_) => r.bad_params(),
    }
}

pub fn power(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let on = match r.param("_newvalue") {
        Some("0") => Some(false),
        Some("1") => Some(true),
        _ => None,
    };
    if let Some(seq) = r.param_i64("seq_no") {
        p.state.lock().sequence_number = seq as u32;
    }
    let noplay = r.param_bool("_noplay");
    // LMS: no status change → echoed, not notified
    if let Ok(o) = run(server, &p, PlayerCommand::Power { on, noplay }) {
        if o.changed {
            r.set_done();
        }
    }
}

pub fn sleep(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(secs) = r.param_f64("_newvalue") else {
        return r.bad_params();
    };
    let _ = run(server, &p, PlayerCommand::Sleep(secs));
    r.set_done();
}

pub fn mixer(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let entity = r.term(1).to_string();
    let val = r.param("_newvalue").map(str::to_string);
    if let Some(seq) = r.param_i64("seq_no") {
        p.state.lock().sequence_number = seq as u32;
    }
    let relative = |v: &str| v.starts_with('+') || v.starts_with('-');
    match entity.as_str() {
        "volume" => {
            let Some(v) = val else { return r.bad_params() };
            let Ok(n) = v.parse::<f64>() else {
                return r.bad_params();
            };
            let n = n.round() as i32;
            let change = if relative(&v) {
                Change::By(n)
            } else {
                Change::To(n)
            };
            let _ = run(server, &p, PlayerCommand::Volume(change));
        }
        "muting" => {
            let muted = match val.as_deref() {
                Some("0") => Some(false),
                Some("1") => Some(true),
                _ => None,
            };
            let _ = run(server, &p, PlayerCommand::Mute(muted));
        }
        "bass" | "treble" | "pitch" | "stereoxl" => {
            let Some(v) = val else { return r.bad_params() };
            let Ok(n) = v.parse::<i64>() else {
                return r.bad_params();
            };
            let old = p.prefs.get_i64(&entity);
            let new = if relative(&v) { old + n } else { n };
            p.prefs.set(&entity, json!(new));
            let mut s = p.state.lock();
            match entity.as_str() {
                "bass" => s.bass = new as i32,
                "treble" => s.treble = new as i32,
                "pitch" => s.pitch = new as i32,
                _ => {}
            }
        }
        _ => return r.bad_dispatch(),
    }
    r.set_done();
}

pub fn name(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(n) = r.param("_newvalue").map(str::to_string) else {
        return r.bad_params();
    };
    if n != "0" {
        let _ = run(server, &p, PlayerCommand::Rename(n));
    }
    r.set_done();
}

pub fn sync(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(v) = r.param("_indexid-").map(str::to_string) else {
        return r.bad_params();
    };
    if v == "-" {
        let _ = run(server, &p, PlayerCommand::Unsync);
    } else {
        let buddy = server.resolve_client(&v).or_else(|| {
            v.parse::<usize>()
                .ok()
                .and_then(|i| server.connected_players().get(i).cloned())
        });
        if let Some(with) = buddy {
            let no_restart = r.param_bool("noRestart");
            let _ = run(server, &p, PlayerCommand::Sync { with, no_restart });
        }
    }
    r.set_done();
}

pub fn client_forget(server: &Arc<Server>, r: &mut Request) {
    let Some(id) = r.client else {
        return r.bad_dispatch();
    };
    if let Some(p) = server.player(&id) {
        if p.is_connected() {
            return;
        }
        server.forget_player(&p);
    }
    r.set_done();
}

/// `connect <ip>`: send the player to another server (`serv` frame).
pub fn connect(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(host) = r.param("_where").map(str::to_string) else {
        return r.bad_params();
    };
    let ip = if host == "www.mysqueezebox.com" || host == "0" {
        0x0000_0001
    } else {
        use std::net::ToSocketAddrs;
        match (host.as_str(), 3483)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.find(|x| x.is_ipv4()))
        {
            Some(std::net::SocketAddr::V4(v4)) => u32::from(*v4.ip()),
            _ => return r.bad_params(),
        }
    };
    ctrl(&p).lock().stop();
    let sg = p.prefs.get_i64("syncgroupid");
    p.send(slimproto::server::ServerFrame::Serv {
        ip,
        sync_group_id: if sg > 0 { Some(sg as u64) } else { None },
    });
    p.prefs.remove("syncgroupid");
    r.set_done();
}

pub fn button(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(b) = r.param("_buttoncode").map(str::to_string) else {
        return r.bad_params();
    };
    super::ir::execute_button(server, &p, &b);
    r.set_done();
}

pub fn ir(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let (Some(code), Some(_t)) = (r.param("_ircode").map(str::to_string), r.param("_time")) else {
        return r.bad_params();
    };
    if let Ok(c) = u32::from_str_radix(code.trim_start_matches("0x"), 16) {
        let time = r
            .param("_time")
            .and_then(|t| t.parse::<f64>().ok())
            .unwrap_or(0.0);
        super::ir::handle_ir(server, &p, c, time as u32);
    }
    r.set_done();
}

/// `display <line1> <line2> <duration>`: players without a display only get the notification.
pub fn display(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(l1) = r.param("_line1").map(str::to_string) else {
        return r.bad_params();
    };
    let l2 = r.param("_line2").unwrap_or("").to_string();
    let dur = r.param_f64("_duration").unwrap_or(3.0);
    show_briefly(server, &p, vec![l1, l2], dur);
    r.set_done();
}

pub fn show(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let l1 = r.param("line1").unwrap_or("").to_string();
    let l2 = r.param("line2").unwrap_or("").to_string();
    if l1.is_empty() && l2.is_empty() {
        return r.bad_params();
    }
    let dur = r.param_f64("duration").unwrap_or(3.0);
    show_briefly(server, &p, vec![l1, l2], dur);
    r.set_done();
}

/// Record a showBriefly and notify `displaystatus` subscribers (Jive popups).
pub fn show_briefly(server: &Arc<Server>, p: &Arc<Player>, lines: Vec<String>, duration: f64) {
    p.state.lock().show_briefly = Some((lines.clone(), crate::util::now() + duration));
    let mut words = vec!["displaynotify".to_string(), "showbriefly".to_string()];
    words.extend(lines);
    words.push(duration.to_string());
    server.notifier.notify_owned(Some(p.id), words);
}

// ---- prefs ---------------------------------------------------------------------------------

pub fn pref(server: &Arc<Server>, r: &mut Request) {
    let Some(name) = r.param("_prefname").map(str::to_string) else {
        return r.bad_params();
    };
    let val = r
        .param("value")
        .or(r.param("_newvalue"))
        .map(|v| v.strip_prefix("value:").unwrap_or(v).to_string());
    let Some(val) = val else {
        return r.bad_params();
    };
    let (ns, pname) = match name.split_once(':') {
        Some((ns, n)) => (ns.to_string(), n.to_string()),
        None => ("server".to_string(), name.clone()),
    };
    let value: Value = if pname == "mediadirs" || pname == "disabledformats" {
        // arrays: accept JSON or comma separated
        serde_json::from_str(&val).unwrap_or_else(|_| {
            json!(val
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>())
        })
    } else if let Ok(i) = val.parse::<i64>() {
        json!(i)
    } else {
        json!(val)
    };
    if r.term(0) == "playerpref" {
        let Some(p) = player(server, r) else {
            return r.bad_dispatch();
        };
        match pname.as_str() {
            "playername" => p.set_name(&val),
            "volume" => {
                p.set_volume(val.parse().unwrap_or(50), false);
            }
            // plugin prefs keep their namespace (`plugin.dontstopthemusic:provider`)
            _ if ns != "server" => {
                p.prefs.set(&format!("{ns}:{pname}"), value);
            }
            _ => {
                p.prefs.set(&pname, value);
            }
        }
        server
            .notifier
            .notify_owned(Some(p.id), vec!["prefset".into(), ns, pname, val]);
    } else {
        if ns != "server" {
            server.prefs.server.set(&format!("{}:{}", ns, pname), value);
        } else {
            server.prefs.server.set(&pname, value);
            if matches!(
                pname.as_str(),
                "mediadirs"
                    | "playlistdir"
                    | "ignoredarticles"
                    | "splitList"
                    | "variousArtistsString"
                    | "groupdiscs"
            ) {
                server.apply_library_config();
            }
            if matches!(pname.as_str(), "autorescan" | "mediadirs" | "playlistdir") {
                crate::watch::start(server);
            }
            if pname == "onlineArtistImages" && !server.library.is_scanning() {
                crate::plugins::artist_images::spawn_fetch(server);
            }
            if pname == "sonicAnalysis" && !server.library.is_scanning() {
                crate::plugins::sonic::spawn_analysis(server);
            }
        }
        server
            .notifier
            .notify_owned(None, vec!["prefset".into(), ns, pname, val]);
    }
    r.set_done();
}

// ---- server --------------------------------------------------------------------------------

pub fn rescan(server: &Arc<Server>, r: &mut Request) {
    let mode = match r.param("_mode") {
        Some("playlists") => rms_library::ScanMode::Playlists,
        Some("full") | None | Some("") => rms_library::ScanMode::Full,
        Some("external") | Some("onlinelibrary") => rms_library::ScanMode::Full,
        Some(other) => {
            if let Some(p) = crate::util::path_from_file_url(other) {
                rms_library::ScanMode::Dir(p)
            } else {
                rms_library::ScanMode::Full
            }
        }
    };
    start_scan(server, mode);
    r.set_done();
}

pub fn wipecache(server: &Arc<Server>, r: &mut Request) {
    start_scan(server, rms_library::ScanMode::Wipe);
    r.set_done();
}

pub fn abortscan(server: &Arc<Server>, r: &mut Request) {
    server.library.abort_scan();
    r.set_done();
}

/// Run a scan in the background unless one is already running.
pub fn start_scan(server: &Arc<Server>, mode: rms_library::ScanMode) {
    let s = server.clone();
    if !claim_scan(server) {
        info!("scan already running, request ignored");
        return;
    }
    tokio::spawn(async move { run_scans(&s, vec![mode]).await });
}

/// Take the scanner for [`run_scans`]; false when a scan is running.
pub fn claim_scan(server: &Server) -> bool {
    !server
        .scanning
        .swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Run scans one after the other (the scanner must be claimed with [`claim_scan`]), then
/// tell clients and start the follow-up work (artist pictures, sonic analysis).
pub async fn run_scans(s: &Arc<Server>, modes: Vec<rms_library::ScanMode>) {
    s.apply_library_config();
    for mode in modes {
        let lib = s.library.clone();
        info!("library scan started: {:?}", mode);
        let res = tokio::task::spawn_blocking(move || lib.scan(mode)).await;
        match res {
            Ok(Ok(summary)) => info!("library scan finished: {:?}", summary),
            Ok(Err(e)) => warn!("library scan failed: {}", e),
            Err(e) => warn!("library scan panicked: {}", e),
        }
    }
    s.scanning.store(false, std::sync::atomic::Ordering::SeqCst);
    s.notifier.notify(None, &["rescan", "done"]);
    s.notifier.notify(None, &["library", "changed", "1"]);
    crate::plugins::artist_images::spawn_fetch(s);
    crate::plugins::sonic::spawn_analysis(s);
}

pub fn stopserver(server: &Arc<Server>, r: &mut Request) {
    info!("stop requested via {}", r.term(0));
    server.shutdown.cancel();
    r.set_done();
}

pub fn rating(server: &Arc<Server>, r: &mut Request) {
    let (Some(item), Some(rating)) = (r.param("_item").map(str::to_string), r.param_i64("_rating"))
    else {
        return r.bad_params();
    };
    server.library.set_rating(&item, rating.clamp(0, 100) as u8);
    r.set_done();
}

// ---- playlist ------------------------------------------------------------------------------

pub fn playlist_clear(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let _ = run(server, &p, PlayerCommand::Clear);
    r.set_done();
}

pub fn playlist_delete(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(idx) = r.param_i64("_index") else {
        return r.bad_params();
    };
    let index = idx.max(0) as usize;
    let _ = run(server, &p, PlayerCommand::Remove { index });
    r.set_done();
}

pub fn playlist_deleteitem(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(item) = r.param("_item").map(str::to_string) else {
        return r.bad_params();
    };
    let url = to_url(&item);
    let c = ctrl(&p);
    let mut c = c.lock();
    loop {
        let pos = c
            .playlist
            .iter()
            .find(|(_, e)| {
                e.url == url
                    || e.url
                        .starts_with(&format!("{}/", url.trim_end_matches('/')))
            })
            .map(|(i, _)| i);
        match pos {
            Some(i) => remove_track(&mut c, i),
            None => break,
        }
    }
    playlist_changed(&mut c);
    r.set_done();
}

pub fn playlist_move(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let (Some(from), Some(to)) = (r.param_i64("_fromindex"), r.param_i64("_toindex")) else {
        return r.bad_params();
    };
    let (from, to) = (from.max(0) as usize, to.max(0) as usize);
    let _ = run(server, &p, PlayerCommand::Move { from, to });
    r.set_done();
}

pub fn playlist_repeat(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let mode = r
        .param_i64("_newvalue")
        .map(|v| Repeat::from_lms(v.clamp(0, 2) as u8));
    let _ = run(server, &p, PlayerCommand::Repeat(mode));
    r.set_done();
}

pub fn playlist_shuffle(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let mode = r
        .param_i64("_newvalue")
        .map(|v| Shuffle::from_lms(v.clamp(0, 2) as u8));
    let _ = run(server, &p, PlayerCommand::Shuffle(mode));
    r.set_done();
}

/// `playlist index|jump <index|+N|-N> [fadein] [noplay] [seekdata]`.
pub fn playlist_jump(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let idx_s = r.param("_index").unwrap_or("0").to_string();
    let n: i64 = idx_s.parse().unwrap_or(0);
    let to = if idx_s.starts_with('+') || idx_s.starts_with('-') {
        Position::By(n)
    } else {
        Position::Index(n.max(0) as usize)
    };
    let cmd = PlayerCommand::Jump {
        to,
        fade: r.param_f64("_fadein"),
        noplay: r.param_bool("_noplay"),
        seek: r
            .param("_seekdata")
            .and_then(|s| s.split(':').next_back().and_then(|t| t.parse::<f64>().ok())),
    };
    if let Ok(o) = run(server, &p, cmd) {
        if o.changed {
            r.set_done();
        }
    }
}

pub fn playlist_save(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let title = r
        .param("_title")
        .or(r.param("playlistName"))
        .map(str::to_string);
    let Some(title) = title.filter(|t| !t.is_empty()) else {
        return r.bad_params();
    };
    let urls: Vec<String> = {
        let c = ctrl(&p);
        let c = c.lock();
        if server.prefs.server.get_bool("saveShuffled") {
            c.playlist.iter().map(|(_, e)| e.url.clone()).collect()
        } else {
            c.playlist.entries.iter().map(|e| e.url.clone()).collect()
        }
    };
    match server.library.save_playlist(&title, &urls) {
        Ok(id) => {
            r.add_result("__playlist_id", id);
            let c = ctrl(&p);
            let mut c = c.lock();
            c.playlist.name = Some(title);
            c.playlist.url = server.library.playlist_by_id(id).map(|(_, u, _)| u);
            c.playlist.modified = false;
        }
        Err(e) => {
            warn!("playlist save failed: {}", e);
            r.add_result("writeError", 1);
        }
    }
    r.set_done();
}

pub fn playlist_zap(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let mut c = c.lock();
    let idx = r
        .param_i64("_index")
        .map(|i| i as usize)
        .unwrap_or_else(|| c.playing_song_index());
    let url = c.playlist.song(idx).map(|e| e.url.clone());
    remove_track(&mut c, idx);
    playlist_changed(&mut c);
    drop(c);
    if let Some(url) = url {
        // append to the "Zapped Songs" saved playlist
        let name = "Zapped Songs";
        let mut urls = Vec::new();
        if let Ok(row) = server.library.query(&rms_library::LibQuery {
            name: rms_library::LibQueryName::Playlists,
            start: Some(0),
            quantity: Some(10000),
            params: [("tags".to_string(), "u".to_string())]
                .into_iter()
                .collect(),
        }) {
            if let Some(Value::Array(items)) = row.get("playlists_loop") {
                if let Some(id) = items
                    .iter()
                    .find(|i| i.get("playlist").and_then(|v| v.as_str()) == Some(name))
                    .and_then(|i| i.get("id"))
                    .and_then(|v| v.as_i64())
                {
                    if let Some((_, _, existing)) = server.library.playlist_by_id(id) {
                        urls = existing;
                    }
                }
            }
        }
        urls.push(url);
        let _ = server.library.save_playlist(name, &urls);
    }
    r.set_done();
}

/// Convert a CLI item (path or URL) to a URL.
pub fn to_url(item: &str) -> String {
    let item = item.trim();
    if item.contains("://") {
        // remote URLs sometimes arrive double-escaped
        if item.starts_with("http%3A") || item.starts_with("https%3A") {
            return crate::util::cli_unescape(item);
        }
        return item.to_string();
    }
    if item.starts_with('/') || item.starts_with('~') {
        return crate::util::file_url_from_path(std::path::Path::new(item));
    }
    item.to_string()
}

const AUDIO_EXTS: &[&str] = &[
    "flac", "flc", "mp3", "m4a", "m4b", "mp4", "aac", "alac", "ogg", "oga", "opus", "wav", "aif",
    "aiff", "wma", "ape", "wv", "mpc", "dsf", "dff",
];
const PLAYLIST_EXTS: &[&str] = &["m3u", "m3u8", "pls", "cue", "xspf", "asx", "wpl"];

/// Expand a `playlist play|add|insert` item into entries (`scanPathOrURL`).
pub async fn expand_item(server: &Arc<Server>, item: &str) -> Vec<Entry> {
    let url = to_url(item);
    // Spotify albums, playlists, artists and liked songs become their tracks
    if crate::plugins::spotify::is_spotify(&url) {
        return match crate::plugins::spotify::get().map(|sp| sp.expand(&url)) {
            Some(f) => match f.await {
                Ok(urls) => urls.into_iter().map(Entry::new).collect(),
                Err(e) => {
                    tracing::warn!("{url}: {e}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
    }
    if crate::util::is_remote_url(&url) {
        return vec![Entry::new(url)];
    }
    // a library album, artist, genre or year saved as a favorite
    if url.starts_with("db:") {
        use crate::control::jive::favorites::{resolve_db, DbObject};
        let lib_server = server.clone();
        let db_url = url.clone();
        let tracks = tokio::task::spawn_blocking(move || {
            let mut f = rms_library::TrackFilter::default();
            match resolve_db(&lib_server, &db_url)? {
                DbObject::Album(id) => f.album_id = Some(id),
                DbObject::Artist(id) => f.artist_id = id.parse().ok(),
                DbObject::Genre(id) => f.genre_id = Some(id),
                DbObject::Year(y) => f.year = y.parse().ok(),
            }
            lib_server.library.tracks_for(&f).ok()
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        return tracks.iter().map(|t| server.entry_for_track(t)).collect();
    }
    if let Some(path) = crate::util::path_from_file_url(&url) {
        if path.is_dir() {
            let lib_tracks = server.library.tracks_for(&rms_library::TrackFilter {
                folder_id: None,
                search: None,
                ..Default::default()
            });
            let _ = lib_tracks;
            let mut files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&path)
                .follow_links(true)
                .sort_by_file_name()
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
                .filter(|p| AUDIO_EXTS.contains(&crate::util::extension(p).as_str()))
                .collect();
            files.sort();
            return files
                .into_iter()
                .map(|f| server.entry_for_url(&crate::util::file_url_from_path(&f)))
                .collect();
        }
        let ext = crate::util::extension(&path);
        if PLAYLIST_EXTS.contains(&ext.as_str()) && ext != "cue" {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                let text = crate::stream::remote::decode_text(&bytes);
                let base = url.clone();
                let mut out = Vec::new();
                for (u, title) in crate::stream::remote::parse_playlist(&text, &ext, &base) {
                    let u = if u.contains("://") {
                        u
                    } else {
                        let p = std::path::Path::new(&u);
                        let full = if p.is_absolute() {
                            p.to_path_buf()
                        } else {
                            path.parent().unwrap_or(std::path::Path::new("/")).join(p)
                        };
                        crate::util::file_url_from_path(&full)
                    };
                    if let Some(t) = title {
                        if crate::util::is_remote_url(&u) {
                            server.remote_titles.insert(u.clone(), t);
                        }
                    }
                    out.push(server.entry_for_url(&u));
                }
                return out;
            }
        }
        if ext == "cue" {
            // cue sheets are indexed by the library; find its virtual tracks
            let prefix = format!(
                "{}#",
                crate::util::file_url_from_path(&path.with_extension(""))
            );
            let _ = prefix;
        }
        return vec![server.entry_for_url(&url)];
    }
    // bare name → saved playlist
    if let Ok(row) = server.library.query(&rms_library::LibQuery {
        name: rms_library::LibQueryName::Playlists,
        start: Some(0),
        quantity: Some(100000),
        params: [
            ("tags".to_string(), "u".to_string()),
            ("search".to_string(), item.to_string()),
        ]
        .into_iter()
        .collect(),
    }) {
        if let Some(Value::Array(items)) = row.get("playlists_loop") {
            if let Some(id) = items
                .iter()
                .find(|i| i.get("playlist").and_then(|v| v.as_str()) == Some(item))
                .and_then(|i| i.get("id"))
                .and_then(|v| v.as_i64())
            {
                if let Some((_, _, urls)) = server.library.playlist_by_id(id) {
                    return urls.iter().map(|u| server.entry_for_url(u)).collect();
                }
            }
        }
    }
    Vec::new()
}

/// `playlist play|load|resume|add|append|insert|insertlist <item> [title] [fadein]`.
pub async fn playlist_xitem(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    let Some(item) = r.param("_item").map(str::to_string) else {
        r.bad_params();
        return r;
    };
    let cmd = r.term(1).to_string();
    let url = to_url(&item);
    if let Some(title) = r.param_nonempty("_title") {
        if crate::util::is_remote_url(&url) {
            server.remote_titles.insert(url.clone(), title.to_string());
        }
    }
    let entries = expand_item(&server, &item).await;
    let fade = r.param_f64("_fadein");
    let c = ctrl(&p);
    {
        let mut c = c.lock();
        match cmd.as_str() {
            "play" | "load" | "resume" => {
                // bug 14760: replaying the single current URL just resumes
                let same = c.playlist.len() == 1
                    && c.playlist.song(0).map(|e| e.url == url).unwrap_or(false)
                    && entries.len() == 1;
                if same && !c.is_stopped() {
                    if c.is_paused() {
                        c.resume(fade);
                    }
                } else {
                    c.stop();
                    c.playlist.clear();
                    c.queue.clear();
                    c.playlist.append(entries);
                    if url.starts_with("file://") && PLAYLIST_EXTS.iter().any(|e| url.ends_with(e))
                    {
                        c.playlist.url = Some(url.clone());
                        c.playlist.name = crate::util::path_from_file_url(&url)
                            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()));
                    }
                    c.playlist.modified = false;
                    let jump = r.param_i64("play_index").unwrap_or(0).max(0) as usize;
                    let noplay = r.param_bool("noplay");
                    if !c.playlist.is_empty() {
                        if noplay {
                            let j = jump.min(c.playlist.len() - 1);
                            c.reset_song_queue(Some(j));
                        } else {
                            drop(c);
                            if !p.power() {
                                server.set_power(&p, true, true);
                            }
                            let cc = ctrl(&p);
                            let mut c = cc.lock();
                            let j = jump.min(c.playlist.len() - 1);
                            c.play(Some(j), None, fade);
                            playlist_changed(&mut c);
                            drop(c);
                            server
                                .notifier
                                .notify(Some(p.id), &["playlist", "load_done"]);
                            r.set_done();
                            return r;
                        }
                    }
                }
            }
            "add" | "append" => {
                c.playlist.append(entries);
                c.playlist.modified = true;
                c.next_if_streamed();
            }
            "insert" | "insertlist" => {
                c.playlist.insert_after_current(entries);
                c.playlist.modified = true;
                c.next_if_streamed();
            }
            _ => {}
        }
        playlist_changed(&mut c);
    }
    server
        .notifier
        .notify(Some(p.id), &["playlist", "load_done"]);
    r.set_done();
    r
}

/// Put `entries` into the queue: `cmd` is `load`/`play` (replace and play from
/// `play_index`), `add`, `insert` (after the current track) or `delete`.  Returns the
/// queue index of the first entry.
pub fn apply_tracks(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: &str,
    entries: Vec<Entry>,
    play_index: Option<usize>,
    fade: Option<f64>,
) -> usize {
    let how = match cmd {
        "load" | "play" => Enqueue::Play {
            start: play_index,
            fade,
        },
        "add" => Enqueue::Add,
        "insert" => Enqueue::Next,
        "delete" => {
            let urls: Vec<String> = entries.into_iter().map(|e| e.url).collect();
            let c = ctrl(p);
            let mut c = c.lock();
            loop {
                let pos = c
                    .playlist
                    .iter()
                    .find(|(_, e)| urls.contains(&e.url))
                    .map(|(i, _)| i);
                match pos {
                    Some(i) => remove_track(&mut c, i),
                    None => break,
                }
            }
            c.playlist.modified = true;
            playlist_changed(&mut c);
            return 0;
        }
        _ => return 0,
    };
    run(server, p, PlayerCommand::Enqueue { entries, how })
        .ok()
        .and_then(|o| o.index)
        .unwrap_or(0)
}

/// `playlist <cmd>tracks <what> [listref] [fadein] [index]`.
pub async fn playlist_xtracks(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    let cmd = r.term(1).trim_end_matches("tracks").to_string();
    let what = r.param("_what").unwrap_or("").to_string();
    let listref = r.param("_listref").unwrap_or("").to_string();
    let entries: Vec<Entry> = if what == "urllist" {
        listref
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|u| server.entry_for_url(&to_url(u)))
            .collect()
    } else {
        // `key=value&key=value` search terms (also `album.id:12`)
        let terms = if what.contains('=') {
            what.clone()
        } else {
            what.replacen(':', "=", 1)
        };
        let mut f = rms_library::TrackFilter::default();
        for kv in terms.split('&') {
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            let v = crate::util::cli_unescape(v);
            match k {
                "album.id" | "album" | "me.album" => f.album_id = Some(v),
                "contributor.id" | "contributor" => f.artist_id = v.parse().ok(),
                "genre.id" | "genre" => f.genre_id = Some(v),
                "year.id" | "year" => f.year = v.parse().ok(),
                "playlist.id" | "playlist" => f.playlist_id = v.parse().ok(),
                "track.id" | "me.id" => {
                    f.track_ids = v.split(',').filter_map(|x| x.parse().ok()).collect()
                }
                "track.titlesearch" | "contributor.namesearch" | "album.titlesearch" => {
                    f.search = Some(v)
                }
                "sort" => f.sort = Some(v),
                "contributorTracks.role" => f.role_id = Some(v),
                _ => {}
            }
        }
        let lib = server.library.clone();
        let tracks = tokio::task::spawn_blocking(move || lib.tracks_for(&f))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        tracks.iter().map(|t| server.entry_for_track(t)).collect()
    };
    let cmd = if cmd == "play" {
        "load".to_string()
    } else {
        cmd
    };
    let idx = apply_tracks(
        &server,
        &p,
        &cmd,
        entries,
        r.param_i64("_index").map(|i| i.max(0) as usize),
        r.param_f64("_fadein"),
    );
    r.add_result("index", idx as i64);
    r.set_done();
    r
}

/// `playlist loadalbum|playalbum|addalbum|insertalbum|deletealbum <genre> <artist> <album> [title]`.
pub async fn playlist_xalbum(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    let cmd = r.term(1).trim_end_matches("album").to_string();
    let pick = |k: &str| {
        r.param(k)
            .filter(|v| !v.is_empty() && *v != "*")
            .map(str::to_string)
    };
    let (genre, artist, album, title) = (
        pick("_genre"),
        pick("_artist"),
        pick("_album"),
        pick("_title"),
    );
    let lib = server.library.clone();
    let search = [album.clone(), artist.clone(), title.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let tracks = tokio::task::spawn_blocking(move || {
        lib.tracks_for(&rms_library::TrackFilter {
            search: Some(search),
            ..Default::default()
        })
    })
    .await
    .ok()
    .and_then(|r| r.ok())
    .unwrap_or_default();
    let matches = |field: &Option<String>, val: &Option<String>| match (field, val) {
        (Some(f), Some(v)) => v.to_lowercase().contains(&f.to_lowercase()),
        (Some(_), None) => false,
        (None, _) => true,
    };
    let entries: Vec<Entry> = tracks
        .iter()
        .filter(|t| {
            matches(&genre, &t.genre)
                && matches(&artist, &t.artist)
                && matches(&album, &t.album)
                && matches(&title, &Some(t.title.clone()))
        })
        .map(|t| server.entry_for_track(t))
        .collect();
    let cmd = if cmd == "play" {
        "load".to_string()
    } else {
        cmd
    };
    apply_tracks(&server, &p, &cmd, entries, None, None);
    r.set_done();
    r
}

/// `playlistcontrol cmd:<load|add|insert|delete> <selectors>`.
pub async fn playlistcontrol(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    let cmd = r.param("cmd").unwrap_or("").to_string();
    if !matches!(cmd.as_str(), "load" | "add" | "insert" | "delete") {
        r.bad_params();
        return r;
    }
    if server.library.is_scanning() {
        r.add_result("rescan", 1);
    }
    let play_index = r.param_i64("play_index").map(|i| i.max(0) as usize);
    if let Some(folder) = r.param("folder_id").map(str::to_string) {
        if cmd == "delete" {
            r.bad_params();
            return r;
        }
        // folder ids are library ids; resolve through the library
        let f = rms_library::TrackFilter {
            folder_id: folder.parse().ok(),
            ..Default::default()
        };
        let lib = server.library.clone();
        let tracks = tokio::task::spawn_blocking(move || lib.tracks_for(&f))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        let entries: Vec<Entry> = tracks.iter().map(|t| server.entry_for_track(t)).collect();
        apply_tracks(&server, &p, &cmd, entries, play_index, None);
        r.add_result("count", 1);
        r.set_done();
        return r;
    }
    let mut f = rms_library::TrackFilter::default();
    if let Some(name) = r.param("playlist_name") {
        let lib = server.library.clone();
        let name = name.to_string();
        let id = tokio::task::spawn_blocking(move || {
            lib.query(&rms_library::LibQuery {
                name: rms_library::LibQueryName::Playlists,
                start: Some(0),
                quantity: Some(100000),
                params: [("search".to_string(), name.clone())].into_iter().collect(),
            })
            .ok()
            .and_then(|row| row.get("playlists_loop").cloned())
            .and_then(|v| v.as_array().cloned())
            .and_then(|a| {
                a.into_iter()
                    .find(|i| i.get("playlist").and_then(|x| x.as_str()) == Some(name.as_str()))
            })
            .and_then(|i| i.get("id").and_then(|x| x.as_i64()))
        })
        .await
        .ok()
        .flatten();
        f.playlist_id = id;
    }
    if let Some(id) = r.param_i64("playlist_id") {
        f.playlist_id = Some(id);
    }
    if let Some(ids) = r.param("track_id") {
        let ids: Vec<i64> = ids
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        // negative ids are remote tracks in the current playlist
        let (remote, local): (Vec<i64>, Vec<i64>) = ids.iter().partition(|i| **i < 0);
        if !remote.is_empty() {
            let c = p.controller();
            let urls: Vec<Entry> = {
                let c = c.lock();
                c.playlist
                    .entries
                    .iter()
                    .filter(|e| remote.contains(&super::songdata::remote_id(&e.url)))
                    .cloned()
                    .collect()
            };
            let n = urls.len();
            apply_tracks(&server, &p, &cmd, urls, play_index, None);
            r.add_result("count", n as i64);
            r.set_done();
            return r;
        }
        f.track_ids = local;
    }
    f.album_id = r.param("album_id").map(str::to_string);
    f.artist_id = r.param_i64("artist_id");
    f.genre_id = r.param("genre_id").map(str::to_string);
    f.year = r.param_i64("year").or(r.param_i64("year_id"));
    f.role_id = r.param("role_id").map(str::to_string);
    f.sort = r.param("sort").map(str::to_string);
    let lib = server.library.clone();
    let tracks = tokio::task::spawn_blocking(move || lib.tracks_for(&f))
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let n = tracks.len();
    if n > 0 {
        let entries: Vec<Entry> = tracks.iter().map(|t| server.entry_for_track(t)).collect();
        apply_tracks(&server, &p, &cmd, entries, play_index, None);
    }
    r.add_result("count", n as i64);
    r.set_done();
    r
}

pub async fn playlist_preview(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    if r.param("cmd") == Some("stop") {
        ctrl(&p).lock().stop();
        r.set_done();
        return r;
    }
    let Some(url) = r.param("url").map(str::to_string) else {
        r.bad_dispatch();
        return r;
    };
    let entries = expand_item(&server, &url).await;
    apply_tracks(&server, &p, "load", entries, None, r.param_f64("fadein"));
    r.set_done();
    r
}

// ---- saved playlists ---------------------------------------------------------------------

pub fn playlists_new(server: &Arc<Server>, r: &mut Request) {
    let Some(name) = r.param_nonempty("name").map(str::to_string) else {
        return r.bad_params();
    };
    match server.library.save_playlist(&name, &[]) {
        Ok(id) => r.add_result("playlist_id", id),
        Err(_) => return r.status = Status::BadConfig,
    }
    r.set_done();
}

pub fn playlists_delete(server: &Arc<Server>, r: &mut Request) {
    let Some(id) = r.param_i64("playlist_id") else {
        return r.bad_params();
    };
    if server.library.delete_playlist(id).is_err() {
        return r.bad_params();
    }
    r.set_done();
}

pub fn playlists_rename(server: &Arc<Server>, r: &mut Request) {
    let (Some(id), Some(name)) = (
        r.param_i64("playlist_id"),
        r.param_nonempty("newname").map(str::to_string),
    ) else {
        return r.bad_params();
    };
    if r.param_bool("dry_run") {
        return r.set_done();
    }
    if server.library.rename_playlist(id, &name).is_err() {
        return r.bad_params();
    }
    r.set_done();
}

pub fn playlists_edit(server: &Arc<Server>, r: &mut Request) {
    let Some(id) = r.param_i64("playlist_id") else {
        return r.bad_params();
    };
    let Some((name, _url, mut urls)) = server.library.playlist_by_id(id) else {
        return r.bad_params();
    };
    let idx = r.param_i64("index").map(|i| i.max(0) as usize);
    match r.param("cmd").unwrap_or("") {
        "delete" => {
            let Some(i) = idx.filter(|i| *i < urls.len()) else {
                return r.bad_params();
            };
            urls.remove(i);
        }
        "up" => {
            let Some(i) = idx.filter(|i| *i > 0 && *i < urls.len()) else {
                return r.bad_params();
            };
            urls.swap(i, i - 1);
        }
        "down" => {
            let Some(i) = idx.filter(|i| i + 1 < urls.len()) else {
                return r.bad_params();
            };
            urls.swap(i, i + 1);
        }
        "move" => {
            let (Some(i), Some(to)) = (idx, r.param_i64("toindex").map(|t| t.max(0) as usize))
            else {
                return r.bad_params();
            };
            if i >= urls.len() || to >= urls.len() {
                return r.bad_params();
            }
            let u = urls.remove(i);
            urls.insert(to, u);
        }
        "add" => {
            let Some(u) = r.param("url").map(to_url) else {
                return r.bad_params();
            };
            if !urls.contains(&u) {
                urls.push(u);
            }
        }
        _ => return r.bad_params(),
    }
    let _ = server.library.save_playlist(&name, &urls);
    r.set_done();
}

// ---- alarms ------------------------------------------------------------------------------

/// `alarm <cmd> [tags]` — stored in the player's `alarms` pref (see `plugins::alarm`).
pub fn alarm(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let mut cmd = r.param("_cmd").unwrap_or("").to_string();
    if let Some((k, v)) = cmd.clone().split_once(':') {
        r.add_param(k, v);
        cmd = r.param("cmd").unwrap_or("").to_string();
    }
    match crate::plugins::alarm::command(server, &p, &cmd, r) {
        Ok(Some(id)) => r.add_result("id", id),
        Ok(None) => {}
        Err(_) => return r.bad_params(),
    }
    r.set_done();
}
