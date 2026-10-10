//! The control layer: request model, dispatch table, commands, queries and the transports
//! that feed them (CLI on port 9090; JSON-RPC and Cometd live in `web`).

pub mod cli;
pub mod commands;
pub mod dispatch;
pub mod ir;
pub mod jive;
pub mod libqueries;
pub mod queries;
pub mod request;
pub mod songdata;
pub mod subscribe;

use dispatch::{DispatchTable, Handler};

/// Wrap an async handler function into a dispatch `Handler`.
macro_rules! asyncf {
    ($f:path) => {
        Handler::Async(|s, r| Box::pin($f(s, r)))
    };
}

/// The complete dispatch table (`Slim::Control::Request::init` plus plugin/Jive entries).
pub fn build_dispatch_table() -> DispatchTable {
    use commands as c;
    use libqueries as l;
    use queries as q;
    let mut t = DispatchTable::default();
    let s = Handler::Sync;
    // terms, needsClient, isQuery, hasTags, handler
    t.add(&["abortscan"], 0, false, false, s(c::abortscan));
    t.add(&["alarm", "_cmd"], 1, false, true, s(c::alarm));
    t.add(
        &["alarm", "playlists", "_index", "_quantity"],
        0,
        true,
        true,
        s(q::alarm_playlists),
    );
    t.add(
        &["alarms", "_index", "_quantity"],
        1,
        true,
        true,
        s(q::alarms),
    );
    t.add(&["album", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["albums", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::albums),
    );
    t.add(&["artist", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["artists", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::artists),
    );
    t.add(
        &["artworkspec", "add", "_spec", "_name"],
        0,
        false,
        false,
        s(c::done),
    );
    t.add(
        &["button", "_buttoncode", "_time", "_orFunction"],
        1,
        false,
        false,
        s(c::button),
    );
    t.add(&["client", "forget"], 1, false, false, s(c::client_forget));
    t.add(&["connect", "_where"], 1, false, false, s(c::connect));
    t.add(&["connected", "?"], 1, true, false, s(q::connected));
    t.add(
        &["contextmenu", "_index", "_quantity"],
        1,
        true,
        true,
        asyncf!(jive::contextmenu),
    );
    t.add(&["current_title", "?"], 1, true, false, s(q::cursonginfo));
    t.add(&["debug", "_debugflag", "?"], 0, true, false, s(q::debug));
    t.add(
        &["debug", "_debugflag", "_newvalue"],
        0,
        false,
        false,
        s(c::done),
    );
    t.add(
        &["disconnect", "_playerid", "_from"],
        0,
        false,
        false,
        s(c::done),
    );
    t.add(&["display", "?", "?"], 1, true, false, s(q::display));
    t.add(
        &["display", "_line1", "_line2", "_duration"],
        1,
        false,
        false,
        s(c::display),
    );
    t.add(&["displaynow", "?", "?"], 1, true, false, s(q::display));
    t.add(&["displaystatus"], 1, true, true, s(q::displaystatus));
    t.add(&["duration", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["readdirectory", "_index", "_quantity"],
        0,
        true,
        true,
        s(q::readdirectory),
    );
    t.add(&["genre", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["genres", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::genres),
    );
    t.add(&["getstring", "_tokens"], 0, true, false, s(q::getstring));
    for e in ["albums", "artists", "genres", "songs", "duration"] {
        t.add(&["info", "total", e, "?"], 0, true, false, s(q::info_total));
    }
    t.add(&["ir", "_ircode", "_time"], 1, false, false, s(c::ir));
    t.add(&["irenable", "?"], 1, true, false, s(q::irenable));
    t.add(&["irenable", "_newvalue"], 1, false, false, s(c::done));
    t.add(&["libraries"], 0, true, false, s(q::libraries));
    t.add(&["libraries", "getid"], 1, true, false, s(q::libraries));
    t.add(
        &["linesperscreen", "?"],
        1,
        true,
        false,
        s(q::linesperscreen),
    );
    t.add(&["logging"], 0, false, true, s(c::done));
    for e in ["bass", "muting", "stereoxl", "pitch", "treble", "volume"] {
        t.add(&["mixer", e, "?"], 1, true, false, s(q::mixer));
        t.add(&["mixer", e, "_newvalue"], 1, false, true, s(c::mixer));
    }
    t.add(&["mode", "?"], 1, true, false, s(q::mode));
    t.add(
        &["musicfolder", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::musicfolder),
    );
    t.add(
        &["mediafolder", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::musicfolder),
    );
    t.add(&["name", "_newvalue"], 1, false, false, s(c::name));
    t.add(&["name", "?"], 1, true, false, s(q::name));
    t.add(&["path", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["pause", "_newvalue", "_fadein", "_suppressShowBriefly"],
        1,
        false,
        false,
        s(c::playcontrol),
    );
    t.add(&["play", "_fadein"], 1, false, false, s(c::playcontrol));
    for e in [
        "address",
        "displaytype",
        "id",
        "uuid",
        "ip",
        "model",
        "isplayer",
        "name",
        "canpoweroff",
    ] {
        t.add(
            &["player", e, "_IDorIndex", "?"],
            0,
            true,
            false,
            s(q::player_x),
        );
    }
    t.add(&["player", "count", "?"], 0, true, false, s(q::player_x));
    t.add(
        &["playerpref", "_prefname", "?"],
        1,
        true,
        false,
        s(q::pref),
    );
    t.add(
        &["playerpref", "validate", "_prefname", "_newvalue"],
        1,
        true,
        false,
        s(q::pref_validate),
    );
    t.add(
        &["playerpref", "_prefname", "_newvalue"],
        1,
        false,
        true,
        s(c::pref),
    );
    t.add(
        &["players", "_index", "_quantity"],
        0,
        true,
        true,
        s(q::players),
    );
    for e in ["add", "append", "insert", "play"] {
        let terms: &[&str] = match e {
            "play" => &["playlist", "play", "_item", "_title", "_fadein"],
            "add" => &["playlist", "add", "_item", "_title"],
            "append" => &["playlist", "append", "_item", "_title"],
            _ => &["playlist", "insert", "_item", "_title"],
        };
        t.add(terms, 1, false, false, asyncf!(c::playlist_xitem));
    }
    t.add(
        &["playlist", "insertlist", "_item"],
        1,
        false,
        false,
        asyncf!(c::playlist_xitem),
    );
    t.add(
        &["playlist", "load", "_item"],
        1,
        false,
        true,
        asyncf!(c::playlist_xitem),
    );
    t.add(
        &["playlist", "resume", "_item"],
        1,
        false,
        true,
        asyncf!(c::playlist_xitem),
    );
    for e in [
        "addalbum",
        "deletealbum",
        "insertalbum",
        "loadalbum",
        "playalbum",
    ] {
        t.add(
            &["playlist", e, "_genre", "_artist", "_album", "_title"],
            1,
            false,
            false,
            asyncf!(c::playlist_xalbum),
        );
    }
    for e in ["addtracks", "deletetracks", "inserttracks"] {
        t.add(
            &["playlist", e, "_what", "_listref"],
            1,
            false,
            false,
            asyncf!(c::playlist_xtracks),
        );
    }
    for e in ["loadtracks", "playtracks"] {
        t.add(
            &["playlist", e, "_what", "_listref", "_fadein", "_index"],
            1,
            false,
            false,
            asyncf!(c::playlist_xtracks),
        );
    }
    for e in [
        "album", "artist", "duration", "genre", "path", "remote", "title",
    ] {
        t.add(
            &["playlist", e, "_index", "?"],
            1,
            true,
            false,
            s(q::playlist_x),
        );
    }
    for e in [
        "index", "jump", "modified", "name", "repeat", "shuffle", "tracks", "url",
    ] {
        t.add(&["playlist", e, "?"], 1, true, false, s(q::playlist_x));
    }
    t.add(
        &["playlist", "clear"],
        1,
        false,
        false,
        s(c::playlist_clear),
    );
    t.add(
        &["playlist", "delete", "_index"],
        1,
        false,
        false,
        s(c::playlist_delete),
    );
    t.add(
        &["playlist", "deleteitem", "_item"],
        1,
        false,
        false,
        s(c::playlist_deleteitem),
    );
    t.add(
        &[
            "playlist",
            "index",
            "_index",
            "_fadein",
            "_noplay",
            "_seekdata",
        ],
        1,
        false,
        false,
        s(c::playlist_jump),
    );
    t.add(
        &[
            "playlist",
            "jump",
            "_index",
            "_fadein",
            "_noplay",
            "_seekdata",
        ],
        1,
        false,
        false,
        s(c::playlist_jump),
    );
    t.add(
        &["playlist", "move", "_fromindex", "_toindex"],
        1,
        false,
        false,
        s(c::playlist_move),
    );
    t.add(
        &["playlist", "playlistsinfo"],
        1,
        true,
        true,
        s(q::playlist_playlistsinfo),
    );
    t.add(
        &["playlist", "preview"],
        1,
        false,
        true,
        asyncf!(c::playlist_preview),
    );
    t.add(
        &["playlist", "repeat", "_newvalue"],
        1,
        false,
        false,
        s(c::playlist_repeat),
    );
    t.add(
        &["playlist", "save", "_title"],
        1,
        false,
        true,
        s(c::playlist_save),
    );
    t.add(
        &["playlist", "shuffle", "_newvalue"],
        1,
        false,
        false,
        s(c::playlist_shuffle),
    );
    t.add(
        &["playlist", "zap", "_index"],
        1,
        false,
        false,
        s(c::playlist_zap),
    );
    t.add(
        &["playlistcontrol"],
        1,
        false,
        true,
        asyncf!(c::playlistcontrol),
    );
    t.add(
        &["playlists", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::playlists),
    );
    t.add(&["playlists", "edit"], 0, false, true, s(c::playlists_edit));
    t.add(
        &["playlists", "delete"],
        0,
        false,
        true,
        s(c::playlists_delete),
    );
    t.add(&["playlists", "new"], 0, false, true, s(c::playlists_new));
    t.add(
        &["playlists", "rename"],
        0,
        false,
        true,
        s(c::playlists_rename),
    );
    t.add(
        &["playlists", "tracks", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::playlist_tracks),
    );
    t.add(
        &["playlisttracks", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::playlist_tracks),
    );
    t.add(&["power", "?"], 1, true, false, s(q::power));
    t.add(
        &["power", "_newvalue", "_noplay"],
        1,
        false,
        true,
        s(c::power),
    );
    t.add(&["pragma", "_pragma"], 0, false, false, s(c::done));
    t.add(&["pref", "_prefname", "?"], 0, true, false, s(q::pref));
    t.add(
        &["pref", "validate", "_prefname", "_newvalue"],
        0,
        true,
        false,
        s(q::pref_validate),
    );
    t.add(
        &["pref", "_prefname", "_newvalue"],
        0,
        false,
        true,
        s(c::pref),
    );
    t.add(&["remote", "?"], 1, true, false, s(q::cursonginfo));
    t.add(&["rescan", "?"], 0, true, false, s(q::rescan));
    t.add(
        &["rescan", "_mode", "_target"],
        0,
        false,
        false,
        s(c::rescan),
    );
    t.add(&["rescanprogress"], 0, true, true, s(q::rescanprogress));
    t.add(&["restartserver"], 0, false, false, s(c::stopserver));
    t.add(
        &["search", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::search),
    );
    t.add(
        &["serverstatus", "_index", "_quantity"],
        0,
        true,
        true,
        s(q::serverstatus),
    );
    t.add(&["show"], 1, false, true, s(c::show));
    t.add(
        &["signalstrength", "?"],
        1,
        true,
        false,
        s(q::signalstrength),
    );
    t.add(&["sleep", "?"], 1, true, false, s(q::sleep));
    t.add(&["sleep", "_newvalue"], 1, false, false, s(c::sleep));
    t.add(
        &["songinfo", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::songinfo),
    );
    t.add(
        &["songs", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::titles),
    );
    t.add(
        &["status", "_index", "_quantity"],
        1,
        true,
        true,
        s(q::status),
    );
    t.add(&["stop"], 1, false, false, s(c::playcontrol));
    t.add(&["stopserver"], 0, false, false, s(c::stopserver));
    t.add(&["sync", "?"], 1, true, false, s(q::sync));
    t.add(&["sync", "_indexid-"], 1, false, true, s(c::sync));
    t.add(&["syncgroups", "?"], 0, true, false, s(q::syncgroups));
    t.add(&["tags"], 0, true, true, s(c::done_query));
    t.add(&["time", "?"], 1, true, false, s(q::time));
    t.add(&["time", "_newvalue"], 1, false, false, s(c::time));
    t.add(&["title", "?"], 1, true, false, s(q::cursonginfo));
    t.add(
        &["titles", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::titles),
    );
    t.add(
        &["tracks", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::titles),
    );
    t.add(&["version", "?"], 0, true, false, s(q::version));
    t.add(&["wipecache", "_queue"], 0, false, false, s(c::wipecache));
    t.add(
        &["years", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::years),
    );
    t.add(
        &["works", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::works),
    );
    t.add(
        &["roles", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(l::roles),
    );
    t.add(&["artwork", "_artworkid"], 0, false, false, s(q::artwork));
    t.add(&["rating", "_item", "?"], 0, true, false, s(q::rating));
    t.add(
        &["rating", "_item", "_rating"],
        0,
        false,
        false,
        s(c::rating),
    );
    // pure notifications
    for terms in [
        &["client", "disconnect"][..],
        &["client", "new"],
        &["client", "reconnect"],
        &["client", "upgrade_firmware"],
        &["playlist", "load_done"],
        &["playlist", "newsong"],
        &["playlist", "open", "_path"],
        &["playlist", "sync"],
        &["playlist", "cant_open", "_url", "_error"],
        &["playlist", "pause", "_newvalue"],
        &["playlist", "stop"],
        &["unknownir", "_ircode", "_time"],
        &["displaynotify", "_type", "_parts", "_duration"],
        &["alarm", "sound", "_id"],
        &["alarm", "end", "_id"],
        &["alarm", "snooze", "_id"],
        &["alarm", "snooze_end", "_id"],
        &["newmetadata"],
    ] {
        t.add(terms, 1, false, false, Handler::None);
    }
    t.add(&["rescan", "done"], 0, false, false, Handler::None);
    t.add(
        &["library", "changed", "_newvalue"],
        0,
        false,
        false,
        Handler::None,
    );
    t.add(
        &["prefset", "_namespace", "_prefname", "_newvalue"],
        0,
        false,
        true,
        Handler::None,
    );
    t.add(
        &["fwdownloaded", "_machine"],
        0,
        false,
        false,
        Handler::None,
    );
    t.add(&["favorites", "changed"], 0, false, false, Handler::None);
    t.add(&["menustatus"], 0, false, false, Handler::None);
    t.add(&["mode", "pause"], 1, false, false, s(c::playcontrol));
    t.add(&["mode", "play"], 1, false, false, s(c::playcontrol));
    t.add(&["mode", "stop"], 1, false, false, s(c::playcontrol));
    t.add(&["gototime", "?"], 1, true, false, s(q::time));
    t.add(&["gototime", "_newvalue"], 1, false, false, s(c::time));
    // CLI plugin
    t.add(
        &["can", "_p1", "_p2", "_p3", "_p4", "_p5", "?"],
        0,
        true,
        false,
        s(q::can),
    );
    t.add(&["listen", "_newvalue"], 0, false, false, s(c::done));
    t.add(&["listen", "?"], 0, true, false, s(c::done_query));
    t.add(&["subscribe", "_functions"], 0, false, false, s(c::done));
    // Jive (Slim::Control::Jive) and plugins
    jive::register(&mut t);
    crate::plugins::register(&mut t);
    t
}
