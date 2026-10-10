//! Context ("info") menus: `trackinfo|albuminfo|artistinfo|genreinfo|yearinfo|playlistinfo|
//! folderinfo items` (`Slim::Menu::TrackInfo`, `AlbumInfo`, … with the Favorites plugin's
//! provider) and `systeminfo items` (`Slim::Menu::SystemInfo`).
//!
//! Each menu is built as a static OPML tree in the provider order `Slim::Menu::Base` produces,
//! then rendered by [`super::xmlbrowser`]; sub-menus ("More Info") are reached by `item_id`.

use super::xmlbrowser::{
    self, descend, obj, split_item_id, value_i64, value_string, ActionDef, Actions, Ctx, Feed,
    Item, Level, Obj,
};
use crate::control::request::Request;
use crate::player::playlist::Entry;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

pub const INFO_QUERIES: [&str; 7] = [
    "trackinfo",
    "albuminfo",
    "artistinfo",
    "genreinfo",
    "yearinfo",
    "playlistinfo",
    "folderinfo",
];

/// Track tag letters needed for the track menu.
const TRACK_TAGS: &str = "aAsSelgGpPytkwiqodYXRrTIfOunDvjcxNKJ";

/// `<name>info items|playlist`.
pub async fn info_query(server: Arc<Server>, mut r: Request) -> Request {
    super::fix_index_quantity(&mut r);
    let query = r.term(0).to_string();
    let mut ctx = Ctx::from_request(&server, &r, &query);
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let feed = match query.as_str() {
        "trackinfo" => track_menu(&server, &r, ctx.menu_mode).await,
        "albuminfo" => album_menu(&server, &r, ctx.menu_mode).await,
        "artistinfo" => artist_menu(&server, &r, ctx.menu_mode).await,
        "genreinfo" => genre_menu(&server, &r, ctx.menu_mode).await,
        "yearinfo" => year_menu(&r, ctx.menu_mode),
        "playlistinfo" => playlist_menu(&server, &r, ctx.menu_mode).await,
        "folderinfo" => folder_menu(&r, ctx.menu_mode),
        _ => None,
    };
    let Some(feed) = feed else {
        r.bad_params();
        return r;
    };
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    let top_play = feed.parent.as_ref().and_then(|p| p.play.clone());
    let level = descend(feed, &path, play.is_none()).unwrap_or(Level::List(Feed::default()));
    match play {
        Some((method, pi)) => {
            // `cliPlaylistCmd`: playing the menu itself plays its object (the track)
            if path.is_empty() {
                if let Some(url) = top_play {
                    let item = Item {
                        kind: Some("audio".into()),
                        url: Some(url),
                        ..Default::default()
                    };
                    xmlbrowser::play(
                        &server,
                        &mut r,
                        Level::Leaf(Box::new(item), Feed::default()),
                        &method,
                        pi,
                    )
                    .await;
                    return r;
                }
            }
            xmlbrowser::play(&server, &mut r, level, &method, pi).await
        }
        None => xmlbrowser::render(&ctx, level, &mut r),
    }
    r
}

// ---- item builders --------------------------------------------------------------------------

/// A menu-mode text item with explicit Jive actions (`addtrack`, `playitem`, …).
fn jive_item(name: String, playcontrol: Option<&str>, style: Option<&str>, actions: Value) -> Item {
    let mut jive = obj(json!({ "actions": actions }));
    if let Some(st) = style {
        jive.insert("style".into(), json!(st));
    }
    Item {
        name: Some(name),
        kind: Some("text".into()),
        playcontrol: playcontrol.map(str::to_string),
        jive: Some(jive),
        ..Default::default()
    }
}

/// The Add to End / Play Next / Play trio acting via `playlistcontrol <params>`
/// (ArtistInfo, GenreInfo, YearInfo, PlaylistInfo, FolderInfo).
fn playlistcontrol_trio(params: &Obj, play_style: Option<&str>) -> Vec<Item> {
    let act = |cmd: &str, nw: &str| {
        let mut p = params.clone();
        p.insert("cmd".into(), json!(cmd));
        json!({ "player": 0, "cmd": ["playlistcontrol"], "params": p, "nextWindow": nw })
    };
    let add = act("add", "parent");
    let insert = act("insert", "parent");
    let play = act("load", "nowPlaying");
    vec![
        jive_item(
            s("ADD_TO_END"),
            Some("add"),
            None,
            json!({ "go": add, "play": add, "add": add }),
        ),
        jive_item(
            s("PLAY_NEXT"),
            Some("insert"),
            None,
            json!({ "go": insert, "play": insert, "add": insert }),
        ),
        jive_item(
            s("PLAY"),
            Some("play"),
            play_style,
            json!({ "go": play, "play": play }),
        ),
    ]
}

/// A browsable `type => 'playlist'` item with `itemActions` (contributors, album, genre, year).
fn browse_item(
    name: String,
    label: &str,
    items_params: Obj,
    pc_params: Obj,
    info: (&str, Obj),
) -> Item {
    let ia = Actions {
        all_defined: true,
        items: Some(ActionDef::cmd(&["browselibrary", "items"], items_params)),
        info: Some(ActionDef::cmd(&[info.0, "items"], info.1)),
        ..Default::default()
    }
    .with_playlistcontrol(&pc_params);
    Item {
        name: Some(name),
        label: Some(label.into()),
        kind: Some("playlist".into()),
        url: Some("blabla".into()),
        item_actions: Some(ia),
        ..Default::default()
    }
}

fn label_item(label: &str, value: impl Into<String>) -> Item {
    Item {
        name: Some(value.into()),
        label: Some(label.into()),
        kind: Some("text".into()),
        ..Default::default()
    }
}

/// Favorites index of a URL (`Slim::Utils::Favorites::findUrl`).
pub type FavIndex<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Favorites plugin provider (`_objectInfoHandler`, menu mode).
fn favorites_item(fav: FavIndex, title: &str, url: &str) -> Item {
    let (cmd, token, index) = match fav(url) {
        Some(i) => ("delete", "JIVE_DELETE_FROM_FAVORITES", Some(i)),
        None => ("add", "JIVE_SAVE_TO_FAVORITES", None),
    };
    let mut params = obj(json!({ "title": title, "url": url, "isContextMenu": 1 }));
    if let Some(i) = index {
        params.insert("item_id".into(), json!(i));
    }
    jive_item(
        s(token),
        None,
        Some("item_fav"),
        json!({ "go": { "player": 0, "cmd": ["jivefavorites", cmd], "params": params } }),
    )
}

/// Filter params passed on by the info menus (`library_id`, …).
fn filter_params(r: &Request, keys: &[&str]) -> Obj {
    let mut p = Obj::new();
    for k in keys {
        if let Some(v) = r.param_nonempty(k) {
            p.insert(k.to_string(), json!(v));
        }
    }
    p
}

fn merged(base: &Obj, extra: Value) -> Obj {
    let mut p = base.clone();
    if let Value::Object(e) = extra {
        p.extend(e);
    }
    p
}

fn get(o: &Obj, k: &str) -> Option<String> {
    o.get(k).and_then(value_string).filter(|v| !v.is_empty())
}

/// `delimitThousands`.
fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

fn mss(secs: f64) -> String {
    let s = secs as i64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Contributor roles in `contributorRoles` order.
const ROLES: [(&str, &str); 6] = [
    ("artist", "ARTIST"),
    ("composer", "COMPOSER"),
    ("conductor", "CONDUCTOR"),
    ("band", "BAND"),
    ("albumartist", "ALBUMARTIST"),
    ("trackartist", "TRACKARTIST"),
];

/// `(role, name, id)` triples from a track row with `A`/`S` tags.
fn contributors(row: &Obj) -> Vec<(&'static str, String, String)> {
    let mut out = Vec::new();
    for (field, role) in ROLES {
        let (Some(names), Some(ids)) = (get(row, field), get(row, &format!("{}_ids", field)))
        else {
            continue;
        };
        let ids: Vec<String> = ids
            .split(',')
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty())
            .collect();
        let names: Vec<String> = if ids.len() == 1 {
            vec![names]
        } else {
            names.split(", ").map(str::to_string).collect()
        };
        if names.len() != ids.len() {
            continue;
        }
        for (n, i) in names.into_iter().zip(ids) {
            out.push((role, n, i));
        }
    }
    out
}

/// `infoContributors` item.
fn contributor_item(role: &str, name: String, id: &str, library_id: Option<&str>) -> Item {
    let lib = |mut p: Obj| {
        if let Some(l) = library_id {
            p.insert("library_id".into(), json!(l));
        }
        p
    };
    let mut items = lib(obj(json!({ "mode": "albums", "artist_id": id })));
    if !matches!(role, "ARTIST" | "ALBUMARTIST" | "TRACKARTIST") {
        items.insert("role_id".into(), json!(role));
    }
    let ids = lib(obj(json!({ "artist_id": id })));
    let mut it = browse_item(name, role, items, ids.clone(), ("artistinfo", ids));
    it.url = Some("not a valid URL, but needed to make the ip3k UI work...".into());
    it
}

// ---- trackinfo ------------------------------------------------------------------------------

/// The track a `trackinfo` request refers to: `(entry, playlist index, is current song)`.
fn track_entry(server: &Arc<Server>, r: &Request) -> Option<(Entry, Option<usize>)> {
    let p = super::player(server, r);
    let pindex = r
        .param_i64("playlist_index")
        .filter(|i| *i >= 0)
        .map(|i| i as usize);
    let track_id = r.param_i64("track_id");
    if let (Some(p), Some(i)) = (&p, pindex) {
        let e = p.controller().lock().playlist.song(i).cloned();
        if let Some(e) = e {
            if track_id.is_none()
                || track_id.map(|t| t < 0).unwrap_or(false)
                || e.track_id == track_id
            {
                return Some((e, Some(i)));
            }
        }
    }
    if let Some(t) = track_id
        .filter(|t| *t > 0)
        .and_then(|t| server.library.track(t))
    {
        return Some((server.entry_for_track(&t), pindex));
    }
    if let Some(u) = r.param_nonempty("url") {
        return Some((server.entry_for_url(u), pindex));
    }
    None
}

async fn track_menu(server: &Arc<Server>, r: &Request, menu_mode: bool) -> Option<Feed> {
    let (entry, pindex) = track_entry(server, r)?;
    let p = super::player(server, r);
    let (row, playlist_len, playing_index, is_playing, live_title) = {
        match &p {
            Some(p) => {
                let c = p.controller();
                let c = c.lock();
                let cur = c.playing_song_index();
                let song = if pindex == Some(cur) {
                    c.playing_song().cloned()
                } else {
                    None
                };
                let row =
                    crate::control::songdata::song_row(server, &entry, song.as_ref(), TRACK_TAGS);
                let live = song.as_ref().and_then(|s| s.remote_meta.title.clone());
                (row, c.playlist.len(), cur, c.is_playing(false), live)
            }
            None => (
                crate::control::songdata::song_row(server, &entry, None, TRACK_TAGS),
                0,
                0,
                false,
                None,
            ),
        }
    };
    let remote = crate::util::is_remote_url(&entry.url);
    let playlist_context = r.param("context") == Some("playlist") && pindex.is_some();
    let ctxinfo = TrackCtx {
        playlist_context,
        pindex: pindex.unwrap_or(0),
        playlist_len,
        playing_index,
        is_playing,
        live_title,
        has_player: p.is_some(),
    };
    let library_id = r.param_nonempty("library_id").map(str::to_string);
    let fav = |u: &str| server.favorites.find_url(u);
    let items = track_items(
        &fav,
        &row,
        &entry.url,
        remote,
        &ctxinfo,
        library_id.as_deref(),
        menu_mode,
    );
    let title = get(&row, "title").unwrap_or_else(|| entry.url.clone());
    let parent = Item {
        play: Some(entry.url.clone()),
        ..Default::default()
    };
    Some(Feed {
        title: Some(title),
        items,
        menu_complete: true,
        parent: Some(Box::new(parent)),
        ..Default::default()
    })
}

/// Player/playlist state relevant to the track menu.
#[derive(Default)]
pub struct TrackCtx {
    pub playlist_context: bool,
    pub pindex: usize,
    pub playlist_len: usize,
    pub playing_index: usize,
    pub is_playing: bool,
    pub live_title: Option<String>,
    pub has_player: bool,
}

/// The TrackInfo provider list for one track row.
pub fn track_items(
    fav: FavIndex,
    row: &Obj,
    url: &str,
    remote: bool,
    tc: &TrackCtx,
    library_id: Option<&str>,
    menu_mode: bool,
) -> Vec<Item> {
    let mut items = Vec::new();
    let track_id = row.get("id").cloned().unwrap_or(json!(0));
    if menu_mode && tc.has_player {
        // addtrack
        if tc.playlist_context {
            let go = json!({ "player": 0, "cmd": ["playlist", "delete", tc.pindex], "nextWindow": "parent" });
            items.push(jive_item(
                s("REMOVE_FROM_PLAYLIST"),
                None,
                None,
                json!({ "go": go, "play": go, "add": go, "add-hold": go }),
            ));
        } else {
            let add = json!({ "player": 0, "cmd": ["playlistcontrol"], "params": { "cmd": "add", "track_id": track_id }, "nextWindow": "parent" });
            items.push(jive_item(
                s("ADD_TO_END"),
                Some("add"),
                None,
                json!({ "add": add, "play": add, "go": add }),
            ));
        }
        // addtracknext
        if tc.playlist_context {
            let move_to = tc.playing_index;
            if tc.playlist_len >= 2 && tc.pindex != move_to && tc.pindex != move_to + 1 {
                let to = if tc.pindex > move_to {
                    move_to + 1
                } else {
                    move_to
                };
                let go = json!({ "player": 0, "cmd": ["playlist", "move", tc.pindex, to], "nextWindow": "parent" });
                items.push(jive_item(
                    s("PLAY_NEXT"),
                    None,
                    None,
                    json!({ "go": go, "play": go, "add": go, "add-hold": go }),
                ));
            }
        } else {
            let add = json!({ "player": 0, "cmd": ["playlistcontrol"], "params": { "cmd": "insert", "track_id": track_id }, "nextWindow": "parent" });
            items.push(jive_item(
                s("PLAY_NEXT"),
                Some("insert"),
                None,
                json!({ "add": add, "play": add, "go": add }),
            ));
        }
        // playitem
        if tc.playlist_context {
            if !(tc.pindex == tc.playing_index && tc.is_playing) {
                let go = json!({ "player": 0, "cmd": ["playlist", "jump", tc.pindex], "nextWindow": "parent" });
                items.push(jive_item(
                    s("PLAY"),
                    Some("play"),
                    Some("itemplay"),
                    json!({ "go": go, "play": go, "add": go, "add-hold": go }),
                ));
            }
        } else {
            let go = json!({ "player": 0, "cmd": ["playlistcontrol"], "params": { "cmd": "load", "track_id": track_id }, "nextWindow": "nowPlaying" });
            items.push(jive_item(
                s("PLAY"),
                Some("play"),
                Some("itemplay"),
                json!({ "go": go, "play": go }),
            ));
        }
    }
    let title = get(row, "title").unwrap_or_default();
    if menu_mode {
        items.push(favorites_item(fav, &title, url));
    }
    // contributors
    if !remote {
        for (role, name, id) in contributors(row) {
            items.push(contributor_item(role, name, &id, library_id));
        }
    } else if let Some(a) = get(row, "artist") {
        items.push(label_item("ARTIST", a));
    }
    // album
    let lib = |mut p: Obj| {
        if let Some(l) = library_id {
            p.insert("library_id".into(), json!(l));
        }
        p
    };
    match (get(row, "album"), get(row, "album_id")) {
        (Some(album), Some(id)) if !remote => {
            let ids = lib(obj(json!({ "album_id": id })));
            items.push(browse_item(
                album,
                "ALBUM",
                merged(&ids, json!({ "mode": "tracks" })),
                ids.clone(),
                ("albuminfo", ids),
            ));
        }
        (Some(album), _) => items.push(label_item("ALBUM", album)),
        _ => {}
    }
    // genres
    if !remote {
        let names = get(row, "genres").or_else(|| get(row, "genre"));
        let ids = get(row, "genre_ids").or_else(|| get(row, "genre_id"));
        if let (Some(names), Some(ids)) = (names, ids) {
            let ids: Vec<&str> = ids.split(',').map(str::trim).collect();
            let names: Vec<String> = if ids.len() == 1 {
                vec![names]
            } else {
                names.split(", ").map(str::to_string).collect()
            };
            for (n, id) in names.into_iter().zip(ids) {
                let p = lib(obj(json!({ "genre_id": id })));
                items.push(browse_item(
                    n,
                    "GENRE",
                    merged(&p, json!({ "mode": "artists" })),
                    p.clone(),
                    ("genreinfo", p),
                ));
            }
        }
    }
    // year
    if let Some(year) = get(row, "year").filter(|y| y != "0") {
        if remote {
            items.push(label_item("YEAR", year));
        } else {
            let p = lib(obj(json!({ "year": year })));
            let mut it = browse_item(
                year,
                "YEAR",
                merged(&p, json!({ "mode": "albums" })),
                p.clone(),
                ("yearinfo", p),
            );
            it.url = None;
            items.push(it);
        }
    }
    // artwork
    if menu_mode && !remote && get(row, "coverart").map(|c| c == "1").unwrap_or(false) {
        let mut it = jive_item(
            s("SHOW_ARTWORK_SINGLE"),
            None,
            None,
            json!({ "do": { "cmd": ["artwork", track_id] } }),
        );
        if let Some(j) = it.jive.as_mut() {
            j.insert("showBigArtwork".into(), json!(1));
        }
        items.push(it);
    }
    // comment, lyrics
    for (field, token) in [("comment", "COMMENT"), ("lyrics", "LYRICS")] {
        if let Some(text) = get(row, field).filter(|t| !t.trim().is_empty()) {
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            let child = Item {
                name: Some(text),
                label: Some(token.into()),
                kind: Some("text".into()),
                wrap: true,
                parse_urls: true,
                ..Default::default()
            };
            items.push(Item {
                name: Some(s(token)),
                items: Some(vec![child]),
                ..Default::default()
            });
        }
    }
    // moreinfo
    let more = more_info_items(row, url, remote);
    items.push(Item {
        name: Some(s("MOREINFO")),
        is_context_menu: true,
        items: Some(more),
        ..Default::default()
    });
    // remotetitle
    if remote {
        if let Some(t) = &tc.live_title {
            items.push(label_item("TITLE", t.clone()));
        }
    }
    items
}

/// The "More Info" children (`parent => 'moreinfo'`), in provider order.
fn more_info_items(row: &Obj, url: &str, remote: bool) -> Vec<Item> {
    let mut v = Vec::new();
    if let (Some(d), Some(dc)) = (get(row, "disc"), get(row, "disccount")) {
        v.push(label_item("DISC", format!("{}/{}", d, dc)));
    }
    if let Some(t) = get(row, "tracknum").filter(|t| t != "0") {
        v.push(label_item("TRACK_NUMBER", t));
    }
    if let Some(ct) = get(row, "type") {
        let up = ct.to_uppercase();
        let name = crate::strings::get(&up).map(str::to_string).unwrap_or(ct);
        v.push(label_item("TYPE", name));
    }
    if let Some(d) = row
        .get("duration")
        .and_then(|d| {
            d.as_f64()
                .or_else(|| value_string(d).and_then(|s| s.parse().ok()))
        })
        .filter(|d| *d > 0.0)
    {
        v.push(label_item("LENGTH", mss(d)));
    }
    for (field, label) in [
        ("replay_gain", "REPLAYGAIN"),
        ("album_replay_gain", "ALBUMREPLAYGAIN"),
    ] {
        if let Some(g) = row
            .get(field)
            .and_then(|g| {
                g.as_f64()
                    .or_else(|| value_string(g).and_then(|s| s.parse().ok()))
            })
            .filter(|g| *g != 0.0)
        {
            v.push(label_item(label, format!("{:.2} dB", g)));
        }
    }
    if let Some(rt) = get(row, "rating").filter(|r| r != "0") {
        v.push(label_item("RATING", rt));
    }
    if let Some(b) = get(row, "bitrate").filter(|b| b != "-1") {
        v.push(label_item("BITRATE", b));
    }
    if let Some(sr) = row.get("samplerate").and_then(value_i64).filter(|s| *s > 0) {
        v.push(label_item(
            "SAMPLERATE",
            format!("{:.1} kHz", sr as f64 / 1000.0),
        ));
    }
    if let Some(ss) = get(row, "samplesize") {
        v.push(label_item("SAMPLESIZE", format!("{}{}", ss, s("BITS"))));
    }
    if let Some(fs) = row.get("filesize").and_then(value_i64).filter(|f| *f > 0) {
        v.push(label_item("FILELENGTH", thousands(fs)));
    }
    if let Some(pc) = row.get("playcount").and_then(value_i64).filter(|p| *p > 0) {
        v.push(label_item("PLAYCOUNT", thousands(pc)));
    }
    if remote {
        let unescaped = percent_encoding::percent_decode_str(url)
            .decode_utf8_lossy()
            .to_string();
        v.push(label_item("URL", unescaped));
    } else if let Some(path) = crate::util::path_from_file_url(url) {
        let mut it = label_item("LOCATION", path.to_string_lossy().to_string());
        let id = get(row, "id").unwrap_or_default();
        let base = path
            .file_name()
            .map(|f| format!("/{}", f.to_string_lossy()))
            .unwrap_or_default();
        it.weblink = Some(format!("/music/{}/download{}", id, base));
        v.push(it);
    }
    for (field, label) in [
        ("modificationTime", "MODTIME"),
        ("addedTime", "ADDEDTIME"),
        ("tagversion", "TAGVERSION"),
    ] {
        if let Some(x) = get(row, field) {
            let x = if label == "TAGVERSION" {
                x
            } else {
                pretty_date(&x)
            };
            v.push(label_item(label, x));
        }
    }
    v
}

/// Raw epoch values (the library's SQL path) as LMS `longdateFormat, timeFormat`
/// (`Monday, January 5, 2026, 3:04 PM`); anything else is returned unchanged.
fn pretty_date(v: &str) -> String {
    use chrono::TimeZone;
    match v
        .parse::<i64>()
        .ok()
        .and_then(|e| chrono::Local.timestamp_opt(e, 0).single())
    {
        Some(t) => t.format("%A, %B %-d, %Y, %-I:%M %p").to_string(),
        None => v.to_string(),
    }
}

// ---- albuminfo ------------------------------------------------------------------------------

async fn album_menu(server: &Arc<Server>, r: &Request, menu_mode: bool) -> Option<Feed> {
    let album_id = r.param_nonempty("album_id")?.to_string();
    let filter = filter_params(
        r,
        &[
            "artist_id",
            "genre_id",
            "year",
            "library_id",
            "work_id",
            "performance",
        ],
    );
    let albums = super::exec(
        server,
        r,
        vec![
            "albums".into(),
            "0".into(),
            "1".into(),
            format!("album_id:{}", album_id),
            "tags:lyjaSiqWwXE".into(),
        ],
    )
    .await;
    let album = albums
        .results
        .get("albums_loop")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tracks = super::exec(
        server,
        r,
        vec![
            "titles".into(),
            "0".into(),
            "1000".into(),
            format!("album_id:{}", album_id),
            "tags:ASd".into(),
        ],
    )
    .await;
    let rows: Vec<Obj> = tracks
        .results
        .get("titles_loop")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_object).cloned().collect())
        .unwrap_or_default();
    let fav = |u: &str| server.favorites.find_url(u);
    let items = album_items(
        &fav,
        &album_id,
        &album,
        &rows,
        &filter,
        r.param_nonempty("library_id"),
        menu_mode,
        super::player(server, r).is_some(),
    );
    Some(Feed {
        title: get(&album, "album").or(Some(album_id)),
        items,
        ..Default::default()
    })
}

/// The AlbumInfo provider list.  `tracks` are the album's `titles` rows (tags `ASd`).
#[allow(clippy::too_many_arguments)]
pub fn album_items(
    fav: FavIndex,
    album_id: &str,
    album: &Obj,
    tracks: &[Obj],
    filter: &Obj,
    library_id: Option<&str>,
    menu_mode: bool,
    has_player: bool,
) -> Vec<Item> {
    let mut items = Vec::new();
    let mut pc_params = obj(json!({ "album_id": album_id }));
    pc_params.extend(filter.clone());
    if has_player {
        let mk = |cmd: &str, name: &str, pcs: &str, nw: &str| {
            let ia = Actions {
                items: Some(ActionDef::cmd(
                    &["playlistcontrol"],
                    merged(&pc_params, json!({ "cmd": cmd })),
                )),
                play: Some(ActionDef::cmd(
                    &["playlistcontrol"],
                    merged(&pc_params, json!({ "cmd": cmd })),
                )),
                add: if cmd == "load" {
                    None
                } else {
                    Some(ActionDef::cmd(
                        &["playlistcontrol"],
                        merged(&pc_params, json!({ "cmd": cmd })),
                    ))
                },
                ..Default::default()
            };
            Item {
                name: Some(s(name)),
                kind: Some(if menu_mode { "text" } else { "link" }.into()),
                playcontrol: Some(pcs.into()),
                next_window: Some(nw.into()),
                item_actions: Some(ia),
                ..Default::default()
            }
        };
        items.push(mk("add", "ADD_TO_END", "add", "parent"));
        items.push(mk("insert", "PLAY_NEXT", "insert", "parent"));
        items.push(mk("load", "PLAY", "play", "nowPlaying"));
    }
    let title = get(album, "album").unwrap_or_default();
    if menu_mode {
        if let Some(url) = get(album, "favorites_url") {
            items.push(favorites_item(fav, &title, &url));
        }
    }
    // contributors (unique per role, over the album's tracks)
    let mut seen: Vec<(String, String)> = Vec::new();
    for row in tracks {
        for (role, name, id) in contributors(row) {
            if !seen.iter().any(|(r, i)| r == role && *i == id) {
                seen.push((role.to_string(), id.clone()));
                items.push(contributor_item(role, name, &id, library_id));
            }
        }
    }
    let lib = |mut p: Obj| {
        if let Some(l) = library_id {
            p.insert("library_id".into(), json!(l));
        }
        p
    };
    if let Some(year) = get(album, "year").filter(|y| y != "0") {
        let p = lib(obj(json!({ "year": year })));
        let mut it = browse_item(
            year,
            "YEAR",
            merged(&p, json!({ "mode": "albums" })),
            p.clone(),
            ("yearinfo", p),
        );
        it.url = None;
        items.push(it);
    }
    if let (Some(d), Some(dc)) = (get(album, "disc"), get(album, "disccount")) {
        items.push(label_item("DISC", format!("{}/{}", d, dc)));
    }
    let total: f64 = tracks
        .iter()
        .filter_map(|t| {
            t.get("duration").and_then(|d| {
                d.as_f64()
                    .or_else(|| value_string(d).and_then(|s| s.parse().ok()))
            })
        })
        .sum();
    if total > 0.0 {
        items.push(label_item("ALBUMLENGTH", mss(total)));
    }
    if let Some(rt) = get(album, "release_type") {
        let name = crate::strings::get(&format!("RELEASE_TYPE_{}", rt.to_uppercase()))
            .map(str::to_string)
            .unwrap_or_else(|| ucfirst_lower(&rt));
        items.push(label_item("RELEASE_TYPE", name));
    }
    if get(album, "compilation").map(|c| c == "1").unwrap_or(false) {
        items.push(label_item("COMPILATION", s("YES")));
    }
    if let Some(e) = get(album, "extid") {
        items.push(label_item("EXTERNAL_ID", e));
    }
    if let Some(g) = album
        .get("album_replay_gain")
        .and_then(|g| {
            g.as_f64()
                .or_else(|| value_string(g).and_then(|s| s.parse().ok()))
        })
        .filter(|g| *g != 0.0)
    {
        items.push(label_item("ALBUMREPLAYGAIN", format!("{:.2} dB", g)));
    }
    items
}

fn ucfirst_lower(t: &str) -> String {
    let l = t.to_lowercase();
    let mut c = l.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---- artist / genre / year / playlist / folder ------------------------------------------------

async fn first_of(server: &Arc<Server>, r: &Request, tokens: Vec<String>, loop_name: &str) -> Obj {
    let out = super::exec(server, r, tokens).await;
    out.results
        .get(loop_name)
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

async fn artist_menu(server: &Arc<Server>, r: &Request, menu_mode: bool) -> Option<Feed> {
    let id = r.param_nonempty("artist_id")?.to_string();
    let row = first_of(
        server,
        r,
        vec![
            "artists".into(),
            "0".into(),
            "1".into(),
            format!("artist_id:{}", id),
        ],
        "artists_loop",
    )
    .await;
    let name = get(&row, "artist").unwrap_or_default();
    let mut items = Vec::new();
    if menu_mode {
        items.extend(playlistcontrol_trio(
            &obj(json!({ "artist_id": id })),
            Some("itemplay"),
        ));
        let url = get(&row, "favorites_url")
            .unwrap_or_else(|| format!("db:contributor.name={}", xmlbrowser::escape_search(&name)));
        items.push(favorites_item(
            &|u: &str| server.favorites.find_url(u),
            &name,
            &url,
        ));
    }
    Some(Feed {
        title: Some(name),
        items,
        menu_complete: true,
        ..Default::default()
    })
}

async fn genre_menu(server: &Arc<Server>, r: &Request, menu_mode: bool) -> Option<Feed> {
    let id = r.param_nonempty("genre_id")?.to_string();
    let row = first_of(
        server,
        r,
        vec![
            "genres".into(),
            "0".into(),
            "1".into(),
            format!("genre_id:{}", id),
        ],
        "genres_loop",
    )
    .await;
    let items = if menu_mode {
        playlistcontrol_trio(&obj(json!({ "genre_id": id })), Some("itemplay"))
    } else {
        vec![]
    };
    Some(Feed {
        title: Some(get(&row, "genre").unwrap_or_default()),
        items,
        menu_complete: true,
        ..Default::default()
    })
}

fn year_menu(r: &Request, menu_mode: bool) -> Option<Feed> {
    let year = r.param_nonempty("year")?.to_string();
    let items = if menu_mode {
        playlistcontrol_trio(&obj(json!({ "year": year })), Some("itemplay"))
    } else {
        vec![]
    };
    Some(Feed {
        title: Some(year),
        items,
        menu_complete: true,
        ..Default::default()
    })
}

async fn playlist_menu(server: &Arc<Server>, r: &Request, menu_mode: bool) -> Option<Feed> {
    let id = r.param_nonempty("playlist_id")?.to_string();
    let (name, url, urls) = id
        .parse::<i64>()
        .ok()
        .and_then(|i| server.library.playlist_by_id(i))
        .unwrap_or_default();
    let tracks = super::exec(
        server,
        r,
        vec![
            "playlists".into(),
            "tracks".into(),
            "0".into(),
            "1".into(),
            format!("playlist_id:{}", id),
        ],
    )
    .await;
    let count = tracks
        .results
        .get("count")
        .and_then(value_i64)
        .unwrap_or(urls.len() as i64);
    let mut items = vec![Item::text(format!(
        "{}{} {}",
        s("INFORMATION_TRACKS"),
        s("COLON"),
        thousands(count)
    ))];
    if menu_mode {
        let mut p = obj(json!({ "playlist_id": id }));
        if let Some(l) = r.param_nonempty("library_id") {
            p.insert("library_id".into(), json!(l));
        }
        items.extend(playlistcontrol_trio(&p, Some("itemplay")));
        if !url.is_empty() {
            items.push(favorites_item(
                &|u: &str| server.favorites.find_url(u),
                &name,
                &url,
            ));
        }
        let text = s("JIVE_DELETE_PLAYLIST")
            .replace("%s", &name)
            .replace('\n', " ");
        items.push(jive_item(text, None, Some("item"), json!({ "go": { "player": 0, "cmd": ["jiveplaylists", "delete"], "params": { "url": url, "playlist_id": id, "title": name, "menu": "track", "menu_all": 1 } } })));
    }
    Some(Feed {
        title: Some(name),
        items,
        menu_complete: true,
        ..Default::default()
    })
}

fn folder_menu(r: &Request, menu_mode: bool) -> Option<Feed> {
    let id = r.param_nonempty("folder_id")?.to_string();
    let mut items = if menu_mode {
        playlistcontrol_trio(&obj(json!({ "folder_id": id })), None)
    } else {
        vec![]
    };
    // FolderInfo's Play item has `play/add = go` and no itemplay style
    if let Some(play) = items.last_mut().and_then(|i| i.jive.as_mut()) {
        if let Some(Value::Object(a)) = play.get_mut("actions") {
            if let Some(go) = a.get("go").cloned() {
                a.insert("add".into(), go);
            }
        }
    }
    Some(Feed {
        title: Some(s("FOLDER_INFO")),
        items,
        menu_complete: true,
        ..Default::default()
    })
}

// ---- systeminfo -----------------------------------------------------------------------------

/// `systeminfo items` (`Slim::Menu::SystemInfo`): server, library and player sections.
pub async fn systeminfo(server: Arc<Server>, mut r: Request) -> Request {
    super::fix_index_quantity(&mut r);
    let mut ctx = Ctx::from_request(&server, &r, "systeminfo");
    let line =
        |label: &str, value: String| Item::text(format!("{}{} {}", s(label), s("COLON"), value));
    let mut sections = Vec::new();
    sections.push(Item {
        name: Some(s("INFORMATION_MENU_SERVER")),
        items: Some(vec![
            line(
                "INFORMATION_VERSION",
                format!("{} - RMS {}", crate::VERSION, env!("CARGO_PKG_VERSION")),
            ),
            line("INFORMATION_HOSTNAME", server.library_name()),
            line(
                "INFORMATION_SERVER_HTTP_ABBR",
                server.http_port().to_string(),
            ),
            line(
                "INFORMATION_OPERATINGSYSTEM_ABBR",
                std::env::consts::OS.to_string(),
            ),
            line(
                "INFORMATION_ARCHITECTURE_ABBR",
                std::env::consts::ARCH.to_string(),
            ),
            line("INFORMATION_CLIENTS", server.players().len().to_string()),
        ]),
        ..Default::default()
    });
    let t = server.library.totals();
    sections.push(Item {
        name: Some(s("INFORMATION_MENU_LIBRARY")),
        items: Some(vec![
            line("INFORMATION_TRACKS", thousands(t.songs as i64)),
            line("INFORMATION_ALBUMS", thousands(t.albums as i64)),
            line("INFORMATION_ARTISTS", thousands(t.artists as i64)),
            line("INFORMATION_GENRES", thousands(t.genres as i64)),
            line("INFORMATION_TIME", mss(t.duration)),
        ]),
        ..Default::default()
    });
    if let Some(p) = super::player(&server, &r) {
        let (model, fw, ip, signal) = {
            let st = p.state.lock();
            (
                st.caps.model_name.clone(),
                st.caps.firmware.clone(),
                st.addr.map(|a| a.ip().to_string()).unwrap_or_default(),
                st.signal_strength,
            )
        };
        let mut lines = vec![
            line("INFORMATION_PLAYER_MODEL_ABBR", model),
            line("INFORMATION_FIRMWARE_ABBR", fw),
            line("INFORMATION_PLAYER_IP_ABBR", ip),
            line("INFORMATION_PLAYER_MAC_ABBR", p.id.to_string()),
        ];
        if signal > 0 && signal <= 100 {
            lines.push(line(
                "INFORMATION_PLAYER_SIGNAL_STRENGTH_ABBR",
                format!("{}%", signal),
            ));
        }
        sections.push(Item {
            name: Some(s("INFORMATION_SPECIFIC_PLAYER").replace("%s", &p.name())),
            items: Some(lines),
            ..Default::default()
        });
    }
    let feed = Feed {
        title: Some(s("INFORMATION")),
        items: sections,
        menu_complete: true,
        ..Default::default()
    };
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    let level = descend(feed, &path, true).unwrap_or(Level::List(Feed::default()));
    xmlbrowser::render(&ctx, level, &mut r);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::jive::xmlbrowser::{render, results_loop};

    fn ctx(q: &str) -> Ctx {
        Ctx {
            query: q.into(),
            terms: vec![q.into(), "items".into()],
            index: 0,
            quantity: 200,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        }
    }

    fn no_fav(_: &str) -> Option<String> {
        None
    }

    fn row() -> Obj {
        obj(json!({
            "id": 101, "title": "Come Together", "artist": "The Beatles", "artist_ids": "12", "composer": "Lennon, McCartney", "composer_ids": "20,21",
            "album": "Abbey Road", "album_id": 7, "genre": "Rock", "genre_id": 3, "year": 1969, "tracknum": 1, "duration": 259.5, "type": "flc",
            "disc": 1, "disccount": 2, "coverart": 1, "comment": "Great", "url": "file:///m/1.flac", "samplerate": 44100, "filesize": 1234567,
        }))
    }

    fn render_items(items: Vec<Item>, q: &str) -> Request {
        let mut r = Request::blank();
        render(
            &ctx(q),
            Level::List(Feed {
                items,
                menu_complete: true,
                ..Default::default()
            }),
            &mut r,
        );
        r
    }

    fn texts(r: &Request) -> Vec<String> {
        results_loop(r, "item_loop")
            .iter()
            .map(|i| i["text"].as_str().unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn trackinfo_items() {
        let tc = TrackCtx {
            has_player: true,
            ..Default::default()
        };
        let r = render_items(
            track_items(&no_fav, &row(), "file:///m/1.flac", false, &tc, None, true),
            "trackinfo",
        );
        let items = results_loop(&r, "item_loop");
        let t = texts(&r);
        assert_eq!(
            &t[..4],
            &["Add to End", "Play Next", "Play", "Save to Favorites"]
        );
        assert_eq!(items[0]["style"], "item_add");
        assert_eq!(
            items[0]["actions"]["go"]["params"],
            json!({ "cmd": "add", "track_id": 101 })
        );
        assert_eq!(items[2]["style"], "itemplay");
        assert_eq!(items[2]["actions"]["go"]["nextWindow"], "nowPlaying");
        assert_eq!(items[3]["style"], "item_fav");
        assert_eq!(
            items[3]["actions"]["go"]["cmd"],
            json!(["jivefavorites", "add"])
        );
        assert_eq!(
            items[3]["actions"]["go"]["params"],
            json!({ "title": "Come Together", "url": "file:///m/1.flac", "isContextMenu": 1 })
        );
        let find = |text: &str| {
            items
                .iter()
                .find(|i| i["text"] == text)
                .unwrap_or_else(|| panic!("missing {}", text))
                .clone()
        };
        let artist = find("Artist: The Beatles");
        assert_eq!(
            artist["actions"]["go"]["params"],
            json!({ "mode": "albums", "artist_id": "12", "menu": 1 })
        );
        assert_eq!(
            artist["actions"]["more"]["cmd"],
            json!(["artistinfo", "items"])
        );
        assert_eq!(
            find("Composer: Lennon")["actions"]["go"]["params"]["role_id"],
            "COMPOSER"
        );
        find("Composer: McCartney");
        let album = find("Album: Abbey Road");
        assert_eq!(
            album["actions"]["go"]["params"],
            json!({ "album_id": "7", "mode": "tracks", "menu": 1 })
        );
        assert_eq!(album["actions"]["play"]["params"]["cmd"], "load");
        find("Genre: Rock");
        assert_eq!(
            find("Year: 1969")["actions"]["go"]["params"]["mode"],
            "albums"
        );
        let art = find("Show Artwork");
        assert_eq!(art["showBigArtwork"], 1);
        assert_eq!(art["actions"]["do"]["cmd"], json!(["artwork", 101]));
        let more = find("More Info");
        assert_eq!(more["actions"]["go"]["cmd"], json!(["trackinfo", "items"]));
        // drill into More Info
        let idx = t.iter().position(|x| x == "More Info").unwrap().to_string();
        let feed = Feed {
            items: track_items(&no_fav, &row(), "file:///m/1.flac", false, &tc, None, true),
            menu_complete: true,
            ..Default::default()
        };
        let mut c = ctx("trackinfo");
        c.crumbs = vec![idx.clone()];
        let mut r = Request::blank();
        render(&c, descend(feed, &[idx], true).unwrap(), &mut r);
        let sub = texts(&r);
        assert_eq!(sub[0], "Disc: 1/2");
        assert_eq!(sub[1], "Track Number: 1");
        assert!(sub.contains(&"Duration: 4:19".to_string()), "{:?}", sub);
        assert!(sub.contains(&"Sample Rate: 44.1 kHz".to_string()));
        assert!(sub.contains(&"File Length: 1,234,567".to_string()));
        assert!(sub.contains(&"Location: /m/1.flac".to_string()));
    }

    #[test]
    fn playlist_context_items() {
        let tc = TrackCtx {
            has_player: true,
            playlist_context: true,
            pindex: 3,
            playlist_len: 5,
            playing_index: 1,
            is_playing: true,
            ..Default::default()
        };
        let items = track_items(&no_fav, &row(), "file:///m/1.flac", false, &tc, None, true);
        assert_eq!(items[0].name.as_deref(), Some("Remove from playlist"));
        assert_eq!(
            items[0].jive.as_ref().unwrap()["actions"]["go"]["cmd"],
            json!(["playlist", "delete", 3])
        );
        assert_eq!(
            items[1].jive.as_ref().unwrap()["actions"]["go"]["cmd"],
            json!(["playlist", "move", 3, 2])
        );
        assert_eq!(
            items[2].jive.as_ref().unwrap()["actions"]["go"]["cmd"],
            json!(["playlist", "jump", 3])
        );
        // the playing track: no Play / Play Next
        let tc = TrackCtx {
            has_player: true,
            playlist_context: true,
            pindex: 1,
            playlist_len: 5,
            playing_index: 1,
            is_playing: true,
            ..Default::default()
        };
        let items = track_items(&no_fav, &row(), "file:///m/1.flac", false, &tc, None, true);
        let names: Vec<String> = items.iter().filter_map(|i| i.name.clone()).collect();
        assert!(!names.contains(&"Play".to_string()));
        assert!(!names.contains(&"Play Next".to_string()));
    }

    #[test]
    fn remote_track_items() {
        let row = obj(json!({ "id": -5, "title": "Groove Salad", "artist": "DJ", "remote": 1 }));
        let tc = TrackCtx {
            has_player: true,
            live_title: Some("DJ - Song".into()),
            ..Default::default()
        };
        let r = render_items(
            track_items(
                &no_fav,
                &row,
                "http://ice2.somafm.com/groovesalad-128-mp3",
                true,
                &tc,
                None,
                true,
            ),
            "trackinfo",
        );
        let t = texts(&r);
        assert!(t.contains(&"Artist: DJ".to_string()));
        assert!(t.contains(&"Title: DJ - Song".to_string()));
    }

    #[test]
    fn album_info_items() {
        let album = obj(
            json!({ "id": 7, "album": "Abbey Road", "year": 1969, "favorites_url": "db:album.title=Abbey%20Road" }),
        );
        let tracks = vec![
            obj(json!({ "artist": "The Beatles", "artist_ids": "12", "duration": 100 })),
            obj(json!({ "artist": "The Beatles", "artist_ids": "12", "duration": 200 })),
        ];
        let fav = |u: &str| (u == "db:album.title=Abbey%20Road").then(|| "4".to_string());
        let r = render_items(
            album_items(&fav, "7", &album, &tracks, &Obj::new(), None, true, true),
            "albuminfo",
        );
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Add to End");
        assert_eq!(items[0]["style"], "item_add");
        assert_eq!(items[0]["actions"]["go"]["cmd"], json!(["playlistcontrol"]));
        assert_eq!(
            items[0]["actions"]["go"]["params"],
            json!({ "album_id": "7", "cmd": "add", "menu": 1 })
        );
        assert_eq!(items[0]["actions"]["go"]["nextWindow"], "parent");
        assert_eq!(items[2]["actions"]["go"]["nextWindow"], "nowPlaying");
        assert_eq!(items[3]["text"], "Delete Favorite");
        assert_eq!(items[3]["actions"]["go"]["params"]["item_id"], "4");
        let t = texts(&r);
        assert_eq!(t.iter().filter(|x| *x == "Artist: The Beatles").count(), 1);
        assert!(t.contains(&"Year: 1969".to_string()));
        assert!(t.contains(&"Duration: 5:00".to_string()));
    }

    #[test]
    fn simple_info_trio() {
        let r = render_items(
            playlistcontrol_trio(&obj(json!({ "artist_id": "12" })), Some("itemplay")),
            "artistinfo",
        );
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[1]["style"], "item_insert");
        assert_eq!(
            items[1]["actions"]["go"]["params"],
            json!({ "artist_id": "12", "cmd": "insert" })
        );
        assert_eq!(items[2]["style"], "itemplay");
        assert!(items[2].get("action").is_none());
    }

    #[test]
    fn number_formatting() {
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(999), "999");
        assert_eq!(mss(259.5), "4:19");
        assert_eq!(mss(3725.0), "1:02:05");
    }
}
