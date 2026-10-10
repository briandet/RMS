//! Commands (`Slim::Control::Commands`).

use super::request::{Request, Status};
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

/// Mark the playlist as changed: timestamp, persistence, keep streaming going.
pub(crate) fn playlist_changed(c: &mut crate::player::controller::Controller) {
    c.playlist.timestamp = crate::util::now();
    c.save_playlist();
}

// ---- play control ------------------------------------------------------------------------

/// `play`, `pause`, `stop`, `mode <x>` (`playcontrolCommand`).
pub fn playcontrol(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let curmode = c.lock().mode();
    let mut want = match r.term(0) {
        "play" => "play",
        "stop" => "stop",
        "mode" => r.term(1),
        "pause" => match r.param("_newvalue") {
            Some("1") => "pause",
            Some("0") => "play",
            _ => {
                if curmode == "pause" || curmode == "stop" {
                    "play"
                } else {
                    "pause"
                }
            }
        },
        _ => return r.bad_dispatch(),
    }
    .to_string();
    if r.term(0) == "pause" && r.param("_newvalue") == Some("1") && curmode != "play" {
        return r.set_done();
    }
    let fade = r.param_f64("_fadein");
    if want == "play" && curmode == "pause" {
        want = "resume".into();
    }
    if want != curmode {
        if !p.power() && (want == "play" || want == "resume") {
            server.set_power(&p, true, true);
        }
        let mut c = c.lock();
        match want.as_str() {
            "play" => {
                if !c.playlist.is_empty() {
                    let idx = c.playing_song_index().min(c.playlist.len() - 1);
                    c.play(Some(idx), None, fade);
                    drop(c);
                    server
                        .notifier
                        .notify(Some(p.id), &["playlist", "jump", &idx.to_string()]);
                }
            }
            "resume" => c.resume(fade),
            "pause" => c.pause(),
            "stop" => c.stop(),
            _ => {}
        }
    }
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
    if t.parse::<f64>().is_err() {
        return r.bad_params();
    }
    ctrl(&p).lock().jump_to_time(&t, false);
    r.set_done();
}

pub fn power(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let new = match r.param("_newvalue") {
        Some("0") => false,
        Some("1") => true,
        _ => !p.power(),
    };
    if let Some(seq) = r.param_i64("seq_no") {
        p.state.lock().sequence_number = seq as u32;
    }
    if new == p.power() {
        return; // LMS: no status change → echoed, not notified
    }
    let noplay = r.param_bool("_noplay");
    server.set_power(&p, new, noplay);
    server.notifier.notify(
        Some(p.id),
        &["prefset", "server", "power", if new { "1" } else { "0" }],
    );
    r.set_done();
}

pub fn sleep(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(secs) = r.param_f64("_newvalue") else {
        return r.bad_params();
    };
    let mut s = p.state.lock();
    if secs > 0.0 {
        s.sleep_time = crate::util::now() + secs;
    } else {
        s.sleep_time = 0.0;
    }
    drop(s);
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
    let group: Vec<Arc<Player>> = if p.prefs.get_bool("syncVolume") {
        ctrl(&p)
            .lock()
            .all_players()
            .into_iter()
            .filter(|o| o.id != p.id && o.prefs.get_bool("syncVolume"))
            .collect()
    } else {
        Vec::new()
    };
    match entity.as_str() {
        "volume" => {
            let Some(v) = val else { return r.bad_params() };
            let relative = v.starts_with('+') || v.starts_with('-');
            let Ok(n) = v.parse::<f64>() else {
                return r.bad_params();
            };
            let mut old = p.volume();
            if p.is_muted() {
                old = 0;
            }
            let new = if relative { old as f64 + n } else { n }.round() as i32;
            let new = p.set_volume(new, false);
            for o in group {
                o.set_volume(new, false);
            }
            server.notifier.notify(
                Some(p.id),
                &["prefset", "server", "volume", &new.to_string()],
            );
        }
        "muting" => {
            let muted = match val.as_deref() {
                Some("0") => false,
                Some("1") => true,
                _ => !p.is_muted(),
            };
            p.set_muted(muted);
            for o in group {
                o.set_muted(muted);
            }
        }
        "bass" | "treble" | "pitch" | "stereoxl" => {
            let Some(v) = val else { return r.bad_params() };
            let relative = v.starts_with('+') || v.starts_with('-');
            let Ok(n) = v.parse::<i64>() else {
                return r.bad_params();
            };
            let old = p.prefs.get_i64(&entity);
            let new = if relative { old + n } else { n };
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
        p.set_name(&n);
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
        server.unsync(&p);
    } else {
        let buddy = server.resolve_client(&v).or_else(|| {
            v.parse::<usize>()
                .ok()
                .and_then(|i| server.connected_players().get(i).cloned())
        });
        if let Some(b) = buddy {
            if b.id != p.id {
                server.sync(&b, &p, r.param_bool("noRestart"));
            }
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
        super::ir::handle_ir(server, &p, c, 0);
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
    if server
        .scanning
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        info!("scan already running, request ignored");
        return;
    }
    server.apply_library_config();
    let s = server.clone();
    tokio::spawn(async move {
        let lib = s.library.clone();
        info!("library scan started: {:?}", mode);
        let res = tokio::task::spawn_blocking(move || lib.scan(mode)).await;
        s.scanning.store(false, std::sync::atomic::Ordering::SeqCst);
        match res {
            Ok(Ok(summary)) => info!("library scan finished: {:?}", summary),
            Ok(Err(e)) => warn!("library scan failed: {}", e),
            Err(e) => warn!("library scan panicked: {}", e),
        }
        s.notifier.notify(None, &["rescan", "done"]);
        s.notifier.notify(None, &["library", "changed", "1"]);
    });
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
    server.library.set_rating(&item, rating.clamp(0, 100));
    r.set_done();
}

// ---- playlist ------------------------------------------------------------------------------

pub fn playlist_clear(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let mut c = c.lock();
    c.stop();
    c.playlist.clear();
    c.queue.clear();
    playlist_changed(&mut c);
    r.set_done();
}

pub fn playlist_delete(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(idx) = r.param_i64("_index") else {
        return r.bad_params();
    };
    let c = ctrl(&p);
    let mut c = c.lock();
    remove_track(&mut c, idx.max(0) as usize);
    playlist_changed(&mut c);
    r.set_done();
}

/// `Slim::Player::Playlist::removeTrack`: stop/skip appropriately when the playing song goes.
fn remove_track(c: &mut crate::player::controller::Controller, idx: usize) {
    if idx >= c.playlist.len() {
        return;
    }
    let playing = c.playing_song_index();
    let streaming = c.streaming_song_index();
    let was_stopped = c.is_stopped();
    if idx == playing && !was_stopped {
        if c.playlist.len() == 1 {
            c.stop();
        } else {
            c.playlist.delete(idx);
            // the song that took its place plays next
            let next = idx.min(c.playlist.len() - 1);
            let paused = c.is_paused();
            c.play(Some(next), None, None);
            if paused {
                c.pause();
            }
            return;
        }
    } else if idx == streaming && idx != playing && !was_stopped {
        c.flush();
    }
    c.playlist.delete(idx);
    for s in c.queue.iter_mut() {
        if s.index > idx {
            s.index -= 1;
        }
    }
    if c.playlist.is_empty() {
        c.queue.clear();
        c.stop();
    }
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
    let c = ctrl(&p);
    let mut c = c.lock();
    let (from, to) = (from.max(0) as usize, to.max(0) as usize);
    c.playlist.move_song(from, to);
    // keep queue indexes consistent with the moved positions
    for s in c.queue.iter_mut() {
        if s.index == from {
            s.index = to;
        } else if from < s.index && to >= s.index {
            s.index -= 1;
        } else if from > s.index && to <= s.index {
            s.index += 1;
        }
    }
    playlist_changed(&mut c);
    r.set_done();
}

pub fn playlist_repeat(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let mut c = c.lock();
    let new = match r.param_i64("_newvalue") {
        Some(v) => v.clamp(0, 2) as u8,
        None => [1u8, 2, 0][c.playlist.repeat as usize % 3],
    };
    c.playlist.repeat = new;
    c.save_playlist();
    drop(c);
    server.notifier.notify(
        Some(p.id),
        &["prefset", "server", "repeat", &new.to_string()],
    );
    r.set_done();
}

pub fn playlist_shuffle(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let mut c = c.lock();
    let new = match r.param_i64("_newvalue") {
        Some(v) => v.clamp(0, 2) as u8,
        None => [1u8, 2, 0][c.playlist.shuffle as usize % 3],
    };
    let playing = !c.is_stopped();
    let cur_entry = c.playlist.order.get(c.playing_song_index()).copied();
    c.playlist.current = c
        .playing_song_index()
        .min(c.playlist.len().saturating_sub(1));
    c.playlist.set_shuffle(new, playing);
    // re-point queue songs at their new positions
    let order = c.playlist.order.clone();
    if let Some(ce) = cur_entry {
        let newpos = order.iter().position(|&i| i == ce).unwrap_or(0);
        for s in c.queue.iter_mut() {
            s.index = newpos;
        }
        c.playlist.current = newpos;
        if playing {
            // drop any already-streamed next song so the new order is followed
            while c.queue.len() > 1 {
                c.queue.pop_front();
            }
            if c.streaming_state != crate::player::controller::StreamingState::Streaming {
                c.flush();
            }
        }
    }
    playlist_changed(&mut c);
    drop(c);
    server.notifier.notify(
        Some(p.id),
        &["prefset", "server", "shuffle", &new.to_string()],
    );
    r.set_done();
}

/// `playlist index|jump <index|+N|-N> [fadein] [noplay] [seekdata]`.
pub fn playlist_jump(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = ctrl(&p);
    let count = c.lock().playlist.len();
    if count == 0 {
        return;
    }
    if !p.power() {
        server.set_power(&p, true, true);
    }
    let idx_s = r.param("_index").unwrap_or("0").to_string();
    let fade = r.param_f64("_fadein");
    let noplay = r.param_bool("_noplay");
    let seek = r
        .param("_seekdata")
        .and_then(|s| s.split(':').next_back().and_then(|t| t.parse::<f64>().ok()));
    let mut c = c.lock();
    let relative = idx_s.starts_with('+') || idx_s.starts_with('-');
    let n: i64 = idx_s.parse().unwrap_or(0);
    let mut new_index: i64;
    if relative {
        if !c.is_stopped() {
            if n == 0 || (n == -1 && count == 1) {
                c.jump_to_time("0", true);
                return r.set_done();
            }
            if n == 1 {
                c.skip();
                return r.set_done();
            }
        }
        new_index = c.playing_song_index() as i64 + n;
        if new_index >= count as i64
            && c.playlist.shuffle != 0
            && c.playlist.repeat == 2
            && server.prefs.server.get_bool("reshuffleOnRepeat")
        {
            c.playlist.reshuffle(false);
        }
    } else {
        new_index = n;
    }
    new_index = new_index.rem_euclid(count as i64);
    let new_index = new_index as usize;
    if noplay && c.is_stopped() {
        c.reset_song_queue(Some(new_index));
    } else {
        c.play(Some(new_index), seek, fade);
    }
    c.save_playlist();
    r.set_done();
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
    if crate::util::is_remote_url(&url) {
        return vec![Entry::new(url)];
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

/// Add tracks to the playlist with LMS `playlistXtracksCommand` semantics.
/// `cmd` is `load|play|add|insert|delete`.  Returns the result `index`.
pub fn apply_tracks(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: &str,
    entries: Vec<Entry>,
    play_index: Option<usize>,
    fade: Option<f64>,
) -> usize {
    let c = ctrl(p);
    let mut c = c.lock();
    let index;
    match cmd {
        "load" | "play" => {
            c.stop();
            c.playlist.clear();
            c.queue.clear();
            c.playlist.append(entries);
            c.playlist.modified = false;
            index = 0;
            if !c.playlist.is_empty() {
                let mut j = play_index.unwrap_or(0).min(c.playlist.len() - 1);
                if c.playlist.shuffle != 0 && play_index.is_some() {
                    c.playlist.jump_to_entry(j);
                    j = c.playlist.current;
                }
                drop(c);
                if !p.power() {
                    server.set_power(p, true, true);
                }
                let cc = p.controller();
                let mut c = cc.lock();
                c.play(Some(j), None, fade);
                playlist_changed(&mut c);
                drop(c);
                server
                    .notifier
                    .notify(Some(p.id), &["playlist", "jump", &j.to_string()]);
                return index;
            }
        }
        "add" => {
            let n = entries.len();
            c.playlist.append(entries);
            index = c.playlist.len() - n;
            c.playlist.modified = true;
            c.next_if_streamed();
        }
        "insert" => {
            index = c.streaming_song_index() + 1;
            c.playlist.insert_after_current(entries);
            c.playlist.modified = true;
            c.next_if_streamed();
            drop(c);
            server
                .notifier
                .notify(Some(p.id), &["playlist", "load_done"]);
            let cc = p.controller();
            let mut c = cc.lock();
            playlist_changed(&mut c);
            return index;
        }
        "delete" => {
            let urls: Vec<String> = entries.into_iter().map(|e| e.url).collect();
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
            index = 0;
        }
        _ => index = 0,
    }
    playlist_changed(&mut c);
    index
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
                "album.id" | "album" | "me.album" => f.album_id = v.parse().ok(),
                "contributor.id" | "contributor" => f.artist_id = v.parse().ok(),
                "genre.id" | "genre" => f.genre_id = v.parse().ok(),
                "year.id" | "year" => f.year = v.parse().ok(),
                "playlist.id" | "playlist" => f.playlist_id = v.parse().ok(),
                "track.id" | "me.id" => {
                    f.track_ids = v.split(',').filter_map(|x| x.parse().ok()).collect()
                }
                "track.titlesearch" | "contributor.namesearch" | "album.titlesearch" => {
                    f.search = Some(v)
                }
                "sort" => f.sort = Some(v),
                "library_id" => f.library_id = Some(v),
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
                && matches(&title, &t.title)
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
    f.album_id = r.param_i64("album_id");
    f.artist_id = r.param_i64("artist_id");
    f.genre_id = r.param_i64("genre_id");
    f.year = r.param_i64("year").or(r.param_i64("year_id"));
    f.role_id = r.param("role_id").map(str::to_string);
    f.library_id = r.param("library_id").map(str::to_string);
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
