//! `browselibrary items|playlist` (`Slim::Menu::BrowseLibrary`) and `globalsearch items`
//! (`Slim::Menu::GlobalSearch`).
//!
//! Every mode runs the matching library CLI query (`artists`, `albums`, `titles`,
//! `musicfolder`, …) through the dispatcher and converts its loop into OPML items with the
//! BrowseLibrary actions; [`super::xmlbrowser`] renders them.  The conversion functions are
//! pure (CLI result JSON in, [`Feed`] out) so they can be tested without a library.

use super::xmlbrowser::{
    self, component_index, component_search, escape_search, obj, split_item_id, value_i64,
    value_string, ActionDef, Actions, Ctx, Feed, Item, Level, Obj,
};
use crate::control::request::Request;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

const BROWSELIBRARY: &str = "browselibrary";

/// `@topLevelArgs`: params that become `searchTags` (`key:value` filters).
const TOP_LEVEL_ARGS: &[&str] = &[
    "track_id",
    "artist_id",
    "genre_id",
    "album_id",
    "playlist_id",
    "year",
    "only_album_years",
    "folder_id",
    "role_id",
    "library_id",
    "remote_library",
    "release_type",
    "work_id",
    "composer_id",
    "from_search",
    "subtitle",
    "grouping",
    "performance",
    "menu_mode",
    "menu_roles",
];

/// How many items to fetch when a whole level is needed (play all).
const ALL: usize = 100_000;

/// The routing parameters of one BrowseLibrary level (`_topLevel` args + passthrough).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrowseParams {
    pub mode: String,
    /// `searchTags`, in `@topLevelArgs` order (only non-empty values).
    pub tags: Vec<(String, String)>,
    pub sort: Option<String>,
    pub order_by: Option<String>,
    pub search: Option<String>,
    pub want_metadata: bool,
    pub menu_style: Option<String>,
}

impl BrowseParams {
    /// From request params (or a stored `_next` map).
    pub fn from_map(get: impl Fn(&str) -> Option<String>) -> BrowseParams {
        let nonempty = |k: &str| get(k).filter(|v| !v.is_empty());
        BrowseParams {
            mode: nonempty("mode").unwrap_or_default(),
            tags: TOP_LEVEL_ARGS
                .iter()
                .filter_map(|k| nonempty(k).map(|v| (k.to_string(), v)))
                .collect(),
            sort: nonempty("sort"),
            order_by: nonempty("orderBy"),
            search: nonempty("search"),
            want_metadata: nonempty("wantMetadata").map(|v| v != "0").unwrap_or(false),
            menu_style: nonempty("menuStyle"),
        }
    }

    fn from_obj(o: &Obj) -> BrowseParams {
        BrowseParams::from_map(|k| o.get(k).and_then(value_string))
    }

    /// `%entryParams` → `$feed->{query}`.
    pub fn entry_query(&self) -> Obj {
        let mut q = Obj::new();
        for (k, v) in &self.tags {
            q.insert(k.clone(), json!(v));
        }
        if let Some(s) = &self.sort {
            q.insert("sort".into(), json!(s));
        }
        if let Some(s) = &self.search {
            q.insert("search".into(), json!(s));
        }
        if !self.mode.is_empty() {
            q.insert("mode".into(), json!(self.mode));
        }
        q
    }

    fn tag(&self, k: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }

    fn has_tag(&self, k: &str) -> bool {
        self.tag(k).is_some()
    }
}

/// `_tagsToParams`.
fn tags_to_params(tags: &[(String, String)]) -> Obj {
    let mut p = Obj::new();
    for (k, v) in tags {
        p.insert(k.clone(), json!(v));
    }
    p
}

fn with(mut p: Obj, k: &str, v: Value) -> Obj {
    p.insert(k.into(), v);
    p
}

fn tag_tokens(tags: &[(String, String)]) -> Vec<String> {
    tags.iter()
        .filter(|(k, v)| !(k == "library_id" && v == "-1") && k != "remote_library")
        .map(|(k, v)| format!("{}:{}", k, v))
        .collect()
}

fn next_level(mode: &str, tags: &[(String, String)], extra: &[(&str, String)]) -> Value {
    let mut o = tags_to_params(tags);
    o.insert("mode".into(), json!(mode));
    for (k, v) in extra {
        if !v.is_empty() {
            o.insert(k.to_string(), json!(v));
        }
    }
    Value::Object(o)
}

fn loop_of<'a>(results: &'a Obj, name: &str) -> Vec<&'a Obj> {
    match results.get(name) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_object).collect(),
        _ => Vec::new(),
    }
}

fn get_str(o: &Obj, k: &str) -> Option<String> {
    o.get(k).and_then(value_string).filter(|s| !s.is_empty())
}

/// An item that copies the CLI fields (`$_` in the Perl loops).
fn cli_item(o: &Obj) -> Item {
    Item {
        fields: o.clone(),
        textkey: get_str(o, "textkey"),
        ..Default::default()
    }
}

/// The result of one mode: items, feed actions and extra items (`$resultsFunc`).
pub struct ModeResult {
    pub feed: Feed,
    pub extras: Vec<Item>,
}

// ---- per-mode CLI query + result conversion -------------------------------------------------

/// `_artists`: CLI query words.
pub fn artists_query(bp: &BrowseParams, library_id: Option<&str>) -> Vec<String> {
    let mut tags = bp.tags.clone();
    let mut search = bp.search.clone();
    if search.is_some() && tags.is_empty() {
        if let Some(l) = library_id {
            tags.push(("library_id".into(), l.into()));
        }
    }
    let filtered = (search.is_some() && !tags.iter().any(|(k, _)| k != "library_id"))
        || tags
            .iter()
            .any(|(k, v)| !(k == "library_id" || (k == "role_id" && v.contains("ALBUMARTIST"))));
    let mut q = vec!["artists".to_string()];
    let mut extra = tag_tokens(&tags);
    if let Some(sort) = &bp.sort {
        extra.push(format!("sort:{}", sort));
    }
    if let Some(s) = search.take() {
        extra.push(format!("search:{}", s));
    }
    if filtered {
        extra.push("tags:s4".into());
    } else {
        extra.push("include_online_only_artists:1".into());
        extra.push("tags:Es4".into());
    }
    q.extend(extra);
    q
}

/// `_artists` result conversion.
pub fn artists_result(bp: &BrowseParams, results: &Obj) -> ModeResult {
    let pt_tags = bp.tags.clone();
    let items = loop_of(results, "artists_loop")
        .into_iter()
        .map(|a| {
            let mut it = cli_item(a);
            it.name = get_str(a, "artist");
            it.kind = Some("playlist".into());
            it.playlist_ref = true;
            it.lazy = true;
            match get_str(a, "portraitid") {
                Some(p) => it.image = Some(format!("contributor/{}/image", p)),
                None => it.icon = Some("html/images/artists.png".into()),
            }
            let id = get_str(a, "id").unwrap_or_default();
            it.fields.insert(
                "_next".into(),
                next_level("albums", &pt_tags, &[("artist_id", id)]),
            );
            it
        })
        .collect();
    let params = tags_to_params(&pt_tags);
    let actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["artistinfo", "items"], Obj::new())),
        items: Some(ActionDef::cmd(
            &[BROWSELIBRARY, "items"],
            with(params.clone(), "mode", json!("albums")),
        )),
        ..Default::default()
    }
    .with_playlistcontrol(&params)
    .common(&[("artist_id", "id")]);
    let mut extras = Vec::new();
    if bp
        .tags
        .iter()
        .any(|(k, _)| k != "role_id" && k != "remote_library")
    {
        let p = tags_to_params(&bp.tags);
        let ia = Actions {
            all_defined: true,
            info: Some(ActionDef::none()),
            items: Some(ActionDef::cmd(
                &[BROWSELIBRARY, "items"],
                with(p.clone(), "mode", json!("albums")),
            )),
            ..Default::default()
        }
        .with_playlistcontrol(&p);
        let mut it = Item {
            name: Some(s("ALL_ALBUMS")),
            icon: Some("html/images/albums.png".into()),
            kind: Some("playlist".into()),
            playlist_ref: true,
            lazy: true,
            skip_if_singleton: true,
            item_actions: Some(ia),
            ..Default::default()
        };
        it.fields
            .insert("_next".into(), next_level("albums", &bp.tags, &[]));
        extras.push(it);
    }
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras,
    }
}

/// `_genres`.
pub fn genres_query(bp: &BrowseParams, library_id: Option<&str>) -> Vec<String> {
    let mut tags = bp.tags.clone();
    if bp.search.is_some() && tags.is_empty() {
        if let Some(l) = library_id {
            tags.push(("library_id".into(), l.into()));
        }
    }
    let mut q = vec!["genres".to_string()];
    q.extend(tag_tokens(&tags));
    if let Some(s) = &bp.search {
        q.push(format!("search:{}", s));
    }
    q.push("tags:s".into());
    q
}

pub fn genres_result(bp: &BrowseParams, results: &Obj, unified: bool) -> ModeResult {
    let mut tags = bp.tags.clone();
    if !unified {
        tags.push(("role_id".into(), "ALBUMARTIST".into()));
    }
    let items = loop_of(results, "genres_loop")
        .into_iter()
        .map(|g| {
            let mut it = cli_item(g);
            it.name = get_str(g, "genre");
            it.kind = Some("playlist".into());
            it.playlist_ref = true;
            it.lazy = true;
            it.fields.insert(
                "_next".into(),
                next_level(
                    "artists",
                    &tags,
                    &[("genre_id", get_str(g, "id").unwrap_or_default())],
                ),
            );
            it
        })
        .collect();
    let params = tags_to_params(&tags);
    let actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["genreinfo", "items"], Obj::new())),
        items: Some(ActionDef::cmd(
            &[BROWSELIBRARY, "items"],
            with(params.clone(), "mode", json!("artists")),
        )),
        ..Default::default()
    }
    .with_playlistcontrol(&params)
    .common(&[("genre_id", "id")]);
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras: vec![],
    }
}

/// `_years`.
pub fn years_query(
    bp: &BrowseParams,
    library_id: Option<&str>,
    only_album_years: bool,
) -> Vec<String> {
    let mut tags = bp.tags.clone();
    if let Some(l) = library_id {
        if !tags.iter().any(|(k, _)| k == "library_id") {
            tags.push(("library_id".into(), l.into()));
        }
    }
    let mut q = vec![
        "years".to_string(),
        format!("hasAlbums:{}", only_album_years as i32),
    ];
    q.extend(tag_tokens(&tags));
    q
}

pub fn years_result(bp: &BrowseParams, results: &Obj) -> ModeResult {
    let tags = bp.tags.clone();
    let items = loop_of(results, "years_loop")
        .into_iter()
        .map(|y| {
            let mut it = cli_item(y);
            let year = get_str(y, "year").unwrap_or_default();
            it.name = Some(year.clone());
            it.kind = Some("playlist".into());
            it.playlist_ref = true;
            it.lazy = true;
            it.fields.insert(
                "_next".into(),
                next_level("albums", &tags, &[("year", year)]),
            );
            it
        })
        .collect();
    let params = tags_to_params(&tags);
    let actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["yearinfo", "items"], Obj::new())),
        items: Some(ActionDef::cmd(
            &[BROWSELIBRARY, "items"],
            with(params.clone(), "mode", json!("albums")),
        )),
        ..Default::default()
    }
    .with_playlistcontrol(&params)
    .common(&[("year", "name")]);
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras: vec![],
    }
}

/// Valid album sorts (`%orderByList` values + the special sorts).
const ALBUM_SORTS: &[&str] = &[
    "new",
    "changed",
    "random",
    "popular",
    "album",
    "yearalbum",
    "yearartistalbum",
    "artistalbum",
    "artflow",
];

/// `_albums`: effective sort.
pub fn albums_sort(bp: &BrowseParams) -> Option<String> {
    let special = |s: &str| matches!(s, "random" | "new" | "changed" | "popular");
    let mut sort = bp.sort.clone();
    if !sort.as_deref().map(special).unwrap_or(false) {
        sort = bp.order_by.clone().or(sort);
    }
    let sort = sort?;
    let mapped = if bp.has_tag("artist_id") {
        match sort.as_str() {
            "album" | "artistalbum" => "album".to_string(),
            "yearalbum" | "yearartistalbum" | "artflow" => "yearalbum".to_string(),
            other => other.to_string(),
        }
    } else {
        sort
    };
    ALBUM_SORTS.contains(&mapped.as_str()).then_some(mapped)
}

pub fn albums_query(bp: &BrowseParams, library_id: Option<&str>) -> Vec<String> {
    let mut tags = bp.tags.clone();
    if bp.search.is_some() && tags.is_empty() {
        if let Some(l) = library_id {
            tags.push(("library_id".into(), l.into()));
        }
    }
    let mut t = "ljsaaSSKEw".to_string();
    if !bp.has_tag("year") {
        t.push('y');
    }
    let mut q = vec!["albums".to_string()];
    q.extend(tag_tokens(&tags));
    if let Some(sort) = albums_sort(bp) {
        q.push(format!("sort:{}", sort));
    }
    if let Some(s) = &bp.search {
        q.push(format!("search:{}", s));
    }
    q.push(format!("tags:{}", t));
    q
}

/// Album display name (`[composer: ][work ([performance ]from ]album[)]`).
fn album_name(a: &Obj) -> String {
    let mut name = match get_str(a, "composer") {
        Some(c) => format!("{}{} ", c, s("COLON")),
        None => String::new(),
    };
    let work = get_str(a, "work_id").is_some();
    if work {
        name.push_str(&get_str(a, "work_name").unwrap_or_default());
        name.push_str(" (");
        if let Some(p) = get_str(a, "performance") {
            name.push_str(&p);
            name.push(' ');
        }
        name.push_str(&s("FROM"));
        name.push(' ');
    }
    name.push_str(&get_str(a, "album").unwrap_or_default());
    if work {
        name.push(')');
    }
    name
}

/// Split `artists` like `split /(?<!\s),(?!\s)/` (commas not adjacent to spaces).
fn split_artists(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, c) in chars.iter().enumerate() {
        let prev_space = i > 0 && chars[i - 1].is_whitespace();
        let next_space = chars.get(i + 1).map(|c| c.is_whitespace()).unwrap_or(false);
        if *c == ',' && !prev_space && !next_space {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(*c);
        }
    }
    out.push(cur);
    out
}

pub fn albums_result(bp: &BrowseParams, results: &Obj, only_album_years: bool) -> ModeResult {
    let sort = albums_sort(bp);
    let artist_id = bp.tag("artist_id").map(str::to_string);
    let track_artist_only = bp
        .tag("role_id")
        .map(|r| r.contains("TRACKARTIST"))
        .unwrap_or(false);
    let mut search_tags = bp.tags.clone();
    let items: Vec<Item> = loop_of(results, "albums_loop")
        .into_iter()
        .map(|a| {
            let mut it = cli_item(a);
            it.name = Some(album_name(a));
            if let Some(art) = get_str(a, "artwork_track_id") {
                it.image = Some(format!("music/{}/cover", art));
            } else if let Some(u) = get_str(a, "artwork_url") {
                it.image = Some(u);
            }
            it.kind = Some("playlist".into());
            it.playlist_ref = true;
            it.lazy = true;
            let artists: Vec<String> = match (get_str(a, "artist_ids"), get_str(a, "artists")) {
                (Some(ids), Some(names)) if ids.contains(',') => split_artists(&names),
                (Some(_), Some(names)) => vec![names],
                _ => get_str(a, "artist").into_iter().collect(),
            };
            let primary = get_str(a, "artist_id");
            if artist_id.is_none()
                || artist_id != primary
                || track_artist_only
                || get_str(a, "work_id").is_some()
            {
                let n2 = artists.join(", ");
                it.name2 = if n2.is_empty() {
                    get_str(a, "artist")
                } else {
                    Some(n2)
                };
            }
            if !bp.want_metadata {
                it.fields.remove("artist");
            }
            it.fields.entry("performance").or_insert(json!(""));
            let id = get_str(a, "id").unwrap_or_default();
            it.fields.insert(
                "_next".into(),
                next_level(
                    "tracks",
                    &bp.tags,
                    &[
                        ("album_id", id),
                        ("performance", get_str(a, "performance").unwrap_or_default()),
                    ],
                ),
            );
            it
        })
        .collect();
    let mut extras = Vec::new();
    if bp.has_tag("year") {
        search_tags.push((
            "only_album_years".into(),
            (only_album_years as i32).to_string(),
        ));
    }
    let special = matches!(
        sort.as_deref(),
        Some("new") | Some("changed") | Some("random")
    );
    if search_tags.iter().any(|(k, _)| k != "remote_library") && !special {
        let p = tags_to_params(&search_tags);
        let mut items_params = p.clone();
        items_params.insert("mode".into(), json!("tracks"));
        items_params.insert("performance".into(), json!("-1"));
        let ia = Actions {
            all_defined: true,
            info: Some(ActionDef::none()),
            items: Some(ActionDef::cmd(&[BROWSELIBRARY, "items"], items_params)),
            ..Default::default()
        }
        .with_playlistcontrol(&p);
        let mut it = Item {
            name: Some(s("ALL_SONGS")),
            icon: Some("html/images/albums.png".into()),
            kind: Some("playlist".into()),
            playlist_ref: true,
            lazy: true,
            skip_if_singleton: true,
            item_actions: Some(ia),
            ..Default::default()
        };
        let mut next = search_tags.clone();
        next.push(("performance".into(), "-1".into()));
        it.fields.insert(
            "_next".into(),
            next_level(
                "tracks",
                &next,
                &[
                    ("sort", "albumtrack".into()),
                    ("menuStyle", "menuStyle:allSongs".into()),
                ],
            ),
        );
        extras.push(it);
    }
    let params = tags_to_params(&search_tags);
    let actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["albuminfo", "items"], params.clone())),
        items: Some(ActionDef::cmd(
            &[BROWSELIBRARY, "items"],
            with(params.clone(), "mode", json!("tracks")),
        )),
        ..Default::default()
    }
    .with_playlistcontrol(&params)
    .common(&[("album_id", "id"), ("performance", "performance")]);
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras,
    }
}

/// `_tracks`: CLI query, or `None` for the "no search term or tags" sanity failure.
pub fn tracks_query(bp: &BrowseParams, library_id: Option<&str>) -> Option<Vec<String>> {
    let mut tags = bp.tags.clone();
    if bp.search.is_some() && tags.is_empty() {
        if let Some(l) = library_id {
            tags.push(("library_id".into(), l.into()));
        }
    }
    if bp.search.as_deref().unwrap_or("").is_empty() && tags.is_empty() {
        return None;
    }
    let mut t = "dtuxgaAsSliqyorf".to_string();
    if bp.want_metadata {
        t.push('k');
    }
    if !bp.has_tag("album_id") {
        t.push_str("cJK");
    }
    let sort = match (&bp.sort, &bp.search) {
        (Some(s), _) => Some(s.trim_start_matches("sort:").to_string()),
        (None, Some(_)) => None,
        (None, None) => Some("albumtrack".into()),
    };
    let mut q = vec!["titles".to_string(), format!("tags:{}", t)];
    if let Some(s) = sort {
        q.push(format!("sort:{}", s));
    }
    q.push(
        bp.menu_style
            .clone()
            .map(|m| {
                if m.starts_with("menuStyle:") {
                    m
                } else {
                    format!("menuStyle:{}", m)
                }
            })
            .unwrap_or_else(|| "menuStyle:album".into()),
    );
    q.extend(tag_tokens(&tags));
    if let Some(s) = &bp.search {
        q.push(format!("search:{}", s));
    }
    Some(q)
}

fn secs_to_mss(secs: f64) -> String {
    let s = secs as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Common per-track conversion of `_tracks` / `_playlistTracks`.
fn track_item(t: &Obj, play_index: usize) -> Item {
    let mut it = cli_item(t);
    if let Some(ty) = t.get("type").cloned() {
        it.fields.insert("ct".into(), ty);
    }
    if let Some(secs) = t
        .get("duration")
        .and_then(|d| {
            d.as_f64()
                .or_else(|| value_string(d).and_then(|s| s.parse().ok()))
        })
        .filter(|d| *d > 0.0)
    {
        it.fields.insert("secs".into(), json!(secs));
        it.fields
            .insert("duration".into(), json!(secs_to_mss(secs)));
    }
    it.name = get_str(t, "title");
    it.kind = Some("audio".into());
    it.url = get_str(t, "url");
    it.playall = true;
    it.fields.insert("play_index".into(), json!(play_index));
    it
}

pub fn tracks_result(bp: &BrowseParams, results: &Obj, offset: usize) -> ModeResult {
    let add_album = !bp.has_tag("album_id");
    let add_artist = add_album && !bp.has_tag("artist_id");
    let mut track_ids = String::new();
    let items: Vec<Item> = loop_of(results, "titles_loop")
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let mut it = track_item(t, offset + i);
            track_ids.push_str(&format!("{},", get_str(t, "id").unwrap_or_default()));
            if get_str(t, "work_id").is_some() {
                it.fields.remove("tracknum");
            }
            let mut artist = get_str(t, "artist");
            if let Some(ta) = get_str(t, "trackartist") {
                artist = Some(ta);
            }
            let mut name2 = String::new();
            if add_artist {
                name2 = artist.unwrap_or_default();
            }
            if add_album {
                if let Some(al) = get_str(t, "album") {
                    if !name2.is_empty() {
                        name2.push_str(" - ");
                    }
                    name2.push_str(&al);
                }
            }
            if !name2.is_empty() {
                let art = get_str(t, "coverid").or_else(|| get_str(t, "artwork_track_id"));
                it.artwork_track_id = art.clone();
                it.name2 = Some(name2);
                it.image = art
                    .map(|a| format!("music/{}/cover", a))
                    .or_else(|| get_str(t, "artwork_url"));
            }
            it
        })
        .collect();
    let params = tags_to_params(&bp.tags);
    let pc = |cmd: &str| ActionDef::cmd(&["playlistcontrol"], obj(json!({ "cmd": cmd })));
    let mut actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["trackinfo", "items"], params.clone())),
        play: Some(pc("load")),
        add: Some(pc("add")),
        insert: Some(pc("insert")),
        ..Default::default()
    }
    .common(&[("track_id", "id")]);
    actions.items = actions.info.clone();
    let mut extras = Vec::new();
    let searching = bp.search.is_some();
    if searching {
        actions.playall = actions.play.clone();
        if !bp.search.as_deref().unwrap_or("").starts_with("sql=") && !track_ids.is_empty() {
            // "All Songs" plays the found tracks (LMS: `browselibrary playlist … search:sql=…`).
            let ids = track_ids.trim_end_matches(',').to_string();
            let p = obj(json!({ "track_id": ids }));
            let ia = Actions {
                all_defined: true,
                info: Some(ActionDef::none()),
                ..Default::default()
            }
            .with_playlistcontrol(&p);
            extras.push(Item {
                name: Some(s("ALL_SONGS")),
                icon: Some("html/images/albums.png".into()),
                kind: Some("playlist".into()),
                playlist_ref: true,
                skip_if_singleton: true,
                item_actions: Some(ia),
                ..Default::default()
            });
        }
    } else {
        let mut sort_tags = bp.tags.clone();
        sort_tags.push((
            "sort".into(),
            bp.sort.clone().unwrap_or_else(|| "albumtrack".into()),
        ));
        let p = tags_to_params(&sort_tags);
        actions.playall = Some(
            ActionDef::cmd(&["playlistcontrol"], with(p.clone(), "cmd", json!("load")))
                .vars(&[("play_index", "play_index")]),
        );
        actions.addall =
            Some(ActionDef::cmd(&["playlistcontrol"], with(p, "cmd", json!("add"))).vars(&[]));
    }
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras,
    }
}

/// `_bmf` (music folder).
pub fn bmf_query(bp: &BrowseParams) -> Vec<String> {
    let mut q = vec!["musicfolder".to_string(), "tags:cdus".to_string()];
    q.extend(tag_tokens(&bp.tags));
    q
}

pub fn bmf_result(results: &Obj) -> ModeResult {
    let items = loop_of(results, "folder_loop")
        .into_iter()
        .map(|f| {
            let mut it = cli_item(f);
            it.name = get_str(f, "filename");
            let id = get_str(f, "id").unwrap_or_default();
            match get_str(f, "type").as_deref() {
                Some("folder") => {
                    it.kind = Some("playlist".into());
                    it.lazy = true;
                    let p = obj(json!({ "folder_id": id }));
                    let ia = Actions {
                        info: Some(ActionDef::cmd(&["folderinfo", "items"], p.clone())),
                        ..Default::default()
                    }
                    .with_playlistcontrol(&p);
                    it.item_actions = Some(ia);
                    it.fields
                        .insert("_next".into(), json!({ "mode": "bmf", "folder_id": id }));
                }
                Some("track") => {
                    it.kind = Some("audio".into());
                    it.url = get_str(f, "url");
                    it.playall = true;
                    it.item_actions = Some(Actions {
                        info: Some(ActionDef::cmd(
                            &["trackinfo", "items"],
                            obj(json!({ "track_id": id })),
                        )),
                        ..Default::default()
                    });
                    if let Some(c) = get_str(f, "coverid") {
                        it.image = Some(format!("music/{}/cover", c));
                        it.artwork_track_id = Some(c);
                    }
                }
                Some("playlist")
                    if get_str(f, "url")
                        .map(|u| u.to_lowercase().ends_with(".cue"))
                        .unwrap_or(false) =>
                {
                    it.playlist_ref = true;
                    it.lazy = true;
                    it.fields.insert(
                        "_next".into(),
                        json!({ "mode": "playlistTracks", "playlist_id": id }),
                    );
                }
                Some("playlist") => {
                    it.kind = Some("audio".into());
                    it.url = get_str(f, "url").map(|u| {
                        if let Some(rest) = u.strip_prefix("file") {
                            format!("tmp{}", rest)
                        } else {
                            u
                        }
                    });
                    it.playall = true;
                    it.item_actions = Some(Actions {
                        info: Some(ActionDef::cmd(
                            &["trackinfo", "items"],
                            obj(json!({ "track_id": id })),
                        )),
                        ..Default::default()
                    });
                }
                _ => it.kind = Some("text".into()),
            }
            it
        })
        .collect();
    ModeResult {
        feed: Feed {
            items,
            ..Default::default()
        },
        extras: vec![],
    }
}

/// `_playlists`.
pub fn playlists_query(bp: &BrowseParams, library_id: Option<&str>) -> Vec<String> {
    let mut tags = bp.tags.clone();
    if bp.search.is_some() && tags.is_empty() {
        if let Some(l) = library_id {
            tags.push(("library_id".into(), l.into()));
        }
    }
    let mut q = vec!["playlists".to_string(), "tags:sux".to_string()];
    q.extend(tag_tokens(&tags));
    if let Some(s) = &bp.search {
        q.push(format!("search:{}", s));
    }
    q
}

pub fn playlists_result(bp: &BrowseParams, results: &Obj) -> ModeResult {
    let items = loop_of(results, "playlists_loop")
        .into_iter()
        .map(|pl| {
            let mut it = cli_item(pl);
            it.name = get_str(pl, "playlist");
            let id = get_str(pl, "id").unwrap_or_default();
            if id.starts_with("file:") {
                it.kind = Some("link".into());
                it.lazy = true;
                it.textkey = Some(" ".into());
                it.fields.insert(
                    "_next".into(),
                    json!({ "mode": "playlists", "folder_id": id }),
                );
            } else {
                it.kind = Some("playlist".into());
                it.playlist_ref = true;
                it.lazy = true;
                it.url = None;
                let p = obj(json!({ "playlist_id": id }));
                let ia = Actions {
                    info: Some(ActionDef::cmd(&["playlistinfo", "items"], p.clone())),
                    items: Some(ActionDef::cmd(
                        &[BROWSELIBRARY, "items"],
                        obj(json!({ "mode": "playlistTracks", "playlist_id": id })),
                    )),
                    ..Default::default()
                }
                .with_playlistcontrol(&p);
                it.item_actions = Some(ia);
                it.fields.insert(
                    "_next".into(),
                    next_level("playlistTracks", &bp.tags, &[("playlist_id", id)]),
                );
            }
            it
        })
        .collect();
    ModeResult {
        feed: Feed {
            items,
            ..Default::default()
        },
        extras: vec![],
    }
}

/// `_playlistTracks`.
pub fn playlist_tracks_query(bp: &BrowseParams) -> Vec<String> {
    let mut q = vec![
        "playlists".to_string(),
        "tracks".to_string(),
        "tags:dtuxgaliqykorfcJK".to_string(),
    ];
    q.push(
        bp.menu_style
            .clone()
            .map(|m| {
                if m.starts_with("menuStyle:") {
                    m
                } else {
                    format!("menuStyle:{}", m)
                }
            })
            .unwrap_or_else(|| "menuStyle:album".into()),
    );
    q.extend(tag_tokens(&bp.tags));
    q
}

pub fn playlist_tracks_result(bp: &BrowseParams, results: &Obj, offset: usize) -> ModeResult {
    let items = loop_of(results, "playlisttracks_loop")
        .into_iter()
        .enumerate()
        .map(|(i, t)| {
            let mut it = track_item(t, offset + i);
            it.name2 = Some(format!(
                "{} - {}",
                get_str(t, "artist").unwrap_or_default(),
                get_str(t, "album").unwrap_or_default()
            ));
            let remote_art = get_str(t, "remote").map(|r| r != "0").unwrap_or(false)
                && get_str(t, "artwork_url").is_some();
            if let Some(c) = get_str(t, "coverid").filter(|_| !remote_art) {
                it.artwork_track_id = Some(c);
            }
            it.image = it
                .artwork_track_id
                .clone()
                .map(|a| format!("music/{}/cover", a))
                .or_else(|| get_str(t, "artwork_url"));
            it
        })
        .collect();
    let p = tags_to_params(&bp.tags);
    let pc = |cmd: &str| ActionDef::cmd(&["playlistcontrol"], obj(json!({ "cmd": cmd })));
    let mut actions = Actions {
        all_defined: true,
        info: Some(ActionDef::cmd(&["trackinfo", "items"], Obj::new())),
        play: Some(pc("load")),
        add: Some(pc("add")),
        insert: Some(pc("insert")),
        playall: Some(
            ActionDef::cmd(&["playlistcontrol"], with(p.clone(), "cmd", json!("load")))
                .vars(&[("play_index", "play_index")]),
        ),
        addall: Some(ActionDef::cmd(&["playlistcontrol"], with(p, "cmd", json!("add"))).vars(&[])),
        ..Default::default()
    }
    .common(&[("track_id", "id"), ("url", "url")]);
    actions.items = actions.info.clone();
    let title = results.get("__playlistTitle").and_then(value_string);
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            title,
            ..Default::default()
        },
        extras: vec![],
    }
}

/// `_works`.
pub fn works_query(bp: &BrowseParams, library_id: Option<&str>) -> Vec<String> {
    let mut tags = bp.tags.clone();
    if let Some(l) = library_id {
        if !tags.iter().any(|(k, _)| k == "library_id") {
            tags.push(("library_id".into(), l.into()));
        }
    }
    if let Some(s) = &bp.search {
        if !tags.iter().any(|(k, _)| k == "from_search") {
            tags.push(("from_search".into(), s.clone()));
        }
    }
    let mut q = vec!["works".to_string()];
    q.extend(tag_tokens(&tags));
    if let Some(s) = &bp.search {
        q.push(format!("search:{}", s));
    }
    q
}

pub fn works_result(bp: &BrowseParams, results: &Obj) -> ModeResult {
    let items = loop_of(results, "works_loop")
        .into_iter()
        .map(|w| {
            let mut it = cli_item(w);
            it.name = get_str(w, "composer");
            it.name2 = get_str(w, "work");
            it.image = Some(
                get_str(w, "artwork_track_id")
                    .map(|a| format!("music/{}/cover", a))
                    .unwrap_or_else(|| "html/images/works.png".into()),
            );
            it.kind = Some("playlist".into());
            it.playlist_ref = true;
            it.lazy = true;
            let extra = [
                ("album_id", get_str(w, "album_id").unwrap_or_default()),
                ("work_id", get_str(w, "work_id").unwrap_or_default()),
                ("composer_id", get_str(w, "composer_id").unwrap_or_default()),
            ];
            it.fields
                .insert("_next".into(), next_level("albums", &bp.tags, &extra));
            it
        })
        .collect();
    let params = tags_to_params(&bp.tags);
    let actions = Actions {
        all_defined: true,
        items: Some(ActionDef::cmd(
            &[BROWSELIBRARY, "items"],
            with(params.clone(), "mode", json!("albums")),
        )),
        ..Default::default()
    }
    .with_playlistcontrol(&params)
    .common(&[
        ("work_id", "work_id"),
        ("composer_id", "composer_id"),
        ("album_id", "album_id"),
    ]);
    ModeResult {
        feed: Feed {
            items,
            actions: Some(actions),
            ..Default::default()
        },
        extras: vec![],
    }
}

/// `searchItems`: the search categories (type `search`, or `link` for global search).
pub fn search_items(kind: &str, search_param: Option<&str>) -> Vec<Item> {
    [
        ("BROWSE_BY_ARTIST", "artists", "ARTISTS"),
        ("BROWSE_BY_ALBUM", "albums", "ALBUMS"),
        ("BROWSE_BY_WORK", "works", "WORKS"),
        ("BROWSE_BY_SONG", "tracks", "SONGS"),
        ("PLAYLISTS", "playlists", "PLAYLISTS"),
    ]
    .iter()
    .map(|(name, mode, cache)| {
        let mut it = Item {
            name: Some(s(name)),
            kind: Some(kind.into()),
            icon: Some("html/images/search.png".into()),
            lazy: true,
            cachesearch: Some(cache.to_string()),
            ..Default::default()
        };
        it.fields.insert("_next".into(), json!({ "mode": mode }));
        if let Some(sp) = search_param {
            it.fields.insert("searchParam".into(), json!(sp));
        }
        it
    })
    .collect()
}

/// `_generic`'s `requestDone`: add extras, `EMPTY` placeholder, totals.
pub fn finish(
    mut m: ModeResult,
    total: usize,
    index: usize,
    quantity: usize,
    searching: bool,
) -> Feed {
    let mut extras = m.extras;
    if total < 2 {
        extras.retain(|e| !e.skip_if_singleton);
    }
    let n = extras.len();
    m.feed.offset = index;
    let n_results = m.feed.items.len();
    if quantity > 0 && index > 0 && n_results == 0 {
        let used = index.saturating_sub(total).min(n);
        m.feed.items.extend(extras.into_iter().skip(used));
    } else if quantity > 0 && n_results < quantity {
        let space = (quantity - n_results).min(n);
        m.feed.items.extend(extras.into_iter().take(space));
    } else {
        m.feed.items.extend(extras);
    }
    m.feed.total = Some(total + n);
    if !searching && m.feed.items.is_empty() {
        m.feed.items = vec![Item {
            kind: Some("text".into()),
            title: Some(s("EMPTY")),
            ..Default::default()
        }];
        m.feed.total = Some(1);
        m.feed.offset = 0;
    }
    m.feed
}

// ---- running a level ------------------------------------------------------------------------

/// Library id for the request's player (`getLibraryIdForClient`).
fn library_id_for(server: &Server, r: &Request) -> Option<String> {
    r.client
        .and_then(|c| server.player(&c))
        .and_then(|p| p.prefs.get("libraryId"))
        .and_then(|v| value_string(&v))
        .filter(|v| !v.is_empty())
        .or_else(|| Some(server.prefs.server.get_str("libraryId")).filter(|v| !v.is_empty()))
}

/// Run one mode for `bp` and return its level (`$browseLibraryModeMap{mode}`).
async fn run_mode(
    server: &Arc<Server>,
    r: &Request,
    bp: &BrowseParams,
    index: usize,
    quantity: usize,
) -> Feed {
    let library_id = library_id_for(server, r);
    let lib = library_id.as_deref();
    let prefs = &server.prefs.server;
    let only_album_years = prefs.get_bool("onlyAlbumYears");
    let words = match bp.mode.as_str() {
        "artists" => artists_query(bp, lib),
        "genres" => genres_query(bp, lib),
        "years" => years_query(bp, lib, only_album_years),
        "albums" | "vaalbums" | "randomalbums" => albums_query(bp, lib),
        "tracks" => match tracks_query(bp, lib) {
            Some(q) => q,
            None => {
                return Feed {
                    title: Some(
                        "Invalid request: no search term or album/artist/genre tags".into(),
                    ),
                    ..Default::default()
                }
            }
        },
        "bmf" | "filesystem" => bmf_query(bp),
        "playlists" => playlists_query(bp, lib),
        "playlistTracks" => playlist_tracks_query(bp),
        "works" => works_query(bp, lib),
        "search" => {
            return Feed {
                name: Some(s("SEARCH")),
                items: search_items("search", None),
                ..Default::default()
            };
        }
        _ => return Feed::default(),
    };
    let mut tokens = vec![words[0].clone()];
    let mut rest = &words[1..];
    if words[0] == "playlists" && words.get(1).map(String::as_str) == Some("tracks") {
        tokens.push("tracks".into());
        rest = &words[2..];
    }
    tokens.push(index.to_string());
    tokens.push(quantity.to_string());
    tokens.extend(rest.iter().cloned());
    let out = super::exec(server, r, tokens).await;
    let results = &out.results;
    let total = results.get("count").and_then(value_i64).unwrap_or(0).max(0) as usize;
    let m = match bp.mode.as_str() {
        "artists" => artists_result(bp, results),
        "genres" => genres_result(bp, results, prefs.get_bool("useUnifiedArtistsList")),
        "years" => years_result(bp, results),
        "albums" | "vaalbums" | "randomalbums" => albums_result(bp, results, only_album_years),
        "tracks" => tracks_result(bp, results, index),
        "bmf" | "filesystem" => bmf_result(results),
        "playlists" => playlists_result(bp, results),
        "playlistTracks" => playlist_tracks_result(bp, results, index),
        "works" => works_result(bp, results),
        _ => return Feed::default(),
    };
    finish(m, total, index, quantity, bp.search.is_some())
}

/// Resolve the level an `item_id` path leads to, starting from `bp`.
/// `final_range` is the (index, quantity) to fetch for the last level.
#[allow(clippy::too_many_arguments)]
async fn resolve(
    server: &Arc<Server>,
    r: &Request,
    ctx: &mut Ctx,
    mut bp: BrowseParams,
    path: &[String],
    is_item_query: bool,
    final_range: (usize, usize),
    base_crumbs: Vec<String>,
) -> Level {
    let mut crumbs = base_crumbs;
    for (depth, comp) in path.iter().enumerate() {
        let Some(i) = component_index(comp) else {
            break;
        };
        let level = run_mode(server, r, &bp, i, 1).await;
        let Some(item) = i
            .checked_sub(level.offset)
            .and_then(|j| level.items.get(j))
            .cloned()
        else {
            return Level::List(Feed::default());
        };
        let last = depth + 1 == path.len();
        let mut comp = comp.clone();
        if item.kind.as_deref() == Some("search") || item.fields.contains_key("searchParam") {
            let search = item
                .fields
                .get("searchParam")
                .and_then(value_string)
                .or_else(|| component_search(&comp))
                .or(ctx.search.clone());
            if let Some(s) = &search {
                if item.kind.as_deref() == Some("search") {
                    comp = format!("{}_{}", i, escape_search(s));
                }
            }
            ctx.search = search.clone();
            let next = item
                .fields
                .get("_next")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut nbp = BrowseParams::from_obj(&next);
            nbp.search = search;
            nbp.order_by = bp.order_by.clone();
            bp = nbp;
            crumbs.push(comp);
            continue;
        }
        crumbs.push(comp);
        let leaf = item.kind.as_deref() == Some("audio") || item.kind.as_deref() == Some("text");
        if last && (leaf || (is_item_query && item.fields.get("_next").is_none())) {
            ctx.crumbs = crumbs;
            return Level::Leaf(Box::new(item), level);
        }
        match item.fields.get("_next").and_then(Value::as_object) {
            Some(next) => {
                let mut nbp = BrowseParams::from_obj(next);
                nbp.order_by = bp.order_by.clone();
                bp = nbp;
            }
            None => {
                ctx.crumbs = crumbs;
                return Level::Leaf(Box::new(item), level);
            }
        }
    }
    ctx.crumbs = crumbs;
    let mut feed = run_mode(server, r, &bp, final_range.0, final_range.1).await;
    if bp.search.is_some() && bp.mode != "search" {
        feed.kind = Some("search".into());
    }
    ctx.top_query.get_or_insert_with(|| bp.entry_query());
    Level::List(feed)
}

/// `browselibrary items|playlist`.
pub async fn browselibrary(server: Arc<Server>, mut r: Request) -> Request {
    super::fix_index_quantity(&mut r);
    let mut ctx = Ctx::from_request(&server, &r, BROWSELIBRARY);
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let mut bp = BrowseParams::from_map(|k| r.param(k).map(str::to_string));
    if bp.mode.is_empty() {
        r.bad_params();
        return r;
    }
    if bp.mode == "randomalbums" {
        bp.sort = Some("random".into());
    }
    if ctx.menu_mode && bp.order_by.is_none() {
        bp.order_by = Some(super::settings::album_sort(&server));
    }
    if bp.tags.iter().all(|(k, _)| k != "library_id") {
        if let Some(l) = library_id_for(&server, &r) {
            bp.tags.push(("library_id".into(), l));
        }
    }
    ctx.top_query = Some(bp.entry_query());
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    let is_search_mode = bp.mode == "search";
    let final_range = if play.is_some() {
        (0, ALL)
    } else if let Some(pc) = ctx.play_control {
        (pc, 1)
    } else {
        (ctx.index, ctx.quantity)
    };
    // In search mode the search term arrives with the item id of the category.
    if is_search_mode {
        bp.search = None;
    }
    let level = resolve(
        &server,
        &r,
        &mut ctx,
        bp.clone(),
        &path,
        play.is_none(),
        final_range,
        Vec::new(),
    )
    .await;
    // the feed `query` is the top-level entry params
    ctx.top_query = Some(bp.entry_query());
    match play {
        Some((method, pi)) => xmlbrowser::play(&server, &mut r, level, &method, pi).await,
        None => {
            let cache = if let Level::List(f) = &level {
                f.kind.as_deref() == Some("search")
            } else {
                false
            };
            xmlbrowser::render(&ctx, level, &mut r);
            if cache && ctx.cachesearch_param {
                cache_search(&server, &r, &ctx, BROWSELIBRARY);
            }
        }
    }
    r
}

/// Remember a search for "Recent Searches" (`_cliQuery_done` → `cacheSearch`).
fn cache_search(server: &Arc<Server>, r: &Request, ctx: &Ctx, query: &str) {
    let Some(search) = ctx.search.clone().filter(|s| !s.is_empty()) else {
        return;
    };
    let key = r.param("cachesearch").filter(|k| k.len() > 1).map(|k| {
        if k.to_uppercase() == k {
            s(k)
        } else {
            k.to_string()
        }
    });
    let title = match key {
        Some(k) => format!("{}: {}", k, search),
        None => search.clone(),
    };
    let mut params = obj(json!({ "item_id": ctx.item_id, "menu": query, "search": search }));
    if let Some(q) = &ctx.top_query {
        for (k, v) in q {
            if k != "search" {
                params.insert(k.clone(), v.clone());
            }
        }
    }
    let item = json!({ "text": title, "actions": { "go": { "player": 0, "cmd": [query, "items"], "params": params } } });
    super::cache_search(server, super::player(server, r).as_ref(), item);
}

/// `globalsearch items search:<text>` (`Slim::Menu::GlobalSearch`): "My Music" with the
/// library search categories as links.
pub async fn globalsearch(server: Arc<Server>, mut r: Request) -> Request {
    super::fix_index_quantity(&mut r);
    let mut ctx = Ctx::from_request(&server, &r, "globalsearch");
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let mut path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    let mut search = ctx.search.clone().filter(|s| !s.is_empty());
    if let Some(first) = path.first().cloned() {
        if let Some(rest) = first.strip_prefix('_') {
            search = Some(
                percent_encoding::percent_decode_str(rest)
                    .decode_utf8_lossy()
                    .to_string(),
            );
            path.remove(0);
        }
    }
    let Some(search) = search else {
        r.bad_params();
        return r;
    };
    ctx.search = Some(search.clone());
    let crumb = format!("_{}", escape_search(&search));
    let my_music = Item {
        name: Some(s("MY_MUSIC")),
        kind: Some("opml".into()),
        items: Some(search_items("link", Some(&search))),
        ..Default::default()
    };
    let top = Feed {
        name: Some(s("GLOBAL_SEARCH_IN").replace("%s", &search)),
        items: vec![my_music],
        menu_complete: true,
        ..Default::default()
    };
    let level = if path.len() < 2 {
        ctx.crumbs = std::iter::once(crumb.clone())
            .chain(path.iter().cloned())
            .collect();
        xmlbrowser::descend(top, &path, play.is_none()).unwrap_or(Level::List(Feed::default()))
    } else {
        // path[0] = My Music, path[1] = category, rest = inside the library results
        let cat = component_index(&path[1]).unwrap_or(0);
        let item = search_items("link", Some(&search)).into_iter().nth(cat);
        match item {
            Some(it) => {
                let next = it
                    .fields
                    .get("_next")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let mut bp = BrowseParams::from_obj(&next);
                bp.search = Some(search.clone());
                if ctx.menu_mode {
                    bp.order_by = Some(super::settings::album_sort(&server));
                }
                ctx.top_query = Some(bp.entry_query());
                let final_range = if play.is_some() {
                    (0, ALL)
                } else if let Some(pc) = ctx.play_control {
                    (pc, 1)
                } else {
                    (ctx.index, ctx.quantity)
                };
                let base = vec![crumb, path[0].clone(), path[1].clone()];
                resolve(
                    &server,
                    &r,
                    &mut ctx,
                    bp,
                    &path[2..],
                    play.is_none(),
                    final_range,
                    base,
                )
                .await
            }
            None => Level::List(Feed::default()),
        }
    };
    match play {
        Some((method, pi)) => xmlbrowser::play(&server, &mut r, level, &method, pi).await,
        None => xmlbrowser::render(&ctx, level, &mut r),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::jive::xmlbrowser::{render, results_loop};

    fn bp(pairs: &[(&str, &str)]) -> BrowseParams {
        let m: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        BrowseParams::from_map(|k| m.get(k).cloned())
    }

    fn ctx() -> Ctx {
        Ctx {
            query: "browselibrary".into(),
            terms: vec!["browselibrary".into(), "items".into()],
            index: 0,
            quantity: 512,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        }
    }

    #[test]
    fn artists_query_and_items() {
        let p = bp(&[("mode", "artists"), ("role_id", "ALBUMARTIST")]);
        assert_eq!(
            artists_query(&p, None),
            vec![
                "artists",
                "role_id:ALBUMARTIST",
                "include_online_only_artists:1",
                "tags:Es4"
            ]
        );
        let g = bp(&[("mode", "artists"), ("genre_id", "3")]);
        assert_eq!(
            artists_query(&g, None),
            vec!["artists", "genre_id:3", "tags:s4"]
        );
        let results = obj(json!({ "count": 2, "artists_loop": [
            { "id": 5, "artist": "ABBA", "textkey": "A" },
            { "id": 6, "artist": "Beatles", "textkey": "B", "portraitid": "ff00" },
        ] }));
        let feed = finish(artists_result(&p, &results), 2, 0, 512, false);
        assert_eq!(feed.items.len(), 2);
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "ABBA");
        assert_eq!(items[0]["textkey"], "A");
        assert_eq!(items[0]["commonParams"], json!({ "artist_id": 5 }));
        assert_eq!(items[0]["icon-id"], "html/images/artists.png");
        assert_eq!(items[1]["icon"], "contributor/ff00/image");
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["go"]["params"],
            json!({ "role_id": "ALBUMARTIST", "mode": "albums", "menu": 1 })
        );
        assert_eq!(
            base["actions"]["more"]["cmd"],
            json!(["artistinfo", "items"])
        );
        assert_eq!(base["actions"]["play"]["params"]["cmd"], "load");
        // _next leads to the artist's albums
        let next = feed_next(&artists_result(&p, &results).feed.items[0]);
        assert_eq!(next.mode, "albums");
        assert_eq!(next.tag("artist_id"), Some("5"));
    }

    fn feed_next(it: &Item) -> BrowseParams {
        BrowseParams::from_obj(it.fields.get("_next").and_then(Value::as_object).unwrap())
    }

    #[test]
    fn genre_artists_get_all_albums_extra() {
        let p = bp(&[("mode", "artists"), ("genre_id", "3")]);
        let results = obj(
            json!({ "count": 2, "artists_loop": [{ "id": 1, "artist": "A" }, { "id": 2, "artist": "B" }] }),
        );
        let feed = finish(artists_result(&p, &results), 2, 0, 512, false);
        assert_eq!(feed.items.len(), 3);
        assert_eq!(feed.total, Some(3));
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[2]["text"], "All Albums");
        assert_eq!(items[2]["actions"]["more"], "none");
        assert_eq!(items[2]["actions"]["go"]["params"]["mode"], "albums");
        assert_eq!(items[2]["actions"]["go"]["params"]["genre_id"], "3");
        assert!(items[2].get("commonParams").is_none());
        // singleton: the extra is skipped
        let one = obj(json!({ "count": 1, "artists_loop": [{ "id": 1, "artist": "A" }] }));
        assert_eq!(
            finish(artists_result(&p, &one), 1, 0, 512, false)
                .items
                .len(),
            1
        );
    }

    #[test]
    fn albums_of_artist() {
        let p = bp(&[
            ("mode", "albums"),
            ("artist_id", "12"),
            ("orderBy", "artflow"),
        ]);
        assert_eq!(albums_sort(&p).as_deref(), Some("yearalbum"));
        assert_eq!(
            albums_query(&p, None),
            vec![
                "albums",
                "artist_id:12",
                "sort:yearalbum",
                "tags:ljsaaSSKEwy"
            ]
        );
        let results = obj(json!({ "count": 2, "albums_loop": [
            { "id": 7, "album": "Abbey Road", "artist": "The Beatles", "artist_id": 12, "artwork_track_id": "c0ffee", "year": 1969, "performance": "", "textkey": "A" },
            { "id": 8, "album": "Help", "artist": "Various", "artist_id": 99, "artists": "The Beatles,Others", "artist_ids": "12,13", "performance": "" },
        ] }));
        let feed = finish(albums_result(&p, &results, false), 2, 0, 512, false);
        assert_eq!(feed.items.len(), 3, "albums + All Songs");
        assert!(feed.items[0].name2.is_none());
        assert_eq!(feed.items[1].name2.as_deref(), Some("The Beatles, Others"));
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["icon"], "music/c0ffee/cover");
        assert_eq!(
            items[0]["commonParams"],
            json!({ "album_id": 7, "performance": "" })
        );
        assert_eq!(items[2]["text"], "All Songs");
        assert_eq!(items[2]["actions"]["go"]["params"]["performance"], "-1");
        assert_eq!(
            items[2]["actions"]["play"]["params"],
            json!({ "cmd": "load", "artist_id": "12", "menu": 1 })
        );
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["go"]["params"],
            json!({ "artist_id": "12", "mode": "tracks", "menu": 1 })
        );
        assert_eq!(
            base["actions"]["more"]["params"],
            json!({ "artist_id": "12", "menu": 1 })
        );
        assert_eq!(r.result("window").unwrap()["windowStyle"], "icon_list");
    }

    #[test]
    fn album_tracks_play_all() {
        let p = bp(&[("mode", "tracks"), ("album_id", "7")]);
        let q = tracks_query(&p, None).unwrap();
        assert_eq!(
            q,
            vec![
                "titles",
                "tags:dtuxgaAsSliqyorf",
                "sort:albumtrack",
                "menuStyle:album",
                "album_id:7"
            ]
        );
        assert!(tracks_query(&bp(&[("mode", "tracks")]), None).is_none());
        let results = obj(json!({ "count": 2, "titles_loop": [
            { "id": 101, "title": "Come Together", "tracknum": 1, "duration": 259.5, "url": "file:///m/1.flac", "artist": "The Beatles", "album": "Abbey Road" },
            { "id": 102, "title": "Something", "tracknum": 2, "duration": 182, "url": "file:///m/2.flac" },
        ] }));
        let feed = finish(tracks_result(&p, &results, 0), 2, 0, 512, false);
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Come Together");
        assert_eq!(items[0]["goAction"], "play");
        assert_eq!(items[0]["style"], "itemplay");
        assert_eq!(items[0]["commonParams"], json!({ "track_id": 101 }));
        assert_eq!(items[1]["playallParams"], json!({ "play_index": 1 }));
        assert!(items[0].get("params").is_none());
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["play"]["params"],
            json!({ "cmd": "load", "album_id": "7", "sort": "albumtrack", "menu": 1 })
        );
        assert_eq!(base["actions"]["play"]["itemsParams"], "playallParams");
        assert_eq!(base["actions"]["add"]["itemsParams"], "addallParams");
        assert_eq!(
            base["actions"]["more"]["cmd"],
            json!(["trackinfo", "items"])
        );
        assert_eq!(
            base["actions"]["more"]["params"],
            json!({ "album_id": "7", "menu": 1 })
        );
        // all items are touch-to-play: go = play
        assert_eq!(base["actions"]["go"], base["actions"]["play"]);
        assert_eq!(r.result("window").unwrap()["windowStyle"], "text_list");
    }

    #[test]
    fn genre_and_year_lists() {
        let g = genres_result(
            &bp(&[("mode", "genres")]),
            &obj(
                json!({ "count": 1, "genres_loop": [{ "id": 3, "genre": "Rock", "textkey": "R" }] }),
            ),
            false,
        );
        assert_eq!(g.feed.items[0].name.as_deref(), Some("Rock"));
        let next = feed_next(&g.feed.items[0]);
        assert_eq!(next.mode, "artists");
        assert_eq!(next.tag("genre_id"), Some("3"));
        assert_eq!(next.tag("role_id"), Some("ALBUMARTIST"));
        let y = years_result(
            &bp(&[("mode", "years")]),
            &obj(json!({ "count": 1, "years_loop": [{ "year": 1999 }] })),
        );
        let mut r = Request::blank();
        render(&ctx(), Level::List(finish(y, 1, 0, 512, false)), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "1999");
        assert_eq!(items[0]["commonParams"], json!({ "year": "1999" }));
        assert_eq!(
            r.result("base").unwrap()["actions"]["more"]["cmd"],
            json!(["yearinfo", "items"])
        );
    }

    #[test]
    fn music_folder_items() {
        let results = obj(json!({ "count": 3, "folder_loop": [
            { "id": 10, "filename": "Beatles", "type": "folder" },
            { "id": 11, "filename": "01.flac", "type": "track", "url": "file:///m/01.flac", "coverid": "aa" },
            { "id": 12, "filename": "notes.txt", "type": "unknown" },
        ] }));
        let feed = finish(bmf_result(&results), 3, 0, 512, false);
        let mut c = ctx();
        c.top_query = Some(bp(&[("mode", "bmf")]).entry_query());
        let mut r = Request::blank();
        render(&c, Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(
            items[0]["actions"]["more"]["cmd"],
            json!(["folderinfo", "items"])
        );
        assert_eq!(
            items[0]["actions"]["play"]["params"],
            json!({ "cmd": "load", "folder_id": "10", "menu": 1 })
        );
        // folders have no item `go`: the base go (browselibrary items) applies with the item params
        assert!(items[0]["actions"].get("go").is_none());
        assert_eq!(
            items[0]["params"],
            json!({ "item_id": "0", "isContextMenu": 1 })
        );
        assert_eq!(items[1]["goAction"], "play");
        assert_eq!(items[1]["params"]["touchToPlay"], "1");
        assert_eq!(items[1]["actions"]["more"]["params"]["track_id"], "11");
        assert_eq!(items[2]["style"], "itemNoAction");
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["play"]["cmd"],
            json!(["browselibrary", "playlist", "play"])
        );
        assert_eq!(base["actions"]["play"]["params"]["mode"], "bmf");
    }

    #[test]
    fn playlists_and_tracks() {
        let results = obj(
            json!({ "count": 1, "playlists_loop": [{ "id": 40, "playlist": "Party", "url": "file:///p/party.m3u", "textkey": "P" }] }),
        );
        let feed = finish(
            playlists_result(&bp(&[("mode", "playlists")]), &results),
            1,
            0,
            512,
            false,
        );
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(
            items[0]["actions"]["go"]["params"],
            json!({ "mode": "playlistTracks", "playlist_id": "40", "menu": 1 })
        );
        assert_eq!(
            items[0]["actions"]["more"]["cmd"],
            json!(["playlistinfo", "items"])
        );
        let tracks = obj(
            json!({ "count": 1, "__playlistTitle": "Party", "playlisttracks_loop": [{ "id": 5, "title": "T", "artist": "A", "album": "B", "coverid": "cc", "url": "file:///x" }] }),
        );
        let p = bp(&[("mode", "playlistTracks"), ("playlist_id", "40")]);
        let feed = finish(playlist_tracks_result(&p, &tracks, 0), 1, 0, 512, false);
        let mut r = Request::blank();
        render(&ctx(), Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "T\nA - B");
        assert_eq!(
            items[0]["commonParams"],
            json!({ "track_id": 5, "url": "file:///x" })
        );
        assert_eq!(r.result("title"), Some(&json!("Party")));
        assert_eq!(
            r.result("base").unwrap()["actions"]["play"]["params"],
            json!({ "cmd": "load", "playlist_id": "40", "menu": 1 })
        );
    }

    #[test]
    fn search_menu_items() {
        let mut c = ctx();
        c.top_query = Some(bp(&[("mode", "search")]).entry_query());
        let feed = Feed {
            name: Some("Search".into()),
            items: search_items("search", None),
            ..Default::default()
        };
        let mut r = Request::blank();
        render(&c, Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items.len(), 5);
        assert_eq!(items[0]["text"], "Artists");
        assert_eq!(
            items[0]["actions"]["go"]["params"],
            json!({ "menu": "browselibrary", "mode": "search", "item_id": "0", "search": "__TAGGEDINPUT__", "cachesearch": "ARTISTS" })
        );
        assert_eq!(items[3]["input"]["len"], 1);
    }

    #[test]
    fn extras_paging() {
        let p = bp(&[("mode", "artists"), ("genre_id", "3")]);
        let results = obj(json!({ "count": 3, "artists_loop": [] }));
        // second page past the real items: only the extra
        let feed = finish(artists_result(&p, &results), 3, 3, 10, false);
        assert_eq!(feed.items.len(), 1);
        assert_eq!(feed.total, Some(4));
        // empty, non-search list gets the EMPTY placeholder
        let feed = finish(
            artists_result(&bp(&[("mode", "artists")]), &obj(json!({ "count": 0 }))),
            0,
            0,
            10,
            false,
        );
        assert_eq!(feed.items[0].title.as_deref(), Some("Empty"));
    }

    #[test]
    fn artist_name_split() {
        assert_eq!(split_artists("A,B"), vec!["A", "B"]);
        assert_eq!(
            split_artists("Crosby, Stills,Nash"),
            vec!["Crosby, Stills", "Nash"]
        );
    }
}
