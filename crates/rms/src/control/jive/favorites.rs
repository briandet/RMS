//! Favorites in menu form (`Slim::Plugin::Favorites::Plugin::cliBrowse` with `menu:`) and
//! the Jive favorites/preset commands (`jivefavorites`, `jivepresets`).

use super::xmlbrowser::{
    self, descend, obj, split_item_id, value_i64, ActionDef, Actions, Ctx, Feed, Item, Level, Obj,
};
use crate::control::request::Request;
use crate::plugins::favorites::Fav;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

/// A `db:<class>.<field>=<value>` favorite resolved to a library object.
#[derive(Debug, Clone, PartialEq)]
pub enum DbObject {
    Album(String),
    Artist(String),
    Genre(String),
    Year(String),
}

/// Parse a `db:` favorites URL into `(class, [(field, value)])`.
pub fn parse_db_url(url: &str) -> Option<(String, Vec<(String, String)>)> {
    let rest = url.strip_prefix("db:")?;
    let mut class = None;
    let mut fields = Vec::new();
    for part in rest.split('&') {
        let (k, v) = part.split_once('=')?;
        let (c, f) = k.split_once('.')?;
        class.get_or_insert_with(|| c.to_string());
        fields.push((
            format!("{}.{}", c, f),
            percent_encoding::percent_decode_str(v)
                .decode_utf8_lossy()
                .to_string(),
        ));
    }
    Some((class?, fields))
}

/// Look up a `db:` favorite in the library (`_prepareDbItems`).
pub fn resolve_db(server: &Server, url: &str) -> Option<DbObject> {
    let (class, fields) = parse_db_url(url)?;
    let field = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
    let query = |name: rms_library::LibQueryName, search: &str| {
        let q = rms_library::LibQuery {
            name,
            start: Some(0),
            quantity: Some(200),
            params: [
                ("search".to_string(), search.to_string()),
                ("tags".to_string(), "la".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        server.library.query(&q).ok()
    };
    let first_match = |rows: Option<Obj>,
                       loop_name: &str,
                       key: &str,
                       want: &str,
                       extra: Option<(&str, String)>|
     -> Option<String> {
        let rows = rows?;
        let list = rows.get(loop_name)?.as_array()?;
        list.iter()
            .filter_map(Value::as_object)
            .find(|o| {
                o.get(key)
                    .and_then(Value::as_str)
                    .map(|v| v.eq_ignore_ascii_case(want))
                    .unwrap_or(false)
                    && extra
                        .as_ref()
                        .map(|(k, v)| {
                            o.get(*k)
                                .and_then(Value::as_str)
                                .map(|x| x.eq_ignore_ascii_case(v))
                                .unwrap_or(true)
                        })
                        .unwrap_or(true)
            })
            .and_then(|o| o.get("id").and_then(xmlbrowser::value_string))
    };
    match class.as_str() {
        "album" => {
            let title = field("album.title")?;
            let artist = field("contributor.name");
            first_match(
                query(rms_library::LibQueryName::Albums, &title),
                "albums_loop",
                "album",
                &title,
                artist.map(|a| ("artist", a)),
            )
            .map(DbObject::Album)
        }
        "contributor" => {
            let name = field("contributor.name")?;
            first_match(
                query(rms_library::LibQueryName::Artists, &name),
                "artists_loop",
                "artist",
                &name,
                None,
            )
            .map(DbObject::Artist)
        }
        "genre" => {
            let name = field("genre.name")?;
            first_match(
                query(rms_library::LibQueryName::Genres, &name),
                "genres_loop",
                "genre",
                &name,
                None,
            )
            .map(DbObject::Genre)
        }
        "year" => field("year.id").map(DbObject::Year),
        _ => None,
    }
}

/// Browse/play actions for a resolved `db:` favorite (BrowseLibrary feeds with `library_id:-1`).
pub fn db_item_actions(obj_: &DbObject) -> Actions {
    let (key, id, mode, info) = match obj_ {
        DbObject::Album(id) => ("album_id", id, "tracks", "albuminfo"),
        DbObject::Artist(id) => ("artist_id", id, "albums", "artistinfo"),
        DbObject::Genre(id) => ("genre_id", id, "artists", "genreinfo"),
        DbObject::Year(id) => ("year", id, "albums", "yearinfo"),
    };
    let p = obj(json!({ key: id }));
    let mut items = p.clone();
    items.insert("mode".into(), json!(mode));
    Actions {
        all_defined: true,
        items: Some(ActionDef::cmd(&["browselibrary", "items"], items)),
        info: Some(ActionDef::cmd(&[info, "items"], p.clone())),
        ..Default::default()
    }
    .with_playlistcontrol(&p)
}

/// One favorite as an OPML item (`OpmlFavorites::xmlbrowser` → `parseOPML`).
pub fn fav_item(f: &Fav, resolve: &dyn Fn(&str) -> Option<DbObject>) -> Item {
    let mut it = Item {
        name: Some(f.text.clone()),
        icon: f.icon.clone(),
        ..Default::default()
    };
    match &f.url {
        None => {
            it.items = Some(f.children.iter().map(|c| fav_item(c, resolve)).collect());
            it.icon
                .get_or_insert_with(|| "html/images/favorites.png".into());
        }
        Some(url) => {
            it.url = Some(url.clone());
            it.kind = Some(f.kind.clone().unwrap_or_else(|| "audio".into()));
            if url.starts_with("db:") {
                if let Some(o) = resolve(url) {
                    it.kind = Some("playlist".into());
                    it.play = Some(format!("{}&libraryTracks.library=-1", url));
                    it.fields.insert("favorites_url".into(), json!(url));
                    it.item_actions = Some(db_item_actions(&o));
                }
            }
            if it.icon.is_none() {
                let default = if crate::util::is_remote_url(url) {
                    "html/images/radio.png"
                } else {
                    "html/images/favorites.png"
                };
                it.icon = Some(default.into());
            }
        }
    }
    it
}

/// Leaf favorites whose name contains `search` (case-insensitive), flattening folders.
fn search_items(items: &[Item], search: &str, out: &mut Vec<Item>) {
    let needle = search.to_lowercase();
    for it in items {
        match &it.items {
            Some(children) => search_items(children, search, out),
            None => {
                if it
                    .name
                    .as_deref()
                    .map(|n| n.to_lowercase().contains(&needle))
                    .unwrap_or(false)
                {
                    out.push(it.clone());
                }
            }
        }
    }
}

/// `favorites items <start> <count> menu:… [item_id:…] [search:…]`.
pub fn items(server: &Arc<Server>, r: &mut Request) {
    super::fix_index_quantity(r);
    let mut ctx = Ctx::from_request(server, r, "favorites");
    let resolve = |u: &str| resolve_db(server, u);
    let mut items: Vec<Item> = server
        .favorites
        .all()
        .iter()
        .map(|f| fav_item(f, &resolve))
        .collect();
    let mut title = s("FAVORITES");
    if let Some(search) = r.param_nonempty("search").map(str::to_string) {
        let mut found = Vec::new();
        search_items(&items, &search, &mut found);
        items = found;
        title = format!("{} (\"{}\")", title, search);
    }
    let feed = Feed {
        title: Some(title),
        items,
        ..Default::default()
    };
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    let level = descend(feed, &path, true).unwrap_or(Level::List(Feed::default()));
    xmlbrowser::render(&ctx, level, r);
}

/// `jiveFavoritesCommand`: `add|delete` confirmation menus and `set_preset`.
pub fn jivefavorites(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = super::player(server, r) else {
        return r.bad_dispatch();
    };
    let cmd = r.param("_cmd").unwrap_or("").to_string();
    match cmd.as_str() {
        "add" | "delete" => {
            let title = r.param("title").unwrap_or("").to_string();
            let mut params = obj(
                json!({ "title": title, "url": r.param("url"), "type": r.param("type"), "parser": r.param("parser") }),
            );
            params.retain(|_, v| !v.is_null());
            if let Some(icon) = r.param_nonempty("icon") {
                params.insert("icon".into(), json!(icon));
            }
            if cmd == "delete" {
                if let Some(id) = r.param_nonempty("item_id") {
                    params.insert("item_id".into(), json!(id));
                }
            }
            let menu = json!([
                { "text": s("CANCEL"), "actions": { "go": { "player": 0, "cmd": ["jiveblankcommand"] } }, "nextWindow": "parent" },
                { "text": format!("{} {}", s(&cmd.to_uppercase()), title), "actions": { "go": { "player": 0, "cmd": ["favorites", cmd], "params": params } }, "nextWindow": "grandparent" },
            ]);
            r.add_result("offset", 0);
            r.add_result("count", 2);
            r.add_result("item_loop", menu);
            r.set_done();
        }
        "set_preset" => {
            let key = r.param_i64("key").unwrap_or(1);
            let slot = if key == 0 { 10 } else { key.clamp(1, 10) } as usize;
            let (mut url, mut title, mut kind) = (
                r.param("favorites_url").map(str::to_string),
                r.param("favorites_title").unwrap_or("").to_string(),
                r.param("favorites_type").unwrap_or("audio").to_string(),
            );
            if let Some(i) = r.param_i64("playlist_index") {
                let e = p
                    .controller()
                    .lock()
                    .playlist
                    .song(i.max(0) as usize)
                    .cloned();
                if let Some(e) = e {
                    title = server
                        .track_info_for(&e)
                        .title
                        .unwrap_or_else(|| crate::player::controller::title_from_url(&e.url));
                    url = Some(e.url);
                    kind = "audio".into();
                }
            }
            let Some(url) = url else {
                return r.bad_params();
            };
            if kind != "playlist" {
                kind = "audio".into();
            }
            let mut presets: Vec<Value> = p
                .prefs
                .get("presets")
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            presets.resize(10, json!({}));
            let mut preset = obj(json!({ "URL": url, "text": title, "type": kind }));
            if let Some(parser) = r.param_nonempty("parser") {
                preset.insert("parser".into(), json!(parser));
            }
            presets[slot - 1] = Value::Object(preset);
            p.prefs.set("presets", Value::Array(presets));
            crate::control::commands::show_briefly(
                server,
                &p,
                vec![s("PRESET_ADDING").replace("%s", &slot.to_string()), title],
                3.0,
            );
            r.set_done();
        }
        _ => r.bad_params(),
    }
}

/// `jivePresetsMenu`: "Set Preset N" items for the given favorite.
pub fn jivepresets(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = super::player(server, r) else {
        return r.bad_dispatch();
    };
    let presets: Vec<Value> = p
        .prefs
        .get("presets")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    let kind = match r.param("type") {
        Some("playlist") => "playlist",
        _ => "audio",
    };
    let mut set_params = obj(json!({
        "favorites_url": r.param("url"), "favorites_title": r.param("title"), "favorites_type": kind, "parser": r.param("parser"),
    }));
    set_params.retain(|_, v| !v.is_null());
    if let Some(i) = r.param_i64("playlist_index") {
        set_params.insert("playlist_index".into(), json!(i));
    }
    let mut menu = Vec::new();
    for n in 0..10usize {
        let jive_preset = n + 1;
        let mut params = set_params.clone();
        params.insert("key".into(), json!(jive_preset));
        let action = json!({ "go": { "player": 0, "cmd": ["jivefavorites", "set_preset"], "params": params } });
        let text = s("JIVE_SET_PRESET_X").replace("%s", &jive_preset.to_string());
        let current = presets
            .get(n)
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        match current {
            None => menu.push(json!({ "text": text, "actions": action, "nextWindow": "presets" })),
            Some(cur) => menu.push(json!({ "text": text, "count": 2, "offset": 0, "isContextMenu": 1, "item_loop": [
                { "text": s("CANCEL"), "actions": { "go": { "player": 0, "cmd": ["jiveblankcommand"] } }, "nextWindow": "parent" },
                { "text": s("JIVE_OVERWRITE_PRESET_X").replace("%s", cur), "actions": action, "nextWindow": "presets" },
            ] })),
        }
    }
    let _ = value_i64;
    super::slice_and_ship(r, menu);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::jive::xmlbrowser::{render, results_loop};

    fn favs() -> Vec<Fav> {
        vec![
            Fav {
                text: "Groove Salad".into(),
                url: Some("http://ice2.somafm.com/groovesalad-128-mp3".into()),
                kind: Some("audio".into()),
                ..Default::default()
            },
            Fav {
                text: "Abbey Road".into(),
                url: Some("db:album.title=Abbey%20Road&contributor.name=The%20Beatles".into()),
                kind: Some("audio".into()),
                ..Default::default()
            },
            Fav {
                text: "Folder".into(),
                children: vec![Fav {
                    text: "Inner".into(),
                    url: Some("file:///m/a.flac".into()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ]
    }

    fn ctx() -> Ctx {
        Ctx {
            query: "favorites".into(),
            terms: vec!["favorites".into(), "items".into()],
            index: 0,
            quantity: 200,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        }
    }

    #[test]
    fn db_urls() {
        let (class, fields) =
            parse_db_url("db:album.title=Abbey%20Road&contributor.name=The%20Beatles").unwrap();
        assert_eq!(class, "album");
        assert_eq!(
            fields[0],
            ("album.title".to_string(), "Abbey Road".to_string())
        );
        assert_eq!(fields[1].1, "The Beatles");
        assert!(parse_db_url("http://x").is_none());
    }

    #[test]
    fn favorites_menu_items() {
        let resolve = |u: &str| {
            u.starts_with("db:album")
                .then(|| DbObject::Album("7".into()))
        };
        let items: Vec<Item> = favs().iter().map(|f| fav_item(f, &resolve)).collect();
        let feed = Feed {
            title: Some("Favorites".into()),
            items,
            ..Default::default()
        };
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        // audio favorite: touch-to-play
        assert_eq!(items[0]["type"], "audio");
        assert_eq!(items[0]["goAction"], "play");
        assert_eq!(
            items[0]["params"],
            json!({ "item_id": "0", "isContextMenu": 1, "touchToPlay": "0", "touchToPlaySingle": 1 })
        );
        assert_eq!(
            items[0]["presetParams"]["favorites_url"],
            "http://ice2.somafm.com/groovesalad-128-mp3"
        );
        assert_eq!(items[0]["icon-id"], "html/images/radio.png");
        // db: favorite browsable through the library
        assert_eq!(items[1]["type"], "playlist");
        assert_eq!(
            items[1]["actions"]["go"]["params"],
            json!({ "album_id": "7", "mode": "tracks", "menu": 1 })
        );
        assert_eq!(items[1]["actions"]["play"]["params"]["cmd"], "load");
        // folder
        assert_eq!(
            items[2]["actions"]["go"]["cmd"],
            json!(["favorites", "items"])
        );
        assert_eq!(
            items[2]["actions"]["go"]["params"],
            json!({ "menu": "favorites", "item_id": "2" })
        );
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["play"]["cmd"],
            json!(["favorites", "playlist", "play"])
        );
        assert_eq!(base["actions"]["more"]["window"]["isContextMenu"], 1);
        assert!(base["actions"]["set-preset-3"].is_object());
        assert_eq!(r.result("title"), Some(&json!("Favorites")));
    }

    #[test]
    fn favorite_context_menu_has_delete() {
        let it = fav_item(&favs()[0], &|_| None);
        let mut c = ctx();
        c.interim_cm = true;
        c.item_id = Some("0".into());
        c.fav_index = Some(xmlbrowser::FavLookup(Arc::new(|u: &str| {
            (u.contains("groovesalad")).then(|| "0".to_string())
        })));
        let mut r = Request::blank();
        render(&c, Level::Leaf(Box::new(it), Feed::default()), &mut r);
        let items = results_loop(&r, "item_loop");
        let del = items
            .iter()
            .find(|i| i["text"] == "Delete Favorite")
            .unwrap();
        assert_eq!(
            del["actions"]["go"]["cmd"],
            json!(["jivefavorites", "delete"])
        );
        assert_eq!(del["actions"]["go"]["params"]["item_id"], "0");
        assert_eq!(
            items[0]["actions"]["go"]["cmd"],
            json!(["favorites", "playlist", "add"])
        );
    }

    #[test]
    fn search_flattens() {
        let items: Vec<Item> = favs().iter().map(|f| fav_item(f, &|_| None)).collect();
        let mut out = Vec::new();
        search_items(&items, "inn", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name.as_deref(), Some("Inner"));
    }
}
