//! Port of `Slim::Control::XMLBrowser`: turns an OPML-style feed (a [`Feed`] of [`Item`]s)
//! into the `<query> items` response (menu mode `item_loop` with `base` actions, or the
//! non-menu `loop_loop`) and executes `<query> playlist <method> item_id:…` commands.
//!
//! Only feeds that are fully resolved by the caller are handled here: callers (BrowseLibrary,
//! the *Info menus, favorites, radio) regenerate their feed for every request and descend
//! along `item_id` themselves (see [`descend`] for static trees), so no session cache is
//! needed.  Item ids therefore never carry the 8-hex-digit session id LMS prepends; incoming
//! ids that do carry one (e.g. the `ffffffff.` crumb of play-control menus) are accepted.

#[cfg(test)]
use crate::control::request::Results;
use crate::control::request::{normalize, Request};
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub type Obj = Map<String, Value>;

/// Convert a `json!({...})` literal into a map.
pub fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Obj::new(),
    }
}

/// One BrowseLibrary-style action (`{ command, fixedParams, variables }`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionDef {
    /// Empty command = the action is explicitly disabled (`'none'` on the wire).
    pub command: Vec<String>,
    pub fixed: Obj,
    /// `variables`: `None` = not given (the feed's `commonVariables` apply),
    /// `Some(vec![])` = explicitly no variables (`variables => []`).
    pub variables: Option<Vec<(String, String)>>,
}

impl ActionDef {
    pub fn cmd(command: &[&str], fixed: Obj) -> ActionDef {
        ActionDef {
            command: command.iter().map(|s| s.to_string()).collect(),
            fixed,
            variables: None,
        }
    }

    /// `{ command => [] }`: suppresses the action.
    pub fn none() -> ActionDef {
        ActionDef::default()
    }

    pub fn vars(mut self, vars: &[(&str, &str)]) -> ActionDef {
        self.variables = Some(
            vars.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        );
        self
    }
}

/// A feed's or item's `actions`/`itemActions` hash.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Actions {
    pub all_defined: bool,
    /// `commonVariables => [param => itemField, …]`.
    pub common_variables: Vec<(String, String)>,
    pub info: Option<ActionDef>,
    pub items: Option<ActionDef>,
    pub play: Option<ActionDef>,
    pub add: Option<ActionDef>,
    pub insert: Option<ActionDef>,
    pub playall: Option<ActionDef>,
    pub addall: Option<ActionDef>,
}

impl Actions {
    pub fn get(&self, name: &str) -> Option<&ActionDef> {
        match name {
            "info" => self.info.as_ref(),
            "items" => self.items.as_ref(),
            "play" => self.play.as_ref(),
            "add" => self.add.as_ref(),
            "insert" => self.insert.as_ref(),
            "playall" => self.playall.as_ref(),
            "addall" => self.addall.as_ref(),
            _ => None,
        }
    }

    pub fn common(mut self, vars: &[(&str, &str)]) -> Actions {
        self.common_variables = vars
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        self
    }

    /// The usual `play/add/insert => playlistcontrol cmd:load|add|insert %params` trio with
    /// `playall = play`, `addall = add` (BrowseLibrary, *Info menus).
    pub fn with_playlistcontrol(mut self, params: &Obj) -> Actions {
        let pc = |cmd: &str| {
            let mut p = obj(json!({ "cmd": cmd }));
            p.extend(params.clone());
            ActionDef::cmd(&["playlistcontrol"], p)
        };
        self.play = Some(pc("load"));
        self.add = Some(pc("add"));
        self.insert = Some(pc("insert"));
        self.playall = self.play.clone();
        self.addall = self.add.clone();
        self
    }
}

/// An OPML item as BrowseLibrary / the menus build it.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub name: Option<String>,
    pub title: Option<String>,
    pub name2: Option<String>,
    /// String token prefixed as `"<Label>: <name>"`.
    pub label: Option<String>,
    /// `type`: text, textarea, audio, playlist, link, search, opml, …
    pub kind: Option<String>,
    /// A plain (string) URL: the stream/track to play, or a favorites URL.
    pub url: Option<String>,
    /// The item's `url` is a code reference (a browsable level built on demand).
    pub lazy: bool,
    pub play: Option<String>,
    /// The item has a `playlist` (code reference) attribute.
    pub playlist_ref: bool,
    pub icon: Option<String>,
    pub image: Option<String>,
    pub artwork_track_id: Option<String>,
    pub textkey: Option<String>,
    pub playall: bool,
    pub playcontrol: Option<String>,
    pub item_actions: Option<Actions>,
    pub jive: Option<Obj>,
    /// Static children (`items`).
    pub items: Option<Vec<Item>>,
    pub next_window: Option<String>,
    pub wrap: bool,
    pub is_context_menu: bool,
    pub weblink: Option<String>,
    pub parse_urls: bool,
    pub cachesearch: Option<String>,
    pub skip_if_singleton: bool,
    pub style: Option<String>,
    pub menu_complete: bool,
    /// Any other attribute (`id`, `performance`, `play_index`, `favorites_url`,
    /// `description`, `bitrate`, …), used for action variables and leaf info lines.
    pub fields: Obj,
}

impl Item {
    pub fn text(name: impl Into<String>) -> Item {
        Item {
            name: Some(name.into()),
            kind: Some("text".into()),
            ..Default::default()
        }
    }

    pub fn field(&self, key: &str) -> Option<Value> {
        match key {
            "name" => self.name.clone().map(Value::String),
            "url" => self.url.clone().map(Value::String),
            "title" => self.title.clone().map(Value::String),
            _ => self.fields.get(key).cloned(),
        }
    }

    fn field_str(&self, key: &str) -> Option<String> {
        self.fields
            .get(key)
            .and_then(value_string)
            .filter(|s| !s.is_empty())
    }

    fn is_type(&self, t: &str) -> bool {
        self.kind.as_deref() == Some(t)
    }

    /// `hasAudio`.
    pub fn has_audio(&self) -> bool {
        if self.play.is_some() {
            return true;
        }
        if self.is_type("audio") || self.is_type("playlist") {
            return self.playlist_ref
                || self.url.is_some()
                || self.lazy
                || self.items.as_ref().map(|i| !i.is_empty()).unwrap_or(false);
        }
        false
    }

    /// `touchToPlay`.
    pub fn touch_to_play(&self) -> bool {
        self.is_type("audio") || self.field_str("on_select").as_deref() == Some("play")
    }

    fn is_playable(&self) -> bool {
        self.play.is_some()
            || self.playlist_ref
            || self.is_type("audio")
            || self.is_type("playlist")
    }

    /// `_favoritesParams`.
    pub fn favorites_params(&self) -> Option<Obj> {
        let url = self
            .field_str("favorites_url")
            .or_else(|| self.play.clone())
            .or_else(|| self.url.clone())?;
        let title = self
            .field_str("favorites_title")
            .or_else(|| self.title.clone())
            .or_else(|| self.name.clone())
            .filter(|t| !t.is_empty())?;
        let kind = self.field_str("favorites_type").unwrap_or_else(|| {
            if self.play.is_some() {
                "audio".into()
            } else {
                self.kind.clone().unwrap_or_else(|| "audio".into())
            }
        });
        let mut p =
            obj(json!({ "favorites_url": url, "favorites_title": title, "favorites_type": kind }));
        if let Some(parser) = self.field_str("parser") {
            p.insert("parser".into(), json!(parser));
        }
        if let Some(icon) = self
            .field_str("favorites_icon")
            .or_else(|| self.image.clone())
            .or_else(|| self.icon.clone())
            .or_else(|| self.field_str("cover"))
        {
            p.insert("icon".into(), json!(icon));
        }
        Some(p)
    }
}

/// A list level: the items to show plus feed-level attributes.
#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub title: Option<String>,
    pub name: Option<String>,
    pub name2: Option<String>,
    pub items: Vec<Item>,
    /// Absolute index of `items[0]` (feeds may hold just one page).
    pub offset: usize,
    /// Total number of items (defaults to `items.len()`).
    pub total: Option<usize>,
    pub actions: Option<Actions>,
    pub menu_complete: bool,
    pub window_id: Option<String>,
    /// `type` of this level (`search` for a search result level).
    pub kind: Option<String>,
    /// The item that led to this level (used as `$subFeed` for play-control menus).
    pub parent: Option<Box<Item>>,
}

impl Feed {
    #[cfg(test)]
    pub fn new(items: Vec<Item>) -> Feed {
        Feed {
            items,
            ..Default::default()
        }
    }

    fn count(&self) -> usize {
        self.total.unwrap_or(self.items.len())
    }
}

/// Where an `item_id` leads.
#[derive(Debug)]
pub enum Level {
    List(Feed),
    /// A leaf (audio/text) item and the level containing it.
    Leaf(Box<Item>, Feed),
}

/// Strip a leading session id (`getSID`) from an item id and split it into components.
pub fn split_item_id(item_id: &str) -> Vec<String> {
    let mut parts: Vec<String> = item_id
        .split('.')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if let Some(first) = parts.first() {
        if first.len() >= 8
            && first[..8].chars().all(|c| c.is_ascii_hexdigit())
            && first.chars().any(|c| c.is_ascii_alphabetic())
        {
            parts.remove(0);
        }
    }
    parts
}

/// Leading digits of an item id component (`"3_foo"` → 3).
pub fn component_index(c: &str) -> Option<usize> {
    let digits: String = c.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Search term embedded in an item id component (`"3_foo%20bar"` → `foo bar`).
pub fn component_search(c: &str) -> Option<String> {
    let (_, rest) = c.split_once('_')?;
    Some(
        percent_encoding::percent_decode_str(rest)
            .decode_utf8_lossy()
            .to_string(),
    )
}

/// `uri_escape_utf8($s, "^A-Za-z0-9")`.
pub fn escape_search(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Descend a static feed tree along `path` (`_cliQuery_done`'s item walk).
/// Returns `None` when a component does not exist.
pub fn descend(feed: Feed, path: &[String], is_item_query: bool) -> Option<Level> {
    let mut cur = feed;
    for (depth, comp) in path.iter().enumerate() {
        let i = component_index(comp)?;
        let item = cur.items.get(i.checked_sub(cur.offset)?)?.clone();
        let last = depth + 1 == path.len();
        if last
            && is_item_query
            && item.items.is_none()
            && (item.is_type("audio") || item.fields.contains_key("description"))
        {
            return Some(Level::Leaf(Box::new(item), cur));
        }
        match &item.items {
            Some(children) => {
                cur = Feed {
                    title: item.title.clone().or(item.name.clone()),
                    name2: item.name2.clone(),
                    items: children.clone(),
                    menu_complete: item.menu_complete,
                    parent: Some(Box::new(item.clone())),
                    ..Default::default()
                };
            }
            None if last => return Some(Level::Leaf(Box::new(item), cur)),
            None => return None,
        }
    }
    Some(Level::List(cur))
}

/// Everything the renderer needs from the request.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    /// The query verb (`browselibrary`, `favorites`, `trackinfo`, …).
    pub query: String,
    /// Request verbs (for the `playControl` base action).
    pub terms: Vec<String>,
    /// Copy of all request params.
    pub params: Obj,
    pub index: usize,
    pub quantity: usize,
    pub menu_mode: bool,
    pub interim_cm: bool,
    pub play_control: Option<usize>,
    pub item_id: Option<String>,
    /// Item id components leading to the rendered level.
    pub crumbs: Vec<String>,
    pub search: Option<String>,
    pub want_url: bool,
    pub url: Option<String>,
    pub track_id: Option<String>,
    pub playalbum: bool,
    pub defeat_ttp: bool,
    /// `$feed->{query}` of the top-level feed.
    pub top_query: Option<Obj>,
    /// Favorites index for a URL (`Slim::Utils::Favorites->findUrl`).
    pub fav_index: Option<FavLookup>,
    pub cachesearch_param: bool,
}

/// Favorites index lookup for a URL.
/// `itemsParams` name → variables (`%actionParamsNeeded`).
type Needed = Vec<(String, Vec<(String, String)>)>;

pub type FavFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Wrapper so `Ctx` can derive `Debug`.
#[derive(Clone)]
pub struct FavLookup(pub Arc<FavFn>);

impl std::fmt::Debug for FavLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FavLookup")
    }
}

impl Ctx {
    /// Build the context from a request (`cliQuery` + `_cliQuery_done` parameter handling).
    pub fn from_request(server: &Arc<Server>, r: &Request, query: &str) -> Ctx {
        let mut params = Obj::new();
        for (k, v) in &r.params {
            if let Some(v) = v {
                params.insert(k.clone(), json!(v));
            }
        }
        let player = r.client.and_then(|c| server.player(&c));
        let playalbum = match player.as_ref().and_then(|p| p.prefs.get("playtrackalbum")) {
            Some(v) if !v.is_null() => value_bool(&v),
            _ => server.prefs.server.get_bool("playtrackalbum"),
        };
        let favs = server.clone();
        Ctx {
            query: query.to_string(),
            terms: r.terms.clone(),
            params,
            index: r.param_i64("_index").unwrap_or(0).max(0) as usize,
            quantity: r.param_i64("_quantity").unwrap_or(0).max(0) as usize,
            menu_mode: r.param("menu").is_some(),
            interim_cm: r.param_bool("xmlBrowseInterimCM"),
            play_control: r
                .param_i64("xmlbrowserPlayControl")
                .map(|i| i.max(0) as usize),
            item_id: r.param("item_id").map(str::to_string),
            crumbs: Vec::new(),
            search: r.param("search").map(str::to_string),
            want_url: r.param_bool("want_url"),
            url: r.param_nonempty("url").map(str::to_string),
            track_id: r.param_nonempty("track_id").map(str::to_string),
            playalbum,
            defeat_ttp: defeat_destructive_ttp(server, r),
            top_query: None,
            fav_index: Some(FavLookup(Arc::new(move |u: &str| {
                favs.favorites.find_url(u)
            }))),
            cachesearch_param: r.param_bool("cachesearch"),
        }
    }
}

/// `_defeatDestructiveTouchToPlay` (default pref 4: defeat while a non-stream track plays).
fn defeat_destructive_ttp(server: &Arc<Server>, r: &Request) -> bool {
    let player = r.client.and_then(|c| server.player(&c));
    let pref = r
        .param_i64("defeatDestructiveTouchToPlay")
        .or_else(|| {
            player
                .as_ref()
                .and_then(|p| p.prefs.get("defeatDestructiveTouchToPlay"))
                .and_then(|v| value_i64(&v))
        })
        .or_else(|| {
            server
                .prefs
                .server
                .get("defeatDestructiveTouchToPlay")
                .and_then(|v| value_i64(&v))
        })
        .unwrap_or(4);
    if pref == 0 {
        return false;
    }
    let Some(p) = player else { return true };
    if pref == 1 {
        return true;
    }
    let c = p.controller();
    let c = c.lock();
    if pref == 4 {
        return c.is_playing(false)
            && c.playing_song()
                .map(|s| !s.is_remote() && s.duration().unwrap_or(0.0) > 0.0)
                .unwrap_or(false);
    }
    let len = c.playlist.len();
    if len < 2 {
        return false;
    }
    !(pref == 3 && !c.is_playing(false))
}

pub fn value_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some((*b as i32).to_string()),
        _ => None,
    }
}

pub fn value_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

pub fn value_bool(v: &Value) -> bool {
    value_i64(v).map(|i| i != 0).unwrap_or(false)
}

/// `_makeAction`.  `needed` collects `itemsParams` → variable lists for the base actions.
fn make_action(
    actions: &Actions,
    name: &str,
    needed: Option<&mut Needed>,
    player: bool,
    context_menu: bool,
    next_window: Option<&str>,
) -> Option<Value> {
    let a = actions.get(name)?;
    if a.command.is_empty() {
        return Some(json!("none"));
    }
    let mut params = a.fixed.clone();
    if !params.get("menu").map(value_bool_or_str).unwrap_or(false) {
        params.insert("menu".into(), json!(1));
    }
    let mut action = obj(json!({ "cmd": a.command, "params": params }));
    if player {
        action.insert("player".into(), json!(0));
    }
    if context_menu {
        action.insert("window".into(), json!({ "isContextMenu": 1 }));
    }
    if let Some(nw) = next_window {
        action.insert("nextWindow".into(), json!(nw));
    }
    if let Some(needed) = needed {
        if let Some(vars) = &a.variables {
            let key = format!("{}Params", name);
            action.insert("itemsParams".into(), json!(key));
            set_needed(needed, key, vars.clone());
        } else if !actions.common_variables.is_empty() {
            action.insert("itemsParams".into(), json!("commonParams"));
            set_needed(
                needed,
                "commonParams".into(),
                actions.common_variables.clone(),
            );
        }
    }
    Some(Value::Object(action))
}

fn value_bool_or_str(v: &Value) -> bool {
    match v {
        Value::String(s) => !s.is_empty() && s != "0",
        other => value_bool(other),
    }
}

fn set_needed(needed: &mut Needed, key: String, vars: Vec<(String, String)>) {
    match needed.iter_mut().find(|(k, _)| *k == key) {
        Some(e) => e.1 = vars,
        None => needed.push((key, vars)),
    }
}

/// `findAction`: item `itemActions` first, then the feed's `actions`.
fn find_action<'a>(
    feed: Option<&'a Feed>,
    item: &'a Item,
    name: &str,
) -> Option<(&'a ActionDef, Option<&'a Actions>)> {
    if let Some(ia) = &item.item_actions {
        if let Some(a) = ia.get(name) {
            return Some((a, None));
        }
        if ia.all_defined {
            return None;
        }
    }
    let fa = feed.and_then(|f| f.actions.as_ref())?;
    fa.get(name).map(|a| (a, Some(fa)))
}

/// `_makePlayAction`.
fn make_play_action(
    ctx: &Ctx,
    feed: Option<&Feed>,
    item: &Item,
    name: &str,
    next_window: &str,
    item_id: &str,
    play_index: Option<usize>,
) -> Value {
    let mut params = Obj::new();
    let cmd: Vec<String>;
    if let Some((a, fa)) = find_action(feed, item, name) {
        params = a.fixed.clone();
        let vars = a
            .variables
            .clone()
            .unwrap_or_else(|| fa.map(|f| f.common_variables.clone()).unwrap_or_default());
        for (p, f) in vars {
            if let Some(v) = item.field(&f) {
                params.insert(p, v);
            }
        }
        cmd = a.command.clone();
    } else {
        params.insert("item_id".into(), json!(item_id));
        if let Some(i) = play_index {
            params.insert("playIndex".into(), json!(i));
        }
        if let Some(m) = ctx.params.get("mode") {
            params.insert("mode".into(), m.clone());
        }
        cmd = vec![ctx.query.clone(), "playlist".into(), name.into()];
    }
    params.insert("menu".into(), json!(1));
    json!({ "player": 0, "cmd": cmd, "params": params, "nextWindow": next_window })
}

/// `_playlistControlContextMenu`: Add to End / Play Next / Play [/ Play all] / Favorites.
pub fn playlist_control_cm(
    ctx: &Ctx,
    item: &Item,
    sub_feed: Option<&Feed>,
    no_favorites: bool,
    item_id_arg: Option<String>,
    sub_id: Option<usize>,
    playalbum: bool,
) -> Vec<Obj> {
    let mut out = Vec::new();
    if item.has_audio() {
        let base_id = item_id_arg
            .clone()
            .or_else(|| ctx.item_id.clone())
            .unwrap_or_default();
        let mut item_id = base_id.clone();
        if let Some(sub) = sub_id {
            if !item_id.is_empty() {
                item_id.push('.');
            }
            item_id.push_str(&sub.to_string());
        }
        let add_play_all = playalbum
            && sub_id.is_some()
            && sub_feed.map(|f| f.count() > 1).unwrap_or(false)
            && item.playall;
        out.push(obj(json!({ "text": s("ADD_TO_END"), "style": "item_add", "actions": { "go": make_play_action(ctx, sub_feed, item, "add", "parentNoRefresh", &item_id, None) } })));
        out.push(obj(json!({ "text": s("PLAY_NEXT"), "style": "item_insert", "actions": { "go": make_play_action(ctx, sub_feed, item, "insert", "parentNoRefresh", &item_id, None) } })));
        out.push(obj(json!({ "text": s(if add_play_all { "PLAY_THIS_SONG" } else { "PLAY" }), "style": "item_play", "actions": { "go": make_play_action(ctx, sub_feed, item, "play", "nowPlaying", &item_id, None) } })));
        if add_play_all {
            out.push(obj(json!({ "text": s("JIVE_PLAY_ALL_SONGS"), "style": "item_playall", "actions": { "go": make_play_action(ctx, sub_feed, item, "playall", "nowPlaying", &base_id, sub_id) } })));
        }
    }
    if let (Some(fav), false) = (item.favorites_params(), no_favorites) {
        let url = fav
            .get("favorites_url")
            .and_then(value_string)
            .unwrap_or_default();
        let index = ctx.fav_index.as_ref().and_then(|f| (f.0)(&url));
        let mut params = obj(json!({
            "title": fav.get("favorites_title"),
            "url": url,
            "type": fav.get("favorites_type"),
            "parser": fav.get("parser"),
            "isContextMenu": 1,
        }));
        if params.get("parser").map(Value::is_null).unwrap_or(true) {
            params.remove("parser");
        }
        if let Some(i) = &index {
            params.insert("item_id".into(), json!(i));
        }
        if let Some(icon) = fav
            .get("icon")
            .cloned()
            .or_else(|| ctx.params.get("icon").cloned())
        {
            params.insert("icon".into(), icon);
        }
        let (cmd, token) = if index.is_some() {
            ("delete", "JIVE_DELETE_FROM_FAVORITES")
        } else {
            ("add", "JIVE_SAVE_TO_FAVORITES")
        };
        out.push(obj(json!({ "text": s(token), "style": "item_fav", "actions": { "go": { "player": 0, "cmd": ["jivefavorites", cmd], "params": params } } })));
    }
    out
}

/// `_fixCount`.
fn fix_count(index: &mut usize, quantity: &mut usize, count: usize) -> usize {
    if count > 0 {
        if *index == 0 && count == *quantity {
        } else if *index == 0 {
            *quantity = quantity.saturating_sub(1);
        } else {
            *index -= 1;
        }
    }
    count + 1
}

/// Render an `items` response for a resolved level.
pub fn render(ctx: &Ctx, level: Level, r: &mut Request) {
    match level {
        Level::List(feed) => render_list(ctx, &feed, r),
        Level::Leaf(item, parent) => render_leaf(ctx, &item, &parent, r),
    }
    r.set_done();
}

/// Leaf item query (audio item, `_cliQuery_done` "display the info").
fn render_leaf(ctx: &Ctx, item: &Item, _parent: &Feed, r: &mut Request) {
    let (valid, start, _) = normalize(Some(ctx.index as i64), Some(ctx.quantity as i64), 1);
    let mut cnt = 0usize;
    if ctx.menu_mode {
        if ctx.interim_cm {
            for m in playlist_control_cm(ctx, item, None, false, None, None, false) {
                r.add_loop_item("item_loop", m);
                cnt += 1;
            }
        }
    } else {
        r.add_result("count", 1);
    }
    if !valid {
        return;
    }
    if ctx.menu_mode {
        r.add_result("offset", start);
        let mut attrs: Vec<String> = Vec::new();
        if let Some(n) = item.name.clone().filter(|n| !n.is_empty()) {
            attrs.push(format!("{}: {}", s("TITLE"), n));
        }
        if let Some(u) = &item.url {
            attrs.push(format!("{}: {}", s("URL"), u));
        }
        if let Some(d) = item.field_str("description") {
            attrs.push(format!("{}: {}", s("DESCRIPTION"), d));
        }
        if let Some(b) = item.field_str("bitrate") {
            attrs.push(format!("{}: {}{}", s("BITRATE"), b, s("KBPS")));
        }
        if let Some(d) = item
            .fields
            .get("duration")
            .and_then(value_i64)
            .filter(|d| *d > 0)
        {
            attrs.push(format!("{}: {}:{:02}", s("LENGTH"), d / 60, d % 60));
        }
        for (k, t) in [
            ("listeners", "NUMBER_OF_LISTENERS"),
            ("current_track", "NOW_PLAYING"),
            ("genre", "GENRE"),
            ("source", "SOURCE"),
        ] {
            if let Some(v) = item.field_str(k) {
                attrs.push(format!("{}: {}", s(t), v));
            }
        }
        for text in attrs {
            r.add_loop_item(
                "item_loop",
                obj(json!({ "text": text, "style": "itemNoAction", "action": "none" })),
            );
            cnt += 1;
        }
        r.add_result("count", cnt);
    } else {
        let mut h = Obj::new();
        h.insert("id".into(), json!(ctx.item_id.clone().unwrap_or_default()));
        if let Some(n) = &item.name {
            h.insert("name".into(), json!(n));
        }
        if let Some(t) = &item.title {
            h.insert("title".into(), json!(t));
        }
        h.insert("isaudio".into(), json!(item.has_audio() as i32));
        if let Some(k) = &item.kind {
            h.insert("type".into(), json!(k));
        }
        if let Some(u) = &item.url {
            h.insert("url".into(), json!(u));
        }
        for k in ["icon", "image"] {
            if let Some(v) = if k == "icon" { &item.icon } else { &item.image } {
                h.insert(k.into(), json!(v));
            }
        }
        for (k, v) in &item.fields {
            if !v.is_null() && !v.is_object() && !v.is_array() {
                h.insert(k.clone(), v.clone());
            }
        }
        r.add_loop_item("loop_loop", h);
    }
}

/// Image fields of an item hash (`icon` / `icon-id`).
fn proxied(url: &str) -> String {
    crate::control::songdata::proxied_image(url)
}

fn render_list(ctx: &Ctx, feed: &Feed, r: &mut Request) {
    let mut items: Vec<Item> = feed.items.clone();
    let mut count = feed.count();
    let mut total_count = count;
    let mut window = Obj::new();
    let mut has_image = false;
    let mut window_style: Option<String> = None;
    let mut preset_fav_set = false;
    let mut all_ttp = true;
    let mut needed: Needed = Vec::new();
    let mut base: Option<Obj> = None;
    let mut index = ctx.index;
    let mut quantity = ctx.quantity;

    if ctx.menu_mode && ctx.play_control.is_some() {
        total_count = 0;
        let pc = ctx.play_control.unwrap_or(0);
        if let Some(item) = pc.checked_sub(feed.offset).and_then(|i| items.get(i)) {
            let mut crumbs = ctx.crumbs.clone();
            crumbs.insert(0, "ffffffff".into());
            let item_id = crumbs.join(".");
            for m in playlist_control_cm(ctx, item, Some(feed), true, Some(item_id), Some(pc), true)
            {
                r.add_loop_item("item_loop", m);
                total_count += 1;
            }
        }
        r.add_result("offset", 0);
    } else if ctx.menu_mode || count > 0 || ctx.interim_cm {
        if ctx.menu_mode && count == 0 && !ctx.interim_cm {
            items = vec![Item::text(s("EMPTY"))];
            count = 1;
            total_count = 1;
        }
        let loop_name = if ctx.menu_mode {
            "item_loop"
        } else {
            "loop_loop"
        };
        if ctx.menu_mode {
            r.add_result("offset", ctx.index);
            let first_chunk = ctx.index == 0;
            if ctx.interim_cm && !feed.menu_complete {
                if let Some(parent) = &feed.parent {
                    for m in playlist_control_cm(ctx, parent, None, false, None, None, false) {
                        total_count = fix_count(&mut index, &mut quantity, total_count);
                        if first_chunk {
                            r.add_loop_item("item_loop", m);
                        }
                    }
                }
            }
        }
        let (valid, start, end) = normalize(Some(index as i64), Some(quantity as i64), count);
        if valid {
            let feed_actions = feed.actions.clone().unwrap_or_default();
            if let Some(t) = feed.title.clone().or(feed.name.clone()) {
                let t = match (&feed.name2, ctx.menu_mode) {
                    (Some(n2), true) => format!("{}\n{}", t, n2),
                    _ => t,
                };
                r.add_result("title", t);
            }
            let mut params = Obj::new();
            if ctx.menu_mode {
                let mut b = Obj::new();
                if !feed_actions.all_defined {
                    params.insert("menu".into(), json!(ctx.query));
                    if let Some(u) = &ctx.url {
                        params.insert("url".into(), json!(u));
                    }
                    if let Some(q) = &ctx.top_query {
                        params.extend(q.clone());
                    } else if let Some(t) = &ctx.track_id {
                        params.insert("track_id".into(), json!(t));
                    }
                    b.insert("go".into(), json!({ "cmd": [ctx.query, "items"], "params": params, "itemsParams": "params" }));
                    b.insert("play".into(), json!({ "player": 0, "cmd": [ctx.query, "playlist", "play"], "itemsParams": "params", "params": params, "nextWindow": "nowPlaying" }));
                    b.insert("add".into(), json!({ "player": 0, "cmd": [ctx.query, "playlist", "add"], "itemsParams": "params", "params": params }));
                    b.insert("add-hold".into(), json!({ "player": 0, "cmd": [ctx.query, "playlist", "insert"], "itemsParams": "params", "params": params }));
                    b.insert("more".into(), json!({ "player": 0, "cmd": [ctx.query, "items"], "itemsParams": "params", "params": params, "window": { "isContextMenu": 1 } }));
                }
                if let Some(fa) = &feed.actions {
                    let mut n = 0;
                    if let Some(a) = make_action(fa, "info", Some(&mut needed), true, true, None) {
                        b.insert("more".into(), a);
                        n += 1;
                    }
                    if let Some(a) = make_action(fa, "items", Some(&mut needed), true, false, None)
                    {
                        b.insert("go".into(), a);
                        n += 1;
                    }
                    let play = if ctx.playalbum {
                        make_action(
                            fa,
                            "playall",
                            Some(&mut needed),
                            true,
                            false,
                            Some("nowPlaying"),
                        )
                    } else {
                        None
                    };
                    if let Some(a) = play.or_else(|| {
                        make_action(
                            fa,
                            "play",
                            Some(&mut needed),
                            true,
                            false,
                            Some("nowPlaying"),
                        )
                    }) {
                        b.insert("play".into(), a);
                        n += 1;
                    }
                    let add = if ctx.playalbum {
                        make_action(fa, "addall", Some(&mut needed), true, false, None)
                    } else {
                        None
                    };
                    if let Some(a) =
                        add.or_else(|| make_action(fa, "add", Some(&mut needed), true, false, None))
                    {
                        b.insert("add".into(), a);
                        n += 1;
                    }
                    if let Some(a) = make_action(fa, "insert", Some(&mut needed), true, false, None)
                    {
                        b.insert("add-hold".into(), a);
                        n += 1;
                    }
                    let _ = n >= 5;
                }
                let mut pc_params = ctx.params.clone();
                pc_params.remove("_index");
                pc_params.remove("_quantity");
                b.insert("playControl".into(), json!({ "player": 0, "window": { "isContextMenu": 1 }, "cmd": ctx.terms, "itemsParams": "playControlParams", "params": pc_params }));
                base = Some(b);
            }
            let feed_all_defined = feed_actions.all_defined
                || feed.actions.as_ref().map(count_defined).unwrap_or(0) >= 5;
            let base_id = if ctx.crumbs.is_empty() {
                String::new()
            } else {
                format!("{}.", ctx.crumbs.join("."))
            };
            let slice_start = start.saturating_sub(feed.offset);
            let slice_end = end.saturating_sub(feed.offset);
            for (this_index, item) in
                (start..).zip(items.iter().take(slice_end + 1).skip(slice_start))
            {
                let id = format!("{}{}", base_id, this_index);
                let mut name = item.name.clone();
                if let (Some(n), Some(label)) = (&name, &item.label) {
                    if !n.is_empty() {
                        name = Some(format!("{}{} {}", s(label), s("COLON"), n));
                    }
                }
                if ctx.menu_mode {
                    let name_or_title = name
                        .clone()
                        .filter(|n| !n.is_empty())
                        .or(item.title.clone())
                        .unwrap_or_default();
                    if item
                        .fields
                        .get("nowPlaying")
                        .map(value_bool)
                        .unwrap_or(false)
                    {
                        r.add_result("goNow", "nowPlaying");
                    }
                    if (item.name.is_some() && item.wrap) || item.is_type("textarea") {
                        window.insert(
                            "textarea".into(),
                            json!(item.name.clone().unwrap_or_default()),
                        );
                        total_count = total_count.saturating_sub(1);
                        if r.loop_len(loop_name) == 0 {
                            r.add_loop_item(loop_name, Obj::new());
                        }
                        continue;
                    }
                    if ctx.interim_cm
                        && (item.play.is_some() || item.is_type("audio"))
                        && total_count < ctx.quantity
                    {
                        total_count = total_count.saturating_sub(1);
                        continue;
                    }
                    let (h, ttp_all, img, style, preset) = item_hash(
                        ctx,
                        item,
                        &id,
                        this_index,
                        &name_or_title,
                        &feed_actions,
                        feed_all_defined,
                        &params,
                        &needed,
                    );
                    if !ttp_all {
                        all_ttp = false;
                    }
                    has_image |= img;
                    if let Some(st) = style {
                        window_style = Some(st);
                    }
                    preset_fav_set |= preset;
                    r.add_loop_item(loop_name, h);
                } else {
                    let mut h = Obj::new();
                    h.insert("id".into(), json!(id));
                    if let Some(n) = &name {
                        h.insert("name".into(), json!(n));
                    }
                    if let Some(k) = &item.kind {
                        h.insert("type".into(), json!(k));
                    }
                    if let Some(t) = &item.title {
                        h.insert("title".into(), json!(t));
                    }
                    if let Some(i) = item.image.clone().or(item.icon.clone()) {
                        h.insert("image".into(), json!(proxied(&i)));
                    }
                    if ctx.want_url {
                        if let Some(u) = &item.url {
                            h.insert("url".into(), json!(u));
                        }
                    }
                    h.insert("isaudio".into(), json!(item.has_audio() as i32));
                    let has_items = match item.kind.as_deref() {
                        Some("text") | Some("audio") => {
                            item.items.as_ref().map(|i| i.len()).unwrap_or(0)
                        }
                        _ => 1,
                    };
                    h.insert("hasitems".into(), json!(has_items));
                    r.add_loop_item(loop_name, h);
                }
            }
        }
    }
    r.add_result("count", total_count);
    if ctx.menu_mode {
        if let Some(mut b) = base {
            if preset_fav_set {
                for n in 0..10 {
                    b.insert(format!("set-preset-{}", n), json!({ "player": 0, "cmd": ["jivefavorites", "set_preset", format!("key:{}", n)], "itemsParams": "presetParams" }));
                }
            }
            if all_ttp {
                let key = if ctx.defeat_ttp {
                    "playControl"
                } else {
                    "play"
                };
                if let Some(a) = b.get(key).cloned() {
                    b.insert("go".into(), a);
                }
            }
            r.add_result("base", json!({ "actions": b }));
        }
        let ws = window_style.unwrap_or_else(|| {
            if has_image {
                "home_menu".into()
            } else {
                "text_list".into()
            }
        });
        window.insert("windowStyle".into(), json!(ws));
        if let Some(w) = &feed.window_id {
            window.insert("windowId".into(), json!(w));
        }
        r.add_result("window", Value::Object(window));
    }
}

fn count_defined(a: &Actions) -> usize {
    ["info", "items", "play", "add", "insert"]
        .iter()
        .filter(|n| a.get(n).is_some())
        .count()
}

/// The per-item hash (menu mode).  Returns (hash, allTouchToPlay still possible, has image,
/// custom window style, preset params set).
#[allow(clippy::too_many_arguments)]
fn item_hash(
    ctx: &Ctx,
    item: &Item,
    id: &str,
    item_index: usize,
    name_or_title: &str,
    feed_actions: &Actions,
    feed_all_defined: bool,
    params: &Obj,
    needed: &[(String, Vec<(String, String)>)],
) -> (Obj, bool, bool, Option<String>, bool) {
    let mut h = Obj::new();
    let mut all_ttp = true;
    let mut has_image = false;
    let mut window_style = None;
    let mut preset = false;
    if let Some(k) = &item.kind {
        h.insert("type".into(), json!(k));
    }
    let touch = item.touch_to_play();
    if let Some(st) = &item.style {
        window_style = Some(st.clone());
    }
    if item
        .fields
        .get("autoplays")
        .map(value_bool)
        .unwrap_or(false)
    {
        h.insert("style".into(), json!("itemplay"));
    } else if let Some(pc) = &item.playcontrol {
        match pc.as_str() {
            "play" => h.insert("style".into(), json!("item_play")),
            "add" => h.insert("style".into(), json!("item_add")),
            "insert" => h.insert("style".into(), json!("item_insert")),
            _ => None,
        };
    }
    let mut text = name_or_title.to_string();
    if let Some(n2) = item.name2.clone().filter(|n| !n.is_empty()) {
        text = format!("{}\n{}", text, n2);
        window_style.get_or_insert_with(|| "icon_list".into());
    } else if let Some(l2) = item
        .field_str("line2")
        .or_else(|| item.field_str("subtext"))
    {
        window_style = Some("icon_list".into());
        text = format!("{}\n{}", item.field_str("line1").unwrap_or(text), l2);
    }
    h.insert("text".into(), json!(text));
    let playable = item.is_playable();
    if playable {
        if let Some(pp) = item.favorites_params() {
            if !ctx.interim_cm {
                h.insert("presetParams".into(), Value::Object(pp));
                preset = true;
            }
        }
    }
    let mut item_params = Obj::new();
    if !item.is_type("text") {
        item_params.insert("item_id".into(), json!(id));
    }
    if playable || item.is_context_menu {
        item_params.insert("isContextMenu".into(), json!(1));
    }
    if item.is_type("slideshow") {
        item_params.insert("slideshow".into(), json!(1));
    }
    if let (Some(w), Some(_)) = (&item.weblink, &item.name) {
        h.insert("weblink".into(), json!(w));
    }
    let mut merged = params.clone();
    merged.extend(item_params.clone());
    if let Some(icon) = &item.icon {
        let key = if icon.starts_with("http://") || icon.starts_with("https://") {
            "icon"
        } else {
            "icon-id"
        };
        h.insert(key.into(), json!(proxied(icon)));
        has_image = true;
    } else if let Some(img) = &item.image {
        h.insert("icon".into(), json!(proxied(img)));
        has_image = true;
    }
    if let Some(c) = &item.artwork_track_id {
        h.insert("icon-id".into(), json!(proxied(c)));
        has_image = true;
    }
    if item.is_type("text") && !item.wrap && item.jive.is_none() {
        h.entry("style").or_insert(json!("itemNoAction"));
        h.insert("action".into(), json!("none"));
    }
    if let Some(e) = item.field_str("extid") {
        h.insert("extid".into(), json!(e));
    }
    if item.parse_urls {
        h.insert("parseURLs".into(), json!(1));
    }
    let style_no_action = h.get("style").and_then(Value::as_str) == Some("itemNoAction");
    if item.is_type("localservice") {
        h.insert(
            "actions".into(),
            json!({ "go": { "localservice": item.field("serviceId") } }),
        );
    } else if item.is_type("search") {
        let mut go_params = params.clone();
        go_params.insert("item_id".into(), json!(id));
        go_params.insert("search".into(), json!("__TAGGEDINPUT__"));
        go_params.insert(
            "cachesearch".into(),
            item.cachesearch
                .clone()
                .map(Value::String)
                .unwrap_or(json!(1)),
        );
        h.insert(
            "actions".into(),
            json!({ "go": { "cmd": [ctx.query, "items"], "params": go_params } }),
        );
        h.insert(
            "input".into(),
            json!({
                "len": 1,
                "processingPopup": { "text": s("SEARCHING") },
                "help": { "text": s("JIVE_SEARCHFOR_HELP") },
                "softbutton1": s("INSERT"),
                "softbutton2": s("DELETE"),
                "title": item.title.clone().or(item.name.clone()),
            }),
        );
        if let Some(nw) = &item.next_window {
            h.insert("nextWindow".into(), json!(nw));
        }
        all_ttp = false;
    } else if !playable && !touch && !style_no_action {
        let mut go = obj(json!({ "cmd": [ctx.query, "items"], "params": merged }));
        if let Some(nw) = &item.next_window {
            go.insert("nextWindow".into(), json!(nw));
            if nw == "nowPlaying" {
                h.insert("style".into(), json!("itemplay"));
            }
        }
        h.insert("actions".into(), json!({ "go": go }));
        h.insert("addAction".into(), json!("go"));
        all_ttp = false;
    } else if touch {
        if !ctx.defeat_ttp {
            item_params.insert("touchToPlay".into(), json!(id));
            if !item.playall {
                item_params.insert("touchToPlaySingle".into(), json!(1));
            }
            h.insert("goAction".into(), json!("play"));
            h.insert("style".into(), json!("itemplay"));
        } else {
            h.insert("goAction".into(), json!("playControl"));
            h.insert(
                "playControlParams".into(),
                json!({ "xmlbrowserPlayControl": item_index.to_string() }),
            );
        }
    } else {
        all_ttp = false;
    }

    let item_all_defined = item
        .item_actions
        .as_ref()
        .map(|a| a.all_defined)
        .unwrap_or(false);
    if let Some(ia) = &item.item_actions {
        let mut actions = if !ia.all_defined {
            h.get("actions")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        } else {
            Obj::new()
        };
        let go_action = h
            .get("goAction")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(a) = make_action(ia, "info", None, true, true, None) {
            actions.insert("more".into(), a);
        }
        if let Some(a) = make_action(ia, "items", None, true, false, item.next_window.as_deref()) {
            if go_action.is_none() {
                actions.insert("go".into(), a);
            }
        }
        if let Some(a) = make_action(ia, "play", None, true, false, Some("nowPlaying")) {
            actions.insert("play".into(), a.clone());
            if go_action.as_deref() == Some("play") {
                actions.insert("go".into(), a);
            }
        }
        if let Some(a) = make_action(ia, "add", None, true, false, None) {
            actions.insert("add".into(), a);
        }
        if let Some(a) = make_action(ia, "insert", None, true, false, None) {
            actions.insert("add-hold".into(), a);
        }
        h.insert("actions".into(), Value::Object(actions));
    }
    let item_all_defined =
        item_all_defined || item.item_actions.as_ref().map(count_defined).unwrap_or(0) >= 5;
    if !item_all_defined {
        for (key, vars) in needed {
            let mut p = Obj::new();
            for (param, field) in vars {
                if let Some(v) = item.field(field).filter(|v| !v.is_null()) {
                    p.insert(param.clone(), v);
                }
            }
            if !p.is_empty() {
                h.insert(key.clone(), Value::Object(p));
            }
        }
    }
    if !item_all_defined && !feed_all_defined && !item_params.is_empty() && (playable || touch) {
        h.insert("params".into(), Value::Object(item_params));
    }
    let _ = feed_actions;
    if let Some(j) = &item.jive {
        let mut actions = h
            .get("actions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(Value::Object(ja)) = j.get("actions") {
            for (k, v) in ja {
                actions.insert(k.clone(), v.clone());
            }
        }
        h.insert("actions".into(), Value::Object(actions));
        for key in ["window", "showBigArtwork", "style", "nextWindow"] {
            if let Some(v) = j.get(key).filter(|v| value_bool_or_str(v) || v.is_object()) {
                h.insert(key.into(), v.clone());
            }
        }
        if let Some(i) = j.get("icon-id").and_then(value_string) {
            h.insert("icon-id".into(), json!(proxied(&i)));
        }
    }
    if h.get("actions")
        .and_then(Value::as_object)
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        h.remove("action");
        if h.get("style").and_then(Value::as_str) == Some("itemNoAction") {
            h.remove("style");
        }
    }
    if let Some(tk) = &item.textkey {
        h.insert("textkey".into(), json!(tk));
    }
    (h, all_ttp, has_image, window_style, preset)
}

/// Is this request a play command (`<query> playlist …`, or touch-to-play `items`)?
/// Applies the touch-to-play rewrite of `cliQuery` (method, `playIndex`, stripped item id).
pub fn prepare_play(ctx: &mut Ctx, r: &mut Request) -> Option<(String, Option<usize>)> {
    let is_playlist = r.term(1) == "playlist";
    let touch = r.param_nonempty("touchToPlay").is_some() && !ctx.interim_cm;
    if !is_playlist && !touch {
        return None;
    }
    let mut method = r.param("_method").map(str::to_string);
    let mut play_index = None;
    if touch && (!is_playlist || method.as_deref() == Some("play")) {
        if method.is_none() {
            method = Some("play".into());
            r.add_result("goNow", "nowPlaying");
        }
        if ctx.playalbum && !r.param_bool("touchToPlaySingle") {
            let id = ctx.item_id.clone().unwrap_or_default();
            if let Some((head, last)) = id.rsplit_once('.') {
                if let Ok(i) = last.parse::<usize>() {
                    play_index = Some(i);
                    ctx.item_id = Some(head.to_string());
                }
            } else if let Ok(i) = id.parse::<usize>() {
                play_index = Some(i);
                ctx.item_id = Some(String::new());
            }
        }
    } else if !is_playlist {
        return None;
    }
    if play_index.is_none() {
        play_index = r.param_i64("playIndex").map(|i| i.max(0) as usize);
    }
    Some((method.unwrap_or_else(|| "play".into()), play_index))
}

/// `$isPlaylistCmd` branch: play/add/insert the item (or all audio children of the level).
pub async fn play(
    server: &Arc<Server>,
    r: &mut Request,
    level: Level,
    method: &str,
    play_index: Option<usize>,
) {
    let Some(p) = r.client.and_then(|c| server.player(&c)) else {
        r.bad_params();
        return;
    };
    if !matches!(
        method,
        "add" | "addall" | "play" | "playall" | "insert" | "load"
    ) {
        r.bad_params();
        return;
    }
    let mut method = method.to_string();
    // Single item, or all items of a list.
    let (single, list): (Option<Item>, Option<Feed>) = match level {
        Level::Leaf(item, _) => (Some(*item), None),
        Level::List(feed) => {
            if let Some(pi) = play_index.filter(|_| r.param_nonempty("touchToPlay").is_some()) {
                match pi.checked_sub(feed.offset).and_then(|i| feed.items.get(i)) {
                    Some(it) if (method == "add" || method == "play") && it.playall => {
                        method.push_str("all");
                        (None, Some(feed))
                    }
                    Some(it) => (Some(it.clone()), None),
                    None => (None, Some(feed)),
                }
            } else {
                (None, Some(feed))
            }
        }
    };
    if let Some(item) = single.as_ref().filter(|i| {
        (i.is_type("audio") && i.url.is_some() || i.play.is_some()) && !method.contains("all")
    }) {
        let url = item.play.clone().or(item.url.clone()).unwrap_or_default();
        if let Some(t) = item
            .name
            .clone()
            .or(item.title.clone())
            .filter(|t| !t.is_empty())
        {
            if crate::util::is_remote_url(&url) {
                server.remote_titles.insert(url.clone(), t);
            }
        }
        let tokens = vec!["playlist".to_string(), method.clone(), url];
        crate::control::dispatch::execute_tokens(server, r.client, tokens, r.source.clone()).await;
        r.set_done();
        return;
    }
    let items: Vec<Item> = match (single, list) {
        (Some(it), _) => it.items.clone().unwrap_or_default(),
        (None, Some(f)) => f.items,
        _ => Vec::new(),
    };
    let mut urls: Vec<(String, String)> = Vec::new();
    let mut pi = if method.starts_with("play") || method == "load" {
        play_index
    } else {
        None
    };
    for it in &items {
        let url = if it.is_type("audio") {
            it.url.clone()
        } else {
            None
        }
        .or(it.play.clone());
        let skip = url.is_none()
            || (pi.is_some() && !it.playall && r.param_nonempty("touchToPlay").is_some());
        if skip {
            if let Some(i) = pi {
                if i >= urls.len() && i > 0 {
                    pi = Some(i - 1);
                }
            }
            continue;
        }
        urls.push((it.name.clone().unwrap_or_default(), url.unwrap_or_default()));
    }
    if urls.is_empty() {
        r.set_done();
        return;
    }
    let cmd = if method.starts_with("play") || method == "load" {
        "load"
    } else if method.starts_with("add") {
        "add"
    } else {
        "insert"
    };
    let mut entries = Vec::new();
    for (title, u) in urls {
        if crate::util::is_remote_url(&u) && !title.is_empty() {
            server.remote_titles.insert(u.clone(), title);
        }
        entries.extend(crate::control::commands::expand_item(server, &u).await);
    }
    crate::control::commands::apply_tracks(
        server,
        &p,
        cmd,
        entries,
        if cmd == "load" { pi } else { None },
        None,
    );
    r.set_done();
}

/// `_jiveNoResults`.
pub fn no_results(r: &mut Request) {
    r.add_result("count", "1");
    r.add_result("offset", 0);
    r.add_loop_item(
        "item_loop",
        obj(json!({ "text": s("EMPTY"), "style": "itemNoAction", "action": "none" })),
    );
    r.set_done();
}

/// Build loop items as `Results` (helper for tests and callers).
#[cfg(test)]
pub fn results_loop(r: &Request, name: &str) -> Vec<Results> {
    match r.result(name) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_object().cloned()).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(query: &str) -> Ctx {
        Ctx {
            query: query.into(),
            terms: vec![query.into(), "items".into()],
            index: 0,
            quantity: 100,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        }
    }

    fn album_feed() -> Feed {
        let params = obj(json!({ "artist_id": "12" }));
        let mut fixed = obj(json!({ "mode": "tracks" }));
        fixed.extend(params.clone());
        let actions = Actions {
            all_defined: true,
            info: Some(ActionDef::cmd(&["albuminfo", "items"], params.clone())),
            items: Some(ActionDef::cmd(&["browselibrary", "items"], fixed)),
            ..Default::default()
        }
        .with_playlistcontrol(&params)
        .common(&[("album_id", "id"), ("performance", "performance")]);
        let mut it = Item {
            name: Some("Abbey Road".into()),
            kind: Some("playlist".into()),
            lazy: true,
            playlist_ref: true,
            ..Default::default()
        };
        it.fields.insert("id".into(), json!(7));
        it.fields.insert("performance".into(), json!(""));
        it.fields
            .insert("favorites_url".into(), json!("db:album.title=Abbey%20Road"));
        it.fields
            .insert("favorites_title".into(), json!("Abbey Road"));
        it.artwork_track_id = Some("abcd".into());
        Feed {
            items: vec![it],
            actions: Some(actions),
            ..Default::default()
        }
    }

    #[test]
    fn base_actions_from_feed_actions() {
        let mut r = Request::blank();
        render(&ctx("browselibrary"), Level::List(album_feed()), &mut r);
        let base = r.result("base").unwrap();
        let go = &base["actions"]["go"];
        assert_eq!(go["cmd"], json!(["browselibrary", "items"]));
        assert_eq!(go["params"]["mode"], "tracks");
        assert_eq!(go["params"]["artist_id"], "12");
        assert_eq!(go["params"]["menu"], 1);
        assert_eq!(go["itemsParams"], "commonParams");
        assert_eq!(
            base["actions"]["more"]["cmd"],
            json!(["albuminfo", "items"])
        );
        assert_eq!(base["actions"]["more"]["window"]["isContextMenu"], 1);
        assert_eq!(base["actions"]["play"]["params"]["cmd"], "load");
        assert_eq!(base["actions"]["play"]["nextWindow"], "nowPlaying");
        assert_eq!(base["actions"]["add-hold"]["params"]["cmd"], "insert");
        assert!(base["actions"]["set-preset-0"].is_object());
        let items = results_loop(&r, "item_loop");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "Abbey Road");
        assert_eq!(
            items[0]["commonParams"],
            json!({ "album_id": 7, "performance": "" })
        );
        assert_eq!(items[0]["icon-id"], "abcd");
        assert!(items[0].get("params").is_none());
        assert_eq!(r.result("count"), Some(&json!(1)));
        assert_eq!(r.result("window").unwrap()["windowStyle"], "home_menu");
    }

    #[test]
    fn empty_list_and_text_items() {
        let mut r = Request::blank();
        render(&ctx("trackinfo"), Level::List(Feed::default()), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Empty");
        assert_eq!(items[0]["style"], "itemNoAction");
        assert_eq!(items[0]["action"], "none");
    }

    #[test]
    fn audio_items_touch_to_play() {
        let mut it = Item {
            name: Some("Groove Salad".into()),
            kind: Some("audio".into()),
            url: Some("http://ice2.somafm.com/groovesalad-128-mp3".into()),
            ..Default::default()
        };
        it.icon = Some("https://somafm.com/img/groovesalad120.png".into());
        let feed = Feed::new(vec![it]);
        let mut c = ctx("radio");
        c.crumbs = vec!["1".into()];
        let mut r = Request::blank();
        render(&c, Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["goAction"], "play");
        assert_eq!(items[0]["style"], "itemplay");
        assert_eq!(items[0]["params"]["item_id"], "1.0");
        assert_eq!(items[0]["params"]["touchToPlay"], "1.0");
        assert_eq!(items[0]["params"]["touchToPlaySingle"], 1);
        assert_eq!(items[0]["presetParams"]["favorites_type"], "audio");
        let base = r.result("base").unwrap();
        // all items touch-to-play: go = play
        assert_eq!(
            base["actions"]["go"]["cmd"],
            json!(["radio", "playlist", "play"])
        );
        assert_eq!(base["actions"]["go"]["params"]["menu"], "radio");
    }

    #[test]
    fn leaf_context_menu() {
        let it = Item {
            name: Some("KEXP".into()),
            kind: Some("audio".into()),
            url: Some("https://kexp-mp3-128.streamguys1.com/kexp128.mp3".into()),
            ..Default::default()
        };
        let mut c = ctx("radio");
        c.interim_cm = true;
        c.item_id = Some("0.3".into());
        let mut r = Request::blank();
        render(&c, Level::Leaf(Box::new(it), Feed::default()), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Add to End");
        assert_eq!(
            items[0]["actions"]["go"]["cmd"],
            json!(["radio", "playlist", "add"])
        );
        assert_eq!(items[0]["actions"]["go"]["params"]["item_id"], "0.3");
        assert_eq!(items[2]["text"], "Play");
        assert_eq!(
            items[3]["actions"]["go"]["cmd"],
            json!(["jivefavorites", "add"])
        );
        assert_eq!(items[4]["text"], "Title: KEXP");
        assert_eq!(r.result("count"), Some(&json!(6)));
    }

    #[test]
    fn descend_static_tree() {
        let child = Item {
            name: Some("Track Number: 3".into()),
            kind: Some("text".into()),
            ..Default::default()
        };
        let more = Item {
            name: Some("More Info".into()),
            items: Some(vec![child]),
            is_context_menu: true,
            ..Default::default()
        };
        let feed = Feed::new(vec![Item::text("x"), more]);
        match descend(feed, &["1".into()], true).unwrap() {
            Level::List(f) => {
                assert_eq!(f.items.len(), 1);
                assert_eq!(f.title.as_deref(), Some("More Info"));
            }
            _ => panic!("expected list"),
        }
        assert_eq!(split_item_id("ffffffff.3.1"), vec!["3", "1"]);
        assert_eq!(split_item_id("3.1"), vec!["3", "1"]);
        assert_eq!(component_search("2_foo%20bar").as_deref(), Some("foo bar"));
        assert_eq!(escape_search("foo bar"), "foo%20bar");
    }
}
