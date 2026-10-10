//! The home menu: `menu <start> <count> [direct:1]` and `menustatus` notifications
//! (`Slim::Control::Jive::menuQuery` / `mainMenu` and the item builders it calls).
//!
//! All items are flat; `node` places them in SqueezePlay's home menu tree and `weight`
//! orders them (server items use 10..100, see `Jive.pm` line 257).

use super::xmlbrowser::value_i64;
use crate::control::request::Request;
use crate::player::Player;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

/// `menuQuery`: with `direct:1` (or for a disconnected player) the menu is returned;
/// otherwise it is delivered as a `menustatus … add` notification and the result is empty.
pub fn menu_query(server: &Arc<Server>, r: &mut Request) {
    let player = super::player(server, r);
    let direct = player.is_none() || r.param_bool("direct");
    let items = main_menu(server, player.as_ref());
    if direct {
        r.add_result("count", items.len());
        r.add_result("offset", 0);
        r.add_result("item_loop", Value::Array(items));
    } else {
        notify_menu(server, player.as_ref(), items, "add");
    }
    r.set_done();
}

/// `_notifyJive`: `["menustatus", [items], "add"|"remove", "<playerid>"|"all"]`.
pub fn notify_menu(server: &Server, p: Option<&Arc<Player>>, items: Vec<Value>, action: &str) {
    let id = p.map(|p| p.id.to_string()).unwrap_or_else(|| "all".into());
    server.notifier.notify_json(
        p.map(|p| p.id),
        vec![
            json!("menustatus"),
            Value::Array(items),
            json!(action),
            json!(id),
        ],
    );
}

/// `mainMenu`: the complete flat home menu.
pub fn main_menu(server: &Arc<Server>, p: Option<&Arc<Player>>) -> Vec<Value> {
    let mut menu = vec![
        json!({ "text": s("MY_MUSIC"), "weight": 11, "id": "myMusic", "isANode": 1, "node": "home" }),
        json!({
            "text": s("FAVORITES"), "id": "favorites", "node": "home", "weight": 100,
            "actions": { "go": { "cmd": ["favorites", "items"], "params": { "menu": "favorites" } } },
        }),
    ];
    menu.extend(super::radio::plugin_menus(server));
    menu.push(crate::plugins::spotify::menu::home_menu());
    if let Some(p) = p {
        menu.extend(player_power(p));
    }
    menu.extend(player_settings_menu(server, p));
    menu.extend(internet_radio_menu());
    menu.push(album_sort_settings_item());
    menu.extend(my_music_menu(server, p));
    if super::has_recent_searches() {
        menu.extend(recent_search_menu());
    }
    menu.push(global_search_menu());
    menu
}

/// `playerPower`: "Turn Off <name>" / "Turn On <name>".
pub fn player_power(p: &Arc<Player>) -> Vec<Value> {
    let name = p.name();
    let (text, action) = if p.power() {
        (s("JIVE_TURN_PLAYER_OFF").replace("%s", &name), 0)
    } else {
        (s("JIVE_TURN_PLAYER_ON").replace("%s", &name), 1)
    };
    vec![json!({
        "text": text, "id": "playerpower", "node": "home", "weight": 100,
        "actions": { "do": { "player": 0, "cmd": ["power", action] } },
    })]
}

/// Choice item for repeat/shuffle (`repeatSettings` / `shuffleSettings`).
fn choice_item(
    text: &str,
    id: &str,
    weight: i64,
    tokens: &[&str],
    cmd: &str,
    selected: i64,
) -> Value {
    let strings: Vec<String> = tokens.iter().map(|t| ucfirst(&s(t))).collect();
    let choices: Vec<Value> = (0..tokens.len())
        .map(|i| json!({ "player": 0, "cmd": ["playlist", cmd, i.to_string()] }))
        .collect();
    json!({
        "text": s(text), "id": id, "node": "settings", "weight": weight,
        "choiceStrings": strings,
        "selectedIndex": selected + 1,
        "actions": { "do": { "choices": choices } },
    })
}

fn ucfirst(t: &str) -> String {
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn playlist_modes(p: Option<&Arc<Player>>) -> (i64, i64) {
    match p {
        Some(p) => {
            let c = p.controller();
            let c = c.lock();
            (c.playlist.repeat as i64, c.playlist.shuffle as i64)
        }
        None => (0, 0),
    }
}

pub fn repeat_settings(p: &Arc<Player>) -> Value {
    choice_item(
        "REPEAT",
        "settingsRepeat",
        20,
        &["OFF", "SONG", "PLAYLIST_SHORT"],
        "repeat",
        playlist_modes(Some(p)).0,
    )
}

pub fn shuffle_settings(p: &Arc<Player>) -> Value {
    choice_item(
        "SHUFFLE",
        "settingsShuffle",
        10,
        &["OFF", "SONG", "ALBUM"],
        "shuffle",
        playlist_modes(Some(p)).1,
    )
}

/// What the player hardware supports (subset of `Slim::Player::*` capabilities).
struct PlayerKind {
    is_boom: bool,
    is_sb2_or_later: bool,
    has_digital_out: bool,
}

fn player_kind(p: &Arc<Player>) -> PlayerKind {
    let st = p.state.lock();
    let model = st.caps.model.to_lowercase();
    PlayerKind {
        is_boom: model == "boom",
        is_sb2_or_later: !matches!(model.as_str(), "slimp3" | "squeezebox"),
        has_digital_out: st.caps.has_digital_out,
    }
}

fn go(cmd: &[&str]) -> Value {
    json!({ "go": { "cmd": cmd, "player": 0 } })
}

/// `playerSettingsMenu`: the player's entries under `settings` / `settingsAudio` /
/// `advancedSettings` / `squeezeboxDisplaySettings`.
pub fn player_settings_menu(_server: &Arc<Server>, p: Option<&Arc<Player>>) -> Vec<Value> {
    let (repeat, shuffle) = playlist_modes(p);
    let mut m = vec![
        json!({ "text": s("AUDIO_SETTINGS"), "id": "settingsAudio", "node": "settings", "isANode": 1, "weight": 35 }),
        choice_item(
            "REPEAT",
            "settingsRepeat",
            20,
            &["OFF", "SONG", "PLAYLIST_SHORT"],
            "repeat",
            repeat,
        ),
        choice_item(
            "SHUFFLE",
            "settingsShuffle",
            10,
            &["OFF", "SONG", "ALBUM"],
            "shuffle",
            shuffle,
        ),
    ];
    let kind = p.map(player_kind);
    if p.is_some() {
        m.push(json!({ "text": s("ALARM"), "id": "settingsAlarm", "node": "settings", "weight": 29, "actions": go(&["alarmsettings"]) }));
    }
    if let Some(k) = &kind {
        if k.is_boom {
            m.push(json!({ "text": s("BASS"), "id": "settingsBass", "node": "settingsAudio", "iconStyle": "hm_settingsAudio", "weight": 10,
                "actions": { "go": { "cmd": ["jivetonesettings"], "player": 0, "params": { "cmd": "bass" } } } }));
        }
        if k.has_digital_out {
            m.push(json!({ "text": s("FIXED_VOLUME"), "id": "settingsFixedVolume", "iconStyle": "hm_settingsAudio", "node": "settingsAudio", "weight": 100, "actions": go(&["jivefixedvolumesettings"]) }));
        }
        if k.is_boom {
            m.push(json!({ "text": s("TREBLE"), "id": "settingsTreble", "node": "settingsAudio", "iconStyle": "hm_settingsAudio", "weight": 20,
                "actions": { "go": { "cmd": ["jivetonesettings"], "player": 0, "params": { "cmd": "treble" } } } }));
            m.push(json!({ "text": s("STEREOXL"), "id": "settingsStereoXL", "node": "settingsAudio", "iconStyle": "hm_settingsAudio", "weight": 90, "actions": go(&["jivestereoxl"]) }));
            m.push(json!({ "text": s("SETUP_ANALOGOUTMODE"), "id": "settingsLineOut", "node": "settingsAudio", "iconStyle": "hm_settingsAudio", "weight": 80, "actions": go(&["jivelineout"]) }));
        }
    }
    // brightness and text size for players with graphic displays
    if p.is_some_and(|p| crate::display::display_width(&p.state.lock().caps.model).is_some()) {
        m.push(json!({ "stringToken": "JIVE_PLAYER_DISPLAY_SETTINGS", "text": s("JIVE_PLAYER_DISPLAY_SETTINGS"), "id": "squeezeboxDisplaySettings",
            "iconStyle": "hm_advancedSettings", "isANode": 1, "node": "advancedSettings" }));
        m.push(json!({ "text": s("PLAYER_BRIGHTNESS"), "id": "settingsPlayerBrightness", "iconStyle": "hm_settingsBrightness",
            "node": "squeezeboxDisplaySettings", "actions": go(&["jiveplayerbrightnesssettings"]) }));
        m.push(json!({ "text": s("TEXTSIZE"), "id": "settingsPlayerTextsize", "node": "squeezeboxDisplaySettings",
            "iconStyle": "hm_advancedSettings", "actions": go(&["jiveplayertextsettings", "activeFont"]) }));
        m.push(json!({ "text": s("OFFDISPLAYSIZE"), "id": "settingsPlayerOffTextsize", "node": "squeezeboxDisplaySettings",
            "iconStyle": "hm_advancedSettings", "actions": go(&["jiveplayertextsettings", "idleFont"]) }));
    }
    m.push(json!({ "text": s("SLEEP"), "id": "settingsSleep", "node": "settings", "weight": 65, "actions": go(&["sleepsettings"]) }));
    m.push(json!({ "text": s("SYNCHRONIZE"), "id": "settingsSync", "node": "settings", "weight": 70, "actions": go(&["syncsettings"]) }));
    m.push(json!({
        "text": s("JIVE_SQUEEZEBOX_INFORMATION"), "id": "settingsInformation", "node": "advancedSettings", "weight": 100,
        "actions": { "go": { "cmd": ["systeminfo", "items"], "params": { "menu": 1 } } },
    }));
    m.push(json!({
        "text": s("INFORMATION_PLAYER_NAME"), "id": "settingsPlayerNameChange", "node": "settings", "weight": 67,
        "input": {
            "initialText": p.map(|p| p.name()).unwrap_or_default(), "len": 1,
            "allowedChars": s("JIVE_ALLOWEDCHARS_WITHCAPS"),
            "help": { "text": s("JIVE_CHANGEPLAYERNAME_HELP") },
            "softbutton1": s("INSERT"), "softbutton2": s("DELETE"),
        },
        "actions": { "do": { "cmd": ["name"], "player": 0, "params": { "playername": "__INPUT__" } } },
    }));
    if let Some(k) = &kind {
        if k.is_sb2_or_later {
            m.push(json!({ "text": s("SETUP_TRANSITIONTYPE"), "id": "settingsXfade", "iconStyle": "hm_settingsAudio", "node": "settingsAudio", "weight": 30, "actions": go(&["crossfadesettings"]) }));
            m.push(json!({ "text": s("REPLAYGAIN"), "id": "settingsReplayGain", "iconStyle": "hm_settingsAudio", "node": "settingsAudio", "weight": 40, "actions": go(&["replaygainsettings"]) }));
        }
    }
    m
}

/// `internetRadioMenu`: the Radio home item (shown when `radios` has entries).
fn internet_radio_menu() -> Vec<Value> {
    if super::radio::providers().is_empty() {
        return vec![];
    }
    vec![json!({
        "text": s("RADIO"), "id": "radios", "node": "home", "weight": 20,
        "actions": { "go": { "cmd": ["radios"], "params": { "menu": "radio" } } },
        "window": { "menuStyle": "album" },
    })]
}

/// `albumSortSettingsItem`.
fn album_sort_settings_item() -> Value {
    json!({
        "text": s("ALBUMS_SORT_METHOD"), "id": "settingsAlbumSettings", "node": "advancedSettings", "iconStyle": "hm_advancedSettings", "weight": 105,
        "actions": { "go": { "cmd": ["jivealbumsortsettings"], "params": { "menu": "radio" } } },
    })
}

/// A BrowseLibrary node (`_registerBaseNodes`).
struct Node {
    id: &'static str,
    name: &'static str,
    home_menu_text: Option<&'static str>,
    params: &'static [(&'static str, &'static str)],
    jive_icon: Option<&'static str>,
    weight: i64,
}

const NODES: &[Node] = &[
    Node {
        id: "myMusicArtists",
        name: "BROWSE_BY_ARTIST",
        home_menu_text: Some("BROWSE_ARTISTS"),
        params: &[("mode", "artists")],
        jive_icon: None,
        weight: 10,
    },
    Node {
        id: "myMusicArtistsAlbumArtists",
        name: "BROWSE_BY_ALBUMARTIST",
        home_menu_text: Some("BROWSE_ALBUMARTISTS"),
        params: &[("mode", "artists"), ("role_id", "ALBUMARTIST")],
        jive_icon: Some("html/images/artists.png"),
        weight: 9,
    },
    Node {
        id: "myMusicArtistsAllArtists",
        name: "BROWSE_BY_ALL_ARTISTS",
        home_menu_text: Some("ALL_ARTISTS"),
        params: &[("mode", "artists")],
        jive_icon: Some("html/images/artists.png"),
        weight: 11,
    },
    Node {
        id: "myMusicAlbums",
        name: "BROWSE_BY_ALBUM",
        home_menu_text: Some("BROWSE_ALBUMS"),
        params: &[("mode", "albums")],
        jive_icon: None,
        weight: 20,
    },
    Node {
        id: "myMusicGenres",
        name: "BROWSE_BY_GENRE",
        home_menu_text: Some("BROWSE_GENRES"),
        params: &[("mode", "genres")],
        jive_icon: None,
        weight: 30,
    },
    Node {
        id: "myMusicWorks",
        name: "BROWSE_BY_WORK",
        home_menu_text: Some("BROWSE_WORKS"),
        params: &[("mode", "works")],
        jive_icon: Some("html/images/works.png"),
        weight: 35,
    },
    Node {
        id: "myMusicYears",
        name: "BROWSE_BY_YEAR",
        home_menu_text: Some("BROWSE_YEARS"),
        params: &[("mode", "years")],
        jive_icon: None,
        weight: 40,
    },
    Node {
        id: "myMusicNewMusic",
        name: "BROWSE_NEW_MUSIC",
        home_menu_text: Some("BROWSE_NEW_MUSIC"),
        params: &[("mode", "albums"), ("sort", "new"), ("wantMetadata", "1")],
        jive_icon: None,
        weight: 50,
    },
    Node {
        id: "myMusicMusicFolder",
        name: "BROWSE_MUSIC_FOLDER",
        home_menu_text: Some("BROWSE_MUSIC_FOLDER"),
        params: &[("mode", "bmf")],
        jive_icon: None,
        weight: 70,
    },
    Node {
        id: "myMusicPlaylists",
        name: "SAVED_PLAYLISTS",
        home_menu_text: Some("SAVED_PLAYLISTS"),
        params: &[("mode", "playlists")],
        jive_icon: None,
        weight: 80,
    },
    Node {
        id: "myMusicSearch",
        name: "SEARCH",
        home_menu_text: None,
        params: &[("mode", "search")],
        jive_icon: None,
        weight: 90,
    },
];

/// Total for a library query (`<query> 0 1 …` → `count`).
fn library_count(server: &Server, name: rms_library::LibQueryName) -> u64 {
    let q = rms_library::LibQuery {
        name,
        start: Some(0),
        quantity: Some(1),
        params: Default::default(),
    };
    server
        .library
        .query(&q)
        .ok()
        .and_then(|r| r.get("count").and_then(value_i64))
        .unwrap_or(0)
        .max(0) as u64
}

/// Whether a My Music node is shown (`_conditionWrapper` / `isEnabledNode` + node conditions).
fn node_enabled(server: &Server, p: Option<&Arc<Player>>, id: &str) -> bool {
    if let Some(p) = p {
        if p.prefs
            .get(&format!("disabled_{}", id))
            .map(|v| super::xmlbrowser::value_bool(&v))
            .unwrap_or(false)
        {
            return false;
        }
    }
    let unified = server.prefs.server.get_bool("useUnifiedArtistsList");
    match id {
        "myMusicArtists" => unified,
        "myMusicArtistsAlbumArtists" | "myMusicArtistsAllArtists" => !unified,
        "myMusicWorks" => library_count(server, rms_library::LibQueryName::Works) > 0,
        "myMusicMusicFolder" => !server.prefs.server.get_string_list("mediadirs").is_empty(),
        "myMusicPlaylists" => {
            !server.prefs.server.get_str("playlistdir").is_empty()
                || library_count(server, rms_library::LibQueryName::Playlists) > 0
        }
        _ => true,
    }
}

/// `myMusicMenu` → `BrowseLibrary::getJiveMenu($client, 'myMusic')`.
pub fn my_music_menu(server: &Arc<Server>, p: Option<&Arc<Player>>) -> Vec<Value> {
    NODES
        .iter()
        .filter(|n| node_enabled(server, p, n.id))
        .map(my_music_item)
        .collect()
}

fn my_music_item(n: &Node) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("menu".into(), json!(1));
    for (k, v) in n.params {
        // numeric flags (`wantMetadata => 1`) are numbers in the Perl hash
        let v = v
            .parse::<i64>()
            .map(|i| json!(i))
            .unwrap_or_else(|_| json!(v));
        params.insert(k.to_string(), v);
    }
    let mut item = json!({
        "text": s(n.name), "id": n.id, "node": "myMusic", "weight": n.weight,
        "actions": { "go": { "cmd": ["browselibrary", "items"], "params": params } },
    });
    if let Some(h) = n.home_menu_text {
        item["homeMenuText"] = json!(s(h));
    }
    if let Some(i) = n.jive_icon {
        item["icon"] = json!(i);
    }
    item
}

/// `recentSearchMenu`.
pub fn recent_search_menu() -> Vec<Value> {
    vec![
        json!({
            "text": s("RECENT_SEARCHES"), "id": "homeSearchRecent", "node": "home", "weight": 111,
            "actions": { "go": { "cmd": ["jiverecentsearches"] } }, "window": { "text": s("RECENT_SEARCHES") },
        }),
        json!({
            "text": s("RECENT_SEARCHES"), "id": "myMusicSearchRecent", "node": "myMusicSearch", "noCustom": 1, "weight": 50,
            "actions": { "go": { "cmd": ["jiverecentsearches"] } }, "window": { "text": s("RECENT_SEARCHES") },
        }),
    ]
}

/// `globalSearchMenu`.
fn global_search_menu() -> Value {
    json!({
        "text": s("SEARCH"), "homeMenuText": s("SEARCH"), "id": "globalSearch", "node": "home", "weight": 22,
        "input": { "len": 1, "processingPopup": { "text": s("SEARCHING") }, "help": { "text": s("JIVE_SEARCHFOR_HELP") } },
        "actions": { "go": { "cmd": ["globalsearch", "items"], "params": { "menu": "globalsearch", "search": "__TAGGEDINPUT__" } } },
        "window": { "text": s("SEARCH") },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_items() {
        let v = choice_item(
            "REPEAT",
            "settingsRepeat",
            20,
            &["OFF", "SONG", "PLAYLIST_SHORT"],
            "repeat",
            2,
        );
        assert_eq!(v["choiceStrings"], json!(["Off", "Song", "Playlist"]));
        assert_eq!(v["selectedIndex"], 3);
        assert_eq!(
            v["actions"]["do"]["choices"][1],
            json!({ "player": 0, "cmd": ["playlist", "repeat", "1"] })
        );
        assert_eq!(v["node"], "settings");
    }

    #[test]
    fn my_music_items() {
        let albums = NODES.iter().find(|n| n.id == "myMusicAlbums").unwrap();
        let v = my_music_item(albums);
        assert_eq!(v["text"], "Albums");
        assert_eq!(v["homeMenuText"], "Browse Albums");
        assert_eq!(
            v["actions"]["go"]["params"],
            json!({ "menu": 1, "mode": "albums" })
        );
        let newm = NODES.iter().find(|n| n.id == "myMusicNewMusic").unwrap();
        assert_eq!(
            my_music_item(newm)["actions"]["go"]["params"],
            json!({ "menu": 1, "mode": "albums", "sort": "new", "wantMetadata": 1 })
        );
        let search = NODES.iter().find(|n| n.id == "myMusicSearch").unwrap();
        assert!(my_music_item(search).get("homeMenuText").is_none());
    }

    #[test]
    fn global_search_item() {
        let v = global_search_menu();
        assert_eq!(v["id"], "globalSearch");
        assert_eq!(v["actions"]["go"]["params"]["search"], "__TAGGEDINPUT__");
        assert_eq!(v["weight"], 22);
    }
}
