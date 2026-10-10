//! Jive / SqueezePlay menu protocol (`Slim::Control::Jive`, `Slim::Menu::*`,
//! `Slim::Control::XMLBrowser`) — what SqueezePlay, Squeezer, iPeng and Material browse with.
//!
//! * [`menu`]: the home `menu` query and `menustatus` notifications (`Jive.pm` `mainMenu`).
//! * [`settings`]: the player settings sub-menus and misc Jive commands.
//! * [`browse`]: `browselibrary items|playlist` (`Slim::Menu::BrowseLibrary`) and `globalsearch`.
//! * [`info`]: `trackinfo|albuminfo|artistinfo|genreinfo|yearinfo|playlistinfo|folderinfo items`
//!   and `contextmenu` (`Slim::Menu::*Info`), plus `systeminfo items`.
//! * [`favorites`]: favorites in menu form and `jivefavorites`.
//! * [`radio`]: the `radios` node and a small built-in internet radio directory.
//! * [`xmlbrowser`]: the shared OPML → Jive item renderer.

pub mod browse;
pub mod favorites;
pub mod info;
pub mod menu;
pub mod radio;
pub mod settings;
pub mod xmlbrowser;

use super::dispatch::{DispatchTable, Handler};
use super::request::{normalize, Request, Source};
use crate::player::Player;
use crate::server::Server;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

macro_rules! asyncf {
    ($f:path) => {
        Handler::Async(|s, r| Box::pin($f(s, r)))
    };
}

/// Register the Jive dispatch entries (`Slim::Control::Jive::init` and the menu queries).
pub fn register(t: &mut DispatchTable) {
    let s = Handler::Sync;
    // terms, needsClient, isQuery, hasTags, handler
    t.add(
        &["menu", "_index", "_quantity"],
        2,
        true,
        true,
        s(menu::menu_query),
    );
    t.add(
        &["alarmsettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::alarm_settings),
    );
    t.add(
        &["jiveupdatealarm", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::alarm_update_menu),
    );
    t.add(
        &["jiveupdatealarmdays", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::alarm_update_days),
    );
    t.add(
        &["jivealarmvolume"],
        1,
        false,
        true,
        s(settings::alarm_volume),
    );
    t.add(
        &["syncsettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::sync_settings),
    );
    t.add(
        &["sleepsettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::sleep_settings),
    );
    t.add(
        &["jivetonesettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::tone_settings),
    );
    t.add(
        &["jivefixedvolumesettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::fixed_volume_settings),
    );
    t.add(
        &["jivestereoxl", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::stereoxl_settings),
    );
    t.add(
        &["jivelineout", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::lineout_settings),
    );
    t.add(
        &["crossfadesettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::crossfade_settings),
    );
    t.add(
        &["replaygainsettings", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::replaygain_settings),
    );
    t.add(
        &["jivedummycommand", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::done),
    );
    t.add(&["jivealarm"], 1, false, true, s(settings::jive_alarm));
    t.add(
        &["jiveendoftracksleep", "_index", "_quantity"],
        1,
        true,
        true,
        s(settings::end_of_track_sleep),
    );
    t.add(
        &["jivefavorites", "_cmd"],
        1,
        false,
        true,
        s(favorites::jivefavorites),
    );
    t.add(
        &["jivepresets", "_index", "_quantity"],
        1,
        true,
        true,
        s(favorites::jivepresets),
    );
    t.add(
        &["jivealbumsortsettings"],
        1,
        false,
        true,
        s(settings::album_sort_settings),
    );
    t.add(
        &["jivesetalbumsort"],
        1,
        false,
        true,
        s(settings::set_album_sort),
    );
    t.add(&["jivesync"], 1, false, true, s(settings::jive_sync));
    t.add(
        &["jiveplaylists", "_cmd"],
        1,
        false,
        true,
        s(settings::jive_playlists),
    );
    t.add(
        &["jiverecentsearches"],
        0,
        true,
        false,
        s(recent_searches_query),
    );
    t.add(&["date"], 0, true, true, s(settings::date));
    t.add(
        &["firmwareupgrade"],
        0,
        true,
        true,
        s(settings::firmware_upgrade),
    );
    for q in ["jiveapplets", "jivewallpapers", "jivesounds", "jivepatches"] {
        t.add(&[q], 0, true, true, s(settings::extensions));
    }
    t.add(&["jiveblankcommand"], 0, false, false, s(settings::done));
    // XMLBrowser-based menus
    t.add(
        &["browselibrary", "items", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(browse::browselibrary),
    );
    t.add(
        &["browselibrary", "playlist", "_method"],
        1,
        true,
        true,
        asyncf!(browse::browselibrary),
    );
    t.add(
        &["globalsearch", "items", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(browse::globalsearch),
    );
    t.add(
        &["globalsearch", "playlist", "_method"],
        1,
        true,
        true,
        asyncf!(browse::globalsearch),
    );
    for q in info::INFO_QUERIES {
        t.add(
            &[q, "items", "_index", "_quantity"],
            0,
            true,
            true,
            asyncf!(info::info_query),
        );
        t.add(
            &[q, "playlist", "_method"],
            1,
            true,
            true,
            asyncf!(info::info_query),
        );
    }
    t.add(
        &["systeminfo", "items", "_index", "_quantity"],
        0,
        true,
        true,
        asyncf!(info::systeminfo),
    );
    radio::register(t);
}

/// `contextmenu <start> <count> menu:<kind> …` → `<kind>info items <start> <count> …`
/// (`Slim::Control::Queries::contextMenuQuery`).
pub async fn contextmenu(server: Arc<Server>, mut r: Request) -> Request {
    let Some(menu) = r.param_nonempty("menu").map(str::to_string) else {
        r.bad_params();
        return r;
    };
    let mut tokens = vec![format!("{}info", menu), "items".to_string()];
    tokens.push(r.param("_index").unwrap_or("0").to_string());
    tokens.push(r.param("_quantity").unwrap_or("0").to_string());
    for (k, v) in r.tagged_params() {
        tokens.push(format!("{}:{}", k, v));
    }
    let inner = super::dispatch::execute_tokens(&server, r.client, tokens, r.source.clone()).await;
    r.results = inner.results;
    r.status = if inner.status.is_error() {
        inner.status
    } else {
        super::request::Status::Done
    };
    r
}

/// `favorites items … menu:…` (Jive form), called from the favorites plugin.
pub fn favorites_menu(server: &Arc<Server>, r: &mut Request) {
    favorites::items(server, r);
}

/// Background task: push `menustatus` updates like `Jive.pm` does when players connect or
/// their settings change (`playerPower`, `repeatSettings`, `shuffleSettings`, …).
pub async fn run(server: Arc<Server>) {
    let mut rx = server.notifier.subscribe();
    loop {
        let n = match rx.recv().await {
            Ok(n) => n,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        if n.first() == "menustatus" {
            continue;
        }
        let player = n.player.and_then(|m| server.player(&m));
        match (n.first(), n.second()) {
            ("client", "new") | ("client", "reconnect") => {
                if let Some(p) = player {
                    let items = menu::main_menu(&server, Some(&p));
                    menu::notify_menu(&server, Some(&p), items, "add");
                }
            }
            ("playlist", "repeat") | ("playlist", "shuffle") => {
                if let Some(p) = player {
                    let item = if n.second() == "repeat" {
                        menu::repeat_settings(&p)
                    } else {
                        menu::shuffle_settings(&p)
                    };
                    menu::notify_menu(&server, Some(&p), vec![item], "add");
                }
            }
            ("power", _) => {
                if let Some(p) = player {
                    menu::notify_menu(&server, Some(&p), menu::player_power(&p), "add");
                }
            }
            ("name", _) => {
                if let Some(p) = player {
                    let mut items = menu::player_settings_menu(&server, Some(&p));
                    items.extend(menu::player_power(&p));
                    menu::notify_menu(&server, Some(&p), items, "add");
                }
            }
            ("sync", _) => {
                for p in server.connected_players() {
                    menu::notify_menu(&server, Some(&p), menu::player_power(&p), "add");
                }
            }
            ("rescan", "done") | ("library", "changed") => {
                for p in server.connected_players() {
                    menu::notify_menu(
                        &server,
                        Some(&p),
                        menu::my_music_menu(&server, Some(&p)),
                        "add",
                    );
                }
            }
            _ => {}
        }
    }
}

// ---- shared helpers -------------------------------------------------------------------------

/// The request's (connected) player.
pub(crate) fn player(server: &Arc<Server>, r: &Request) -> Option<Arc<Player>> {
    r.client.and_then(|c| server.player(&c))
}

/// `sliceAndShip`: `count`, `offset` and the requested slice of a menu as `item_loop`.
pub(crate) fn slice_and_ship(r: &mut Request, menu: Vec<Value>) {
    let count = menu.len();
    r.add_result("count", count);
    let (valid, start, end) = normalize(r.param_i64("_index"), r.param_i64("_quantity"), count);
    if valid {
        r.add_result("offset", start);
        for v in menu.into_iter().take(end + 1).skip(start) {
            if let Value::Object(m) = v {
                r.add_loop_item("item_loop", m);
            }
        }
    }
    r.set_done();
}

/// Run another request internally on behalf of `r`'s client.
pub(crate) async fn exec(server: &Arc<Server>, r: &Request, tokens: Vec<String>) -> Request {
    super::dispatch::execute_tokens(server, r.client, tokens, Source::Internal).await
}

/// `cliQuery`'s fix-up: SqueezePlay sometimes sends tagged params in the `_index`/`_quantity`
/// slots (`nextWindow` actions without paging).
pub(crate) fn fix_index_quantity(r: &mut Request) {
    for (key, default) in [("_index", "0"), ("_quantity", "200")] {
        if let Some(v) = r.param(key).map(str::to_string) {
            if let Some((k, val)) = v.split_once(':') {
                r.add_param(k, val);
                r.add_param(key, default);
            }
        }
    }
}

/// Recent searches (`Slim::Control::Jive::cacheSearch`), newest first.
fn recent_searches() -> &'static Mutex<Vec<Value>> {
    static RECENT: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();
    RECENT.get_or_init(|| Mutex::new(Vec::new()))
}

/// `cacheSearch`: remember a search and, on the first one, add the Recent Searches items.
pub(crate) fn cache_search(server: &Arc<Server>, p: Option<&Arc<Player>>, item: Value) {
    let first = {
        let mut list = recent_searches().lock();
        list.insert(0, item);
        list.truncate(200);
        list.len() == 1
    };
    if first {
        if let Some(p) = p {
            menu::notify_menu(server, Some(p), menu::recent_search_menu(), "add");
        }
    }
}

pub(crate) fn has_recent_searches() -> bool {
    !recent_searches().lock().is_empty()
}

/// `jiverecentsearches`.
fn recent_searches_query(_: &Arc<Server>, r: &mut Request) {
    let list = recent_searches().lock().clone();
    if list.is_empty() {
        return xmlbrowser::no_results(r);
    }
    r.add_result("count", list.len());
    r.add_result("offset", 0);
    for v in list {
        if let Value::Object(m) = v {
            r.add_loop_item("item_loop", m);
        }
    }
    r.set_done();
}
