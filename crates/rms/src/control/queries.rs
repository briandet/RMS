//! Queries (`Slim::Control::Queries`) that do not hit the music library.

use super::commands::player;
use super::request::{normalize, Request, Results};
use super::songdata::{proxied_image, song_row};
use crate::player::Player;
use crate::server::Server;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn version(_: &Arc<Server>, r: &mut Request) {
    r.add_result("_version", crate::VERSION);
    r.set_done();
}

pub fn connected(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    r.add_result("_connected", p.is_connected() as i32);
    r.set_done();
}

pub fn power(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    r.add_result("_power", p.power() as i32);
    r.set_done();
}

pub fn mode(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let m = p.controller().lock().mode();
    r.add_result("_mode", m);
    r.set_done();
}

pub fn time(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let t = p.controller().lock().playing_song_elapsed();
    r.add_result("_time", t);
    r.set_done();
}

pub fn sleep(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let st = p.state.lock().sleep_time;
    let rem = if st > 0.0 {
        (st - crate::util::now()).max(0.0)
    } else {
        0.0
    };
    r.add_result("_sleep", rem);
    r.set_done();
}

pub fn signalstrength(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let s = p.state.lock().signal_strength;
    r.add_result("_signalstrength", if s > 100 { 0 } else { s });
    r.set_done();
}

pub fn name(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    r.add_result("_value", p.name());
    r.set_done();
}

pub fn irenable(server: &Arc<Server>, r: &mut Request) {
    if player(server, r).is_none() {
        return r.bad_dispatch();
    }
    r.add_result("_irenable", 1);
    r.set_done();
}

pub fn linesperscreen(server: &Arc<Server>, r: &mut Request) {
    if player(server, r).is_none() {
        return r.bad_dispatch();
    }
    r.add_result("_linesperscreen", 2);
    r.set_done();
}

pub fn display(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let lines = p
        .state
        .lock()
        .show_briefly
        .clone()
        .map(|(l, _)| l)
        .unwrap_or_default();
    r.add_result("_line1", lines.first().cloned().unwrap_or_default());
    r.add_result("_line2", lines.get(1).cloned().unwrap_or_default());
    r.set_done();
}

pub fn sync(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let others: Vec<String> = p
        .controller()
        .lock()
        .all_players()
        .into_iter()
        .filter(|o| o.id != p.id)
        .map(|o| o.id.to_string())
        .collect();
    r.add_result(
        "_sync",
        if others.is_empty() {
            "-".to_string()
        } else {
            others.join(",")
        },
    );
    r.set_done();
}

pub fn syncgroups(server: &Arc<Server>, r: &mut Request) {
    for g in server.sync_groups() {
        let mut item = Results::new();
        item.insert(
            "sync_members".into(),
            json!(g
                .iter()
                .map(|p| p.id.to_string())
                .collect::<Vec<_>>()
                .join(",")),
        );
        item.insert(
            "sync_member_names".into(),
            json!(g.iter().map(|p| p.name()).collect::<Vec<_>>().join(",")),
        );
        r.add_loop_item("syncgroups_loop", item);
    }
    r.set_done();
}

pub fn mixer(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    match r.term(1) {
        "volume" => {
            let v = p.volume();
            r.add_result("_volume", if p.is_muted() { -v } else { v });
        }
        "muting" => r.add_result("_muting", p.is_muted() as i32),
        "bass" => r.add_result("_bass", p.prefs.get_i64("bass")),
        "treble" => r.add_result("_treble", p.prefs.get_i64("treble")),
        "pitch" => r.add_result("_pitch", p.prefs.get_i64("pitch")),
        _ => return r.bad_dispatch(),
    }
    r.set_done();
}

pub fn rescan(server: &Arc<Server>, r: &mut Request) {
    r.add_result("_rescan", server.library.is_scanning() as i32);
    r.set_done();
}

pub fn rescanprogress(server: &Arc<Server>, r: &mut Request) {
    let p = server.library.progress();
    if p.running || server.library.is_scanning() {
        r.add_result("rescan", 1);
        if !p.step.is_empty() {
            let pct = if p.total > 0 {
                (p.done * 100 / p.total) as i64
            } else {
                -1
            };
            r.add_result(&p.step, pct);
            r.add_result("steps", p.step.clone());
        }
        let t = p.total_time as u64;
        r.add_result(
            "totaltime",
            format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60),
        );
    } else {
        r.add_result("rescan", 0);
    }
    r.set_done();
}

pub fn debug(_: &Arc<Server>, r: &mut Request) {
    r.add_result("_value", "INFO");
    r.set_done();
}

pub fn getstring(_: &Arc<Server>, r: &mut Request) {
    let tokens = r.param("_tokens").unwrap_or("").to_string();
    for t in tokens.split(',') {
        r.add_result(t, crate::strings::get(t).unwrap_or(""));
    }
    r.set_done();
}

pub fn info_total(server: &Arc<Server>, r: &mut Request) {
    let t = server.library.totals();
    match r.term(2) {
        "albums" => r.add_result("_albums", t.albums),
        "artists" => r.add_result("_artists", t.artists),
        "genres" => r.add_result("_genres", t.genres),
        "songs" => r.add_result("_songs", t.songs),
        "duration" => r.add_result("_duration", t.duration),
        _ => return r.bad_dispatch(),
    }
    r.set_done();
}

pub fn libraries(server: &Arc<Server>, r: &mut Request) {
    if r.term(1) == "getid" {
        r.add_result("id", 0);
    } else {
        let _ = server;
    }
    r.set_done();
}

pub fn can(server: &Arc<Server>, r: &mut Request) {
    let mut terms = Vec::new();
    for k in ["_p1", "_p2", "_p3", "_p4", "_p5"] {
        match r.param(k) {
            Some(v) if !v.is_empty() && v != "?" => terms.push(v.to_string()),
            _ => {
                r.delete_param(k);
            }
        }
    }
    let ok = matches!(
        terms.first().map(String::as_str),
        Some("login" | "shutdown" | "exit")
    ) || server.dispatch.can(&terms);
    r.add_result("_can", ok as i32);
    r.set_done();
}

pub fn pref(server: &Arc<Server>, r: &mut Request) {
    let Some(name) = r.param("_prefname").map(str::to_string) else {
        return r.bad_params();
    };
    let (ns, pname) = match name.split_once(':') {
        Some((n, p)) => (n.to_string(), p.to_string()),
        None => ("server".into(), name.clone()),
    };
    let v = if r.term(0) == "playerpref" {
        let Some(p) = player(server, r) else {
            return r.bad_dispatch();
        };
        match pname.as_str() {
            "playername" => Some(json!(p.name())),
            _ => p.prefs.get(&pname),
        }
    } else if ns == "server" {
        server.prefs.server.get(&pname)
    } else if ns == "plugin.cli" && pname == "cliport" {
        Some(json!(server.cli_port()))
    } else {
        server.prefs.server.get(&format!("{}:{}", ns, pname))
    };
    r.add_result("_p2", v.unwrap_or(Value::Null));
    r.set_done();
}

pub fn pref_validate(_: &Arc<Server>, r: &mut Request) {
    r.add_result("valid", 1);
    r.set_done();
}

pub fn artwork(_: &Arc<Server>, r: &mut Request) {
    let id = r.param("_artworkid").unwrap_or("").to_string();
    if id.contains("://") {
        r.add_result("artworkUrl", proxied_image(&id));
    } else {
        r.add_result("artworkId", id);
    }
    r.add_result("offset", 0);
    r.set_done();
}

pub fn rating(server: &Arc<Server>, r: &mut Request) {
    let Some(item) = r.param("_item").map(str::to_string) else {
        return r.bad_params();
    };
    let t = item
        .parse::<i64>()
        .ok()
        .and_then(|id| server.library.track(id))
        .or_else(|| server.library.track_by_url(&super::commands::to_url(&item)));
    let Some(t) = t else { return r.bad_params() };
    let row = server.library.track_row(&t, "R");
    r.add_result("_rating", row.get("rating").cloned().unwrap_or(json!(0)));
    r.set_done();
}

pub fn readdirectory(_: &Arc<Server>, r: &mut Request) {
    let folder = r.param("folder").unwrap_or("/").to_string();
    let filter = r.param("filter").unwrap_or("").to_string();
    let mut items: Vec<(String, String, bool)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&folder) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = e.path().is_dir();
            let keep = match filter.as_str() {
                "foldersonly" => is_dir,
                "filesonly" => !is_dir,
                "musicfiles" => {
                    is_dir
                        || crate::server::content_type_for_ext(&crate::util::extension(&e.path()))
                            != ""
                }
                f if f.starts_with("filetype:") => {
                    is_dir
                        || name
                            .to_lowercase()
                            .ends_with(&format!(".{}", &f[9..].to_lowercase()))
                }
                "" => true,
                f => regex::RegexBuilder::new(f)
                    .case_insensitive(true)
                    .build()
                    .map(|re| re.is_match(&e.path().to_string_lossy()))
                    .unwrap_or(true),
            };
            if keep {
                items.push((e.path().to_string_lossy().to_string(), name, is_dir));
            }
        }
    }
    items.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    r.add_result("count", items.len());
    let (valid, start, end) =
        normalize(r.param_i64("_index"), r.param_i64("_quantity"), items.len());
    if valid {
        for (path, name, dir) in &items[start..=end] {
            let mut i = Results::new();
            i.insert("path".into(), json!(path));
            i.insert("name".into(), json!(name));
            i.insert("isfolder".into(), json!(if *dir { 1 } else { 0 }));
            r.add_loop_item("fsitems_loop", i);
        }
    }
    r.set_done();
}

pub fn displaystatus(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let sb = p.state.lock().show_briefly.clone();
    if let Some((lines, until)) = sb {
        if until > crate::util::now() {
            r.add_result("type", "showbriefly");
            if matches!(r.source, super::request::Source::Cli(_)) {
                for (i, l) in lines.iter().enumerate() {
                    if !l.is_empty() {
                        r.add_result(&format!("line{}", i), l.clone());
                    }
                }
            } else {
                r.add_result("display", json!({"text": lines, "duration": ((until - crate::util::now()) * 1000.0) as i64}));
            }
        }
    }
    r.set_done();
}

// ---- current song / playlist entity queries -------------------------------------------------

pub fn cursonginfo(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = p.controller();
    let c = c.lock();
    let idx = c.playing_song_index();
    let Some(entry) = c.playlist.song(idx).cloned() else {
        return r.set_done();
    };
    let song = c.playing_song().cloned();
    drop(c);
    let key = r.term(0).to_string();
    match key.as_str() {
        "path" => r.add_result("_path", entry.url.clone()),
        "remote" => r.add_result("_remote", crate::util::is_remote_url(&entry.url) as i32),
        "current_title" => r.add_result(
            "_current_title",
            current_title(server, &entry, song.as_ref()),
        ),
        _ => {
            let row = song_row(server, &entry, song.as_ref(), "dalg");
            if let Some(v) = row.get(&key) {
                r.add_result(&format!("_{}", key), v.clone());
            }
        }
    }
    r.set_done();
}

/// `getCurrentTitle`: live stream title, else the track's standard title.
pub fn current_title(
    server: &Server,
    entry: &crate::player::playlist::Entry,
    song: Option<&crate::player::Song>,
) -> String {
    if let Some(s) = song {
        return crate::player::controller::standard_title(s);
    }
    let info = server.track_info_for(entry);
    info.title
        .unwrap_or_else(|| crate::player::controller::title_from_url(&entry.url))
}

pub fn playlist_x(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = p.controller();
    let c = c.lock();
    let entity = r.term(1).to_string();
    match entity.as_str() {
        "repeat" => r.add_result("_repeat", c.playlist.repeat),
        "shuffle" => r.add_result("_shuffle", c.playlist.shuffle),
        "index" | "jump" => r.add_result(&format!("_{}", entity), c.playing_song_index()),
        "tracks" => r.add_result("_tracks", c.playlist.len()),
        "modified" => r.add_result("_modified", c.playlist.modified as i32),
        "url" => {
            if let Some(u) = &c.playlist.url {
                r.add_result("_url", u.clone());
            }
        }
        "name" if c.playlist.name.is_some() => {
            r.add_result("_name", c.playlist.name.clone().unwrap())
        }
        "path" => {
            let idx = r.param_i64("_index").unwrap_or(0).max(0) as usize;
            r.add_result(
                "_path",
                c.playlist
                    .song(idx)
                    .map(|e| json!(e.url))
                    .unwrap_or(json!(0)),
            );
        }
        "remote" => {
            let idx = r.param_i64("_index").unwrap_or(0).max(0) as usize;
            if let Some(e) = c.playlist.song(idx) {
                r.add_result("_remote", crate::util::is_remote_url(&e.url) as i32);
            }
        }
        _ => {
            let idx = r
                .param_i64("_index")
                .map(|i| i.max(0) as usize)
                .unwrap_or_else(|| c.playing_song_index());
            let Some(e) = c.playlist.song(idx).cloned() else {
                return r.set_done();
            };
            let song = c.queue.iter().find(|s| s.index == idx).cloned();
            drop(c);
            let row = song_row(server, &e, song.as_ref(), "dalgN");
            let key = if entity == "name" {
                "title"
            } else {
                entity.as_str()
            };
            let v = row.get(key).cloned().or_else(|| {
                if entity == "name" {
                    row.get("remote_title").cloned()
                } else {
                    None
                }
            });
            if let Some(v) = v {
                r.add_result(&format!("_{}", entity), v);
            }
            return r.set_done();
        }
    }
    r.set_done();
}

pub fn playlist_playlistsinfo(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let c = p.controller();
    let c = c.lock();
    if let (Some(name), Some(url)) = (&c.playlist.name, &c.playlist.url) {
        r.add_result("name", name.clone());
        r.add_result("modified", c.playlist.modified as i32);
        r.add_result("url", url.clone());
    }
    r.set_done();
}

// ---- players / serverstatus ------------------------------------------------------------------

pub fn player_x(server: &Arc<Server>, r: &mut Request) {
    let entity = r.term(1).to_string();
    if entity == "count" {
        r.add_result("_count", server.players().len());
        return r.set_done();
    }
    let id = r.param("_IDorIndex").unwrap_or("").to_string();
    let p = slimproto::MacAddress::parse(&id)
        .and_then(|m| server.player(&m))
        .or_else(|| {
            id.parse::<usize>()
                .ok()
                .and_then(|i| server.players().get(i).cloned())
        })
        .or_else(|| server.resolve_client(&id));
    let Some(p) = p else { return r.set_done() };
    let s = p.state.lock();
    let v: Value = match entity.as_str() {
        "name" => json!(s.name),
        "address" | "id" => json!(p.id.to_string()),
        "ip" => json!(s.addr.map(|a| a.to_string()).unwrap_or_default()),
        "model" => json!(s.caps.model),
        "isplayer" => json!(1),
        "displaytype" => json!(if s.caps.has_display {
            "graphic-320x32"
        } else {
            "none"
        }),
        "canpoweroff" => json!(1),
        "uuid" => json!(s.uuid),
        _ => return,
    };
    drop(s);
    r.add_result(&format!("_{}", entity), v);
    r.set_done();
}

fn player_item(p: &Arc<Player>, index: usize, playerprefs: &[String]) -> Results {
    let s = p.state.lock();
    let mut i = Results::new();
    i.insert("playerindex".into(), json!(index));
    i.insert("playerid".into(), json!(p.id.to_string()));
    i.insert("uuid".into(), json!(s.uuid));
    i.insert(
        "ip".into(),
        json!(s.addr.map(|a| a.to_string()).unwrap_or_default()),
    );
    i.insert("name".into(), json!(s.name));
    if s.sequence_number > 0 {
        i.insert("seq_no".into(), json!(s.sequence_number));
    }
    i.insert("model".into(), json!(s.caps.model));
    i.insert("modelname".into(), json!(s.caps.model_name));
    i.insert("power".into(), json!(s.power as i32));
    drop(s);
    let playing = p.controller().lock().is_playing(false);
    let s = p.state.lock();
    i.insert("isplaying".into(), json!(playing as i32));
    i.insert(
        "displaytype".into(),
        json!(if s.caps.has_display {
            "graphic-320x32"
        } else {
            "none"
        }),
    );
    i.insert("isplayer".into(), json!(1));
    i.insert("canpoweroff".into(), json!(1));
    i.insert("connected".into(), json!(s.connected as i32));
    i.insert("firmware".into(), json!(s.caps.firmware));
    drop(s);
    for pref in playerprefs {
        if let Some(v) = p.prefs.get(pref) {
            i.insert(pref.clone(), v);
        }
    }
    i
}

pub fn players(server: &Arc<Server>, r: &mut Request) {
    let list = server.players();
    r.add_result("count", list.len());
    let prefs: Vec<String> = r
        .param("playerprefs")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let (valid, start, end) =
        normalize(r.param_i64("_index"), r.param_i64("_quantity"), list.len());
    if valid {
        for (n, p) in list[start..=end].iter().enumerate() {
            r.add_loop_item("players_loop", player_item(p, start + n, &prefs));
        }
    }
    r.set_done();
}

pub fn serverstatus(server: &Arc<Server>, r: &mut Request) {
    let lib = &server.library;
    if lib.is_scanning() {
        r.add_result("rescan", "1");
        let p = lib.progress();
        if !p.step.is_empty() {
            r.add_result("progressname", p.step.clone());
            r.add_result("progressdone", p.done);
            r.add_result("progresstotal", p.total);
        }
    } else if let Some(ls) = lib.progress().last_scan {
        r.add_result("lastscan", ls.to_string());
    }
    r.add_result("version", crate::VERSION);
    r.add_result("uuid", server.uuid());
    r.add_result("ip", crate::util::server_ip());
    r.add_result("httpport", server.http_port().to_string());
    let t = lib.totals();
    r.add_result("info total albums", t.albums);
    r.add_result("info total artists", t.artists);
    r.add_result("info total genres", t.genres);
    r.add_result("info total songs", t.songs);
    r.add_result("info total duration", t.duration);
    if let Some(prefs) = r.param("prefs").map(str::to_string) {
        for pname in prefs.split(',') {
            if let Some(v) = server.prefs.server.get(pname) {
                r.add_result(pname, v);
            }
        }
    }
    let list = server.players();
    r.add_result("player count", list.len());
    let pprefs: Vec<String> = r
        .param("playerprefs")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let (valid, start, end) =
        normalize(r.param_i64("_index"), r.param_i64("_quantity"), list.len());
    if valid {
        for (n, p) in list[start..=end].iter().enumerate() {
            r.add_loop_item("players_loop", player_item(p, start + n, &pprefs));
        }
    }
    r.add_result("other player count", 0);
    r.set_done();
}

// ---- status ----------------------------------------------------------------------------------

pub fn status(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        r.add_result("error", "invalid player");
        return r.set_done();
    };
    let menu_mode = r.param("menu").is_some();
    let use_context_menu = r.param_bool("useContextMenu");
    let mut tags = r.param("tags").unwrap_or("").to_string();
    let index_param = r.param("_index").map(str::to_string);
    let quantity = r.param_i64("_quantity");

    if server.library.is_scanning() {
        r.add_result("rescan", "1");
    }
    let (name, connected, ip, power, signal, seq, sleep_time) = {
        let s = p.state.lock();
        (
            s.name.clone(),
            s.connected,
            s.addr.map(|a| a.to_string()),
            s.power,
            s.signal_strength,
            s.sequence_number,
            s.sleep_time,
        )
    };
    if !name.is_empty() {
        r.add_result("player_name", name);
    }
    r.add_result("player_connected", connected as i32);
    if connected {
        r.add_result("player_ip", ip.unwrap_or_default());
    }
    r.add_result("power", power as i32);
    r.add_result("signalstrength", if signal > 100 { 0 } else { signal });

    let c = p.controller();
    let c = c.lock();
    r.add_result("mode", c.mode());
    if c.is_playing(false) && !c.is_playing(true) {
        r.add_result("waitingToPlay", 1);
    }
    let song = c.playing_song().cloned();
    if let Some(s) = &song {
        if s.is_remote() {
            r.add_result("remote", 1);
            r.add_result(
                "current_title",
                crate::player::controller::standard_title(s),
            );
        }
        r.add_result("time", c.playing_song_elapsed());
        r.add_result("rate", 1);
        if let Some(d) = s.duration() {
            r.add_result("duration", d);
        }
        if s.can_seek && s.duration().is_some() {
            r.add_result("can_seek", 1);
        }
        if s.replay_gain_db != 0.0 {
            r.add_result("replay_gain", s.replay_gain_db);
        }
        if s.spec
            .as_ref()
            .map(|sp| matches!(sp.source, crate::stream::SourceKind::Ffmpeg { .. }))
            .unwrap_or(false)
        {
            r.add_result("is_transcoded", 1);
        }
        if tags.contains('o') && !s.stream_format.is_empty() {
            r.add_result("type", s.stream_format.clone());
        }
        if tags.contains('b') {
            if let Some(b) = s.stream_bitrate {
                r.add_result(
                    "bitrate",
                    super::songdata::pretty_bitrate(b, s.track.vbr, &s.stream_format),
                );
            }
        }
        if tags.contains('T') {
            if let Some(sr) = s.track.samplerate {
                r.add_result("samplerate", sr);
            }
        }
        if tags.contains('I') {
            if let Some(ss) = s.track.samplesize {
                r.add_result("samplesize", ss);
            }
        }
    }
    if sleep_time > 0.0 {
        let rem = (sleep_time - crate::util::now()).max(0.0);
        r.add_result("sleep", rem.ceil());
        r.add_result("will_sleep_in", rem);
    }
    let members = c.all_players();
    if members.len() > 1 {
        r.add_result("sync_master", c.master_id.to_string());
        r.add_result(
            "sync_slaves",
            members
                .iter()
                .filter(|m| m.id != c.master_id)
                .map(|m| m.id.to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    let vol = p.volume();
    r.add_result("mixer volume", if p.is_muted() { -vol } else { vol });
    r.add_result("playlist repeat", c.playlist.repeat);
    r.add_result("playlist shuffle", c.playlist.shuffle);
    r.add_result("playlist mode", "off");
    if seq > 0 {
        r.add_result("seq_no", seq);
    }
    if let (Some(n), Some(u)) = (&c.playlist.name, &c.playlist.url) {
        if let Some(id) = server.library.track_by_url(u).map(|t| t.id) {
            r.add_result("playlist_id", id);
        }
        r.add_result("playlist_name", n.clone());
        r.add_result("playlist_modified", c.playlist.modified as i32);
    }
    let count = c.playlist.len();
    let cur = c.playing_song_index();
    if count > 0 {
        r.add_result("playlist_cur_index", cur);
        r.add_result("playlist_timestamp", c.playlist.timestamp);
    }
    r.add_result("playlist_tracks", count);
    let dvc = p.prefs.get_i64("digitalVolumeControl");
    r.add_result("digital_volume_control", dvc);
    let has_digital = p.state.lock().caps.has_digital_out;
    r.add_result("use_volume_control", (dvc != 0 || !has_digital) as i32);

    if menu_mode || r.param_bool("alarmData") {
        let (state, next) = crate::plugins::alarm::status_info(&p);
        r.add_result("alarm_state", state);
        r.add_result("alarm_next", next);
        r.add_result("alarm_version", 2);
        r.add_result(
            "alarm_snooze_seconds",
            p.prefs.get_i64("alarmSnoozeSeconds"),
        );
        r.add_result(
            "alarm_timeout_seconds",
            p.prefs.get_i64("alarmTimeoutSeconds"),
        );
    }
    if menu_mode {
        r.add_result("preset_loop", json!([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        r.add_result(
            "preset_data",
            json!([{}, {}, {}, {}, {}, {}, {}, {}, {}, {}]),
        );
        r.add_result("count", if count > 0 { count + 2 } else { 0 });
        let base = if use_context_menu {
            json!({"actions": {"more": {"player": 0, "cmd": ["contextmenu"], "params": {"menu": "track", "context": "playlist"}, "itemsParams": "params", "window": {"isContextMenu": 1}}}})
        } else {
            json!({"actions": {"go": {"cmd": ["trackinfo", "items"], "params": {"menu": "nowhere", "useContextMenu": 1, "context": "playlist"}, "itemsParams": "params"}}})
        };
        r.add_result("base", base);
    }

    if count == 0 {
        return r.set_done();
    }
    let total_only = !menu_mode && tags.contains("DD");
    if menu_mode {
        tags = "aAlKNcxJ".into();
    }
    if total_only {
        let mut total = 0.0;
        for (_, e) in c.playlist.iter() {
            total += server.track_info_for(e).duration().unwrap_or(0.0);
        }
        r.add_result("playlist duration", total);
        return r.set_done();
    }
    // paging
    let mut indexes: Vec<usize> = Vec::new();
    let modecurrent = index_param.as_deref() == Some("-");
    if modecurrent {
        let (valid, start, end) = normalize(Some(cur as i64), quantity, count);
        if valid {
            if c.playlist.repeat == 1 {
                indexes.push(cur);
            } else {
                indexes.extend(start..=end);
                let q = quantity.unwrap_or(0).max(0) as usize;
                if c.playlist.repeat == 2
                    && (c.playlist.shuffle == 0
                        || !server.prefs.server.get_bool("reshuffleOnRepeat"))
                    && indexes.len() < q
                {
                    let (v2, s2, e2) = normalize(Some(0), Some((q - indexes.len()) as i64), count);
                    if v2 {
                        indexes.extend(s2..=e2);
                    }
                }
            }
        }
        if menu_mode {
            r.add_result("offset", cur);
        }
    } else {
        let idx = index_param.as_deref().and_then(|s| s.parse::<i64>().ok());
        let (valid, start, end) = normalize(idx, quantity, count);
        if valid {
            indexes.extend(start..=end);
        }
        if menu_mode {
            r.add_result("offset", idx.unwrap_or(0));
        }
    }
    let entries: Vec<(
        usize,
        crate::player::playlist::Entry,
        Option<crate::player::Song>,
    )> = indexes
        .iter()
        .filter_map(|&i| {
            c.playlist
                .song(i)
                .cloned()
                .map(|e| (i, e, c.queue.iter().find(|s| s.index == i).cloned()))
        })
        .collect();
    let cur_entry = c.playlist.song(cur).cloned();
    drop(c);

    // remoteMeta for a current remote stream
    if let (Some(e), Some(s)) = (&cur_entry, &song) {
        if crate::util::is_remote_url(&e.url) && !menu_mode {
            let mut t = tags.clone();
            t.push('B');
            r.add_result(
                "remoteMeta",
                Value::Object(song_row(server, e, Some(s), &t)),
            );
        }
    }
    let loop_name = if menu_mode {
        "item_loop"
    } else {
        "playlist_loop"
    };
    for (i, e, s) in entries {
        let song_ref = if Some(i) == Some(cur) {
            song.as_ref().or(s.as_ref())
        } else {
            s.as_ref()
        };
        let row = song_row(server, &e, song_ref, &tags);
        if menu_mode {
            let item = jive_song(&row, i, crate::util::is_remote_url(&e.url));
            r.add_loop_item(loop_name, item);
            if i + 1 == count {
                add_jive_playlist_controls(r);
            }
        } else {
            let mut item = Results::new();
            item.insert("playlist index".into(), json!(i));
            item.extend(row);
            r.add_loop_item(loop_name, item);
        }
    }
    r.set_done();
}

/// `_addJiveSong`.
fn jive_song(row: &Results, index: usize, remote: bool) -> Results {
    let s = |k: &str| {
        row.get(k).and_then(|v| {
            v.as_str().map(str::to_string).or_else(|| {
                if v.is_number() {
                    Some(v.to_string())
                } else {
                    None
                }
            })
        })
    };
    let mut item = Results::new();
    item.insert(
        "trackType".into(),
        json!(if remote { "radio" } else { "local" }),
    );
    if let Some(a) = s("artwork_url") {
        item.insert("icon".into(), json!(proxied_image(&a)));
    } else if let Some(c) = s("coverid").or_else(|| s("artwork_track_id")) {
        item.insert("icon-id".into(), json!(c));
    } else if remote {
        item.insert("icon-id".into(), json!("/html/images/radio.png"));
    }
    let title = s("title").unwrap_or_default();
    let album = s("album").or_else(|| s("remote_title")).unwrap_or_default();
    let mut artists: Vec<String> = Vec::new();
    for k in ["albumartist", "trackartist", "artist"] {
        if let Some(v) = s(k) {
            for a in v.split(", ") {
                if !a.is_empty() && !artists.iter().any(|x| x == a) {
                    artists.push(a.to_string());
                }
            }
        }
    }
    let artist = artists.join(", ");
    item.insert("track".into(), json!(title));
    item.insert("album".into(), json!(album));
    item.insert("artist".into(), json!(artist));
    let second: Vec<&str> = [artist.as_str(), album.as_str()]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect();
    item.insert(
        "text".into(),
        json!(format!("{}\n{}", title, second.join(" - "))),
    );
    let id = row.get("id").cloned().unwrap_or(json!(0));
    item.insert(
        "params".into(),
        json!({"track_id": id, "playlist_index": index}),
    );
    item.insert("style".into(), json!("itemplay"));
    item
}

/// `_addJivePlaylistControls`: Clear / Save playlist items after the last song.
fn add_jive_playlist_controls(r: &mut Request) {
    use crate::strings::get as st;
    let clear = json!({
        "text": st("CLEAR_PLAYLIST").unwrap_or("Clear Playlist"),
        "icon-id": "/html/images/playlistclear.png",
        "offset": 0, "count": 2,
        "item_loop": [
            {"text": st("CANCEL").unwrap_or("Cancel"), "actions": {"go": {"player": 0, "cmd": ["jiveblankcommand"]}}, "nextWindow": "parent"},
            {"text": st("CLEAR_PLAYLIST").unwrap_or("Clear Playlist"), "actions": {"do": {"player": 0, "cmd": ["playlist", "clear"]}}, "nextWindow": "home"}
        ]
    });
    let save = json!({
        "text": st("SAVE_PLAYLIST").unwrap_or("Save Playlist"),
        "icon-id": "/html/images/playlistsave.png",
        "input": {"len": 1, "allowedChars": st("JIVE_ALLOWEDCHARS_WITHCAPS").unwrap_or(""), "help": {"text": st("JIVE_SAVEPLAYLIST_HELP").unwrap_or("")}},
        "actions": {"do": {"player": 0, "cmd": ["playlist", "save"], "params": {"playlistName": "__INPUT__"}, "itemsParams": "params"}}
    });
    for v in [clear, save] {
        if let Value::Object(m) = v {
            r.add_loop_item("item_loop", m);
        }
    }
}

// ---- alarms ------------------------------------------------------------------------------

pub fn alarms(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    crate::plugins::alarm::alarms_query(&p, r);
    r.set_done();
}

pub fn alarm_playlists(server: &Arc<Server>, r: &mut Request) {
    let mut item = Results::new();
    item.insert(
        "category".into(),
        json!(crate::strings::get("CURRENT_PLAYLIST").unwrap_or("Current Playlist")),
    );
    item.insert(
        "title".into(),
        json!(crate::strings::get("ALARM_USE_CURRENT_PLAYLIST").unwrap_or("Current playlist")),
    );
    item.insert("url".into(), Value::Null);
    item.insert("singleton".into(), json!("1"));
    r.add_loop_item("item_loop", item);
    for (title, url) in server.favorites.flat_audio() {
        let mut item = Results::new();
        item.insert("category".into(), json!("Favorites"));
        item.insert("title".into(), json!(title));
        item.insert("url".into(), json!(url));
        item.insert("singleton".into(), json!("0"));
        r.add_loop_item("item_loop", item);
    }
    r.add_result("offset", r.param_i64("_index").unwrap_or(0));
    let n = r.loop_len("item_loop");
    r.add_result("count", n);
    r.set_done();
}
