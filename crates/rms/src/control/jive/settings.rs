//! Player settings sub-menus and small Jive commands (`Slim::Control::Jive`):
//! alarms, sync, sleep, audio settings, album sort, `date`, `firmwareupgrade`, …
//! Menus are returned through [`super::slice_and_ship`] (`count`, `offset`, `item_loop`).

use super::{player, slice_and_ship};
use crate::control::request::Request;
use crate::player::Player;
use crate::plugins::alarm::Alarm;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// `jiveblankcommand` / `jivedummycommand`: no-ops used as "Cancel".
pub fn done(_: &Arc<Server>, r: &mut Request) {
    r.set_done();
}

fn ucfirst(t: &str) -> String {
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---- alarms ----------------------------------------------------------------------------------

pub(crate) fn alarms_of(p: &Player) -> Vec<Alarm> {
    let map: BTreeMap<String, Alarm> = p
        .prefs
        .get("alarms")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut list: Vec<Alarm> = map.into_values().collect();
    list.sort_by(|a, b| {
        a.time.cmp(&b.time).then(
            a.created
                .partial_cmp(&b.created)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    list
}

/// `secsToPrettyTime` with the default `timeFormat` (`|%I:%M %p`).
pub fn pretty_time(secs: u32) -> String {
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let h12 = if h.is_multiple_of(12) { 12 } else { h % 12 };
    format!("{}:{:02} {}", h12, m, if h < 12 { "AM" } else { "PM" })
}

/// `Slim::Utils::Alarm::displayStr`.
pub(crate) fn alarm_display(a: &Alarm) -> String {
    let mut out = pretty_time(a.time);
    if !a.days.iter().all(|d| *d) {
        for day in [1, 2, 3, 4, 5, 6, 0] {
            if a.days[day] {
                out.push(' ');
                out.push_str(&s(&format!("ALARM_SHORT_DAY_{}", day)));
            }
        }
    }
    if !a.enabled {
        out = format!("{} ({})", s("ALARM_OFF"), out);
    }
    out
}

fn find_alarm(p: &Player, id: &str) -> Option<Alarm> {
    alarms_of(p).into_iter().find(|a| a.id == id)
}

/// `alarmSettingsQuery`.
pub fn alarm_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let enabled = p.prefs.get_i64("alarmsEnabled");
    let mut menu = vec![json!({
        "text": s("ALARM_ALL_ALARMS"),
        "choiceStrings": [ucfirst(&s("OFF")), ucfirst(&s("ON"))],
        "selectedIndex": enabled + 1,
        "actions": { "do": { "choices": [
            { "player": 0, "cmd": ["alarm", "disableall"] },
            { "player": 0, "cmd": ["alarm", "enableall"] },
        ] } },
    })];
    for (n, a) in alarms_of(&p).iter().enumerate() {
        let days: Vec<String> = (0..7)
            .filter(|d| a.days[*d])
            .map(|d| d.to_string())
            .collect();
        menu.push(json!({
            "text": format!("{} {}: {}", s("ALARM_ALARM"), n + 1, alarm_display(a)),
            "actions": { "go": { "cmd": ["jiveupdatealarm"], "params": {
                "id": a.id, "enabled": a.enabled as i32, "days": days.join(","), "time": a.time, "playlist": a.url.clone().unwrap_or_else(|| "0".into()),
            }, "player": 0 } },
        }));
    }
    menu.push(json!({
        "text": s("ALARM_ADD"),
        "input": { "initialText": 25200, "title": s("ALARM_ADD"), "_inputStyle": "time", "len": 1, "help": { "text": s("JIVE_ALARMSET_HELP") } },
        "actions": { "do": { "player": 0, "cmd": ["alarm", "add"], "params": { "time": "__TAGGEDINPUT__", "enabled": 1 } } },
        "nextWindow": "refresh",
    }));
    if p.prefs.get_i64("digitalVolumeControl") != 0 {
        menu.push(json!({ "text": s("ALARM_VOLUME"), "actions": { "go": { "player": 0, "cmd": ["jivealarmvolume"] } } }));
    }
    let fade = (p.prefs.get_i64("alarmfadeseconds") != 0) as i32;
    menu.push(json!({
        "text": s("ALARM_FADE"), "checkbox": fade,
        "actions": {
            "on": { "player": 0, "cmd": ["jivealarm"], "params": { "fadein": 1 } },
            "off": { "player": 0, "cmd": ["jivealarm"], "params": { "fadein": 0 } },
        },
    }));
    slice_and_ship(r, menu);
}

/// `alarmUpdateMenu` (`jiveupdatealarm id:…`).
pub fn alarm_update_menu(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(id) = r.param_nonempty("id").map(str::to_string) else {
        return r.bad_params();
    };
    let a = find_alarm(&p, &id);
    let enabled = a
        .as_ref()
        .map(|a| a.enabled as i32)
        .or(r.param_i64("enabled").map(|e| e as i32))
        .unwrap_or(0);
    let time = a
        .as_ref()
        .map(|a| a.time as i64)
        .or(r.param_i64("time"))
        .unwrap_or(0);
    let mode = a.as_ref().map(|a| a.shufflemode).unwrap_or(0);
    let repeat = a.as_ref().map(|a| a.repeat).unwrap_or(true);
    let upd = |extra: Value| {
        let mut p = json!({ "id": id });
        if let (Value::Object(m), Value::Object(e)) = (&mut p, extra) {
            m.extend(e);
        }
        p
    };
    let shuffle_item = |token: &str, m: u8| {
        json!({ "text": s(token), "radio": (mode == m) as i32, "onClick": "refreshOrigin",
            "actions": { "do": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "shufflemode": m })) } }, "nextWindow": "refresh" })
    };
    let menu = vec![
        json!({ "text": s("ALARM_ALARM_ENABLED"), "checkbox": enabled, "onClick": "refreshOrigin", "actions": {
            "on": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "enabled": 1 })) },
            "off": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "enabled": 0 })) },
        } }),
        json!({ "text": s("ALARM_SET_TIME"),
            "input": { "initialText": time, "title": s("ALARM_SET_TIME"), "_inputStyle": "time", "len": 1, "help": { "text": s("JIVE_ALARMSET_HELP") } },
            "actions": { "do": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "time": "__TAGGEDINPUT__" })) } }, "nextWindow": "parent" }),
        json!({ "text": s("ALARM_SET_DAYS"), "actions": { "go": { "player": 0, "cmd": ["jiveupdatealarmdays"], "params": { "id": id } } } }),
        json!({ "text": s("ALARM_SELECT_PLAYLIST"), "actions": { "go": { "player": 0, "cmd": ["alarm", "playlists"], "params": { "id": id, "menu": 1 } } } }),
        json!({ "text": s("SHUFFLE"), "count": 3, "offset": 0, "item_loop": [
            shuffle_item("SHUFFLE_OFF", 0), shuffle_item("SHUFFLE_ON_SONGS", 1), shuffle_item("SHUFFLE_ON_ALBUMS", 2),
        ] }),
        json!({ "text": s("ALARM_ALARM_REPEAT"), "radio": repeat as i32, "onClick": "refreshOrigin",
            "actions": { "do": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "repeat": 1 })) } } }),
        json!({ "text": s("ALARM_ALARM_ONETIME"), "radio": (!repeat) as i32, "onClick": "refreshOrigin",
            "actions": { "do": { "player": 0, "cmd": ["alarm", "update"], "params": upd(json!({ "repeat": 0 })) } } }),
        json!({ "text": s("ALARM_DELETE"), "count": 2, "offset": 0, "item_loop": [
            { "text": s("CANCEL"), "actions": { "go": { "player": 0, "cmd": ["jiveblankcommand"] } }, "nextWindow": "parent" },
            { "text": s("ALARM_DELETE"), "actions": { "go": { "player": 0, "cmd": ["alarm", "delete"], "params": { "id": id } } }, "nextWindow": "grandparent" },
        ] }),
    ];
    slice_and_ship(r, menu);
}

/// `alarmUpdateDays` (`jiveupdatealarmdays id:…`).
pub fn alarm_update_days(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let Some(id) = r.param_nonempty("id").map(str::to_string) else {
        return r.bad_params();
    };
    let days = find_alarm(&p, &id).map(|a| a.days).unwrap_or([true; 7]);
    let menu = (0..7)
        .map(|d| {
            json!({ "text": s(&format!("ALARM_DAY{}", d)), "checkbox": days[d] as i32, "onClick": "refreshGrandparent", "actions": {
                "on": { "player": 0, "cmd": ["alarm", "update"], "params": { "id": id, "dowAdd": d } },
                "off": { "player": 0, "cmd": ["alarm", "update"], "params": { "id": id, "dowDel": d } },
            } })
        })
        .collect();
    slice_and_ship(r, menu);
}

/// `jiveAlarmVolumeSlider` (`jivealarmvolume`).
pub fn alarm_volume(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let vol = match p.prefs.get_i64("alarmDefaultVolume") {
        0 => 50,
        v => v,
    };
    r.add_result("offset", 0);
    r.add_result("count", 1);
    r.add_result(
        "item_loop",
        json!([{ "slider": 1, "min": 1, "max": 100, "sliderIcons": "volume", "initial": vol,
            "actions": { "do": { "player": 0, "cmd": ["alarm", "defaultvolume"], "params": { "valtag": "volume" } } } }]),
    );
    r.set_done();
}

/// `jiveAlarmCommand`: `fadein:N` sets `alarmfadeseconds`.  `snooze:1` pauses playback for
/// `alarmSnoozeSeconds`; `stop:1` pauses it unless `continueAudio:1` (the alarm plugin has
/// no sounding-alarm state of its own, so this acts on playback directly).
pub fn jive_alarm(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    if let Some(f) = r.param("fadein") {
        p.prefs
            .set("alarmfadeseconds", json!(f.parse::<i64>().unwrap_or(0)));
    }
    let snooze = r.param_bool("snooze");
    let stop = r.param_bool("stop") && !r.param_bool("continueAudio");
    if snooze || stop {
        let server = server.clone();
        let client = r.client;
        let snooze_secs = p.prefs.get_i64("alarmSnoozeSeconds").max(1) as u64;
        tokio::spawn(async move {
            let pause = |v: &str| vec!["pause".to_string(), v.to_string()];
            crate::control::dispatch::execute_tokens(
                &server,
                client,
                pause("1"),
                Default::default(),
            )
            .await;
            if snooze {
                tokio::time::sleep(std::time::Duration::from_secs(snooze_secs)).await;
                crate::control::dispatch::execute_tokens(
                    &server,
                    client,
                    pause("0"),
                    Default::default(),
                )
                .await;
            }
        });
    }
    r.set_done();
}

// ---- sync ------------------------------------------------------------------------------------

/// Names of the players in `p`'s sync group other than `p` (`syncedWithNames(0)`).
fn synced_with(p: &Arc<Player>) -> Vec<Arc<Player>> {
    p.controller()
        .lock()
        .all_players()
        .into_iter()
        .filter(|o| o.id != p.id)
        .collect()
}

/// `syncSettingsQuery` / `getPlayersToSyncWith`.
pub fn sync_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let others: Vec<Arc<Player>> = server
        .connected_players()
        .into_iter()
        .filter(|o| o.id != p.id)
        .collect();
    if others.is_empty() {
        r.add_result("window", json!({ "textarea": s("SYNC_ABOUT") }));
        r.add_result("count", 0);
        return r.set_done();
    }
    let mut menu =
        vec![json!({ "text": s("SYNC_X_TO").replace("%s", &p.name()), "style": "itemNoAction" })];
    let mates = synced_with(&p);
    let names = |list: &[Arc<Player>]| {
        list.iter()
            .map(|x| x.name())
            .collect::<Vec<_>>()
            .join(" & ")
    };
    let currently = if mates.is_empty() {
        String::new()
    } else {
        names(&mates)
    };
    // (id, name, isSyncedWith)
    let mut list: Vec<(String, String, bool)> = Vec::new();
    if !mates.is_empty() {
        list.push((p.id.to_string(), currently.clone(), true));
    }
    let mut seen_groups: Vec<String> = Vec::new();
    for o in &others {
        if mates.iter().any(|m| m.id == o.id) {
            continue;
        }
        let group = o.controller().lock().all_players();
        if group.len() > 1 {
            let master = o.controller().lock().master_id;
            if master == o.id && !seen_groups.contains(&o.id.to_string()) {
                seen_groups.push(o.id.to_string());
                list.push((o.id.to_string(), names(&group), false));
            }
        } else {
            list.push((o.id.to_string(), o.name(), false));
        }
    }
    list.sort_by(|a, b| a.1.cmp(&b.1));
    let unsync: Value = if currently.is_empty() {
        json!(0)
    } else {
        json!(currently)
    };
    for (id, name, synced) in list {
        menu.push(json!({ "text": name, "radio": synced as i32,
            "actions": { "do": { "player": 0, "cmd": ["jivesync"], "params": { "syncWith": id, "syncWithString": name, "unsyncWith": unsync } } },
            "nextWindow": "refresh" }));
    }
    if !mates.is_empty() {
        menu.push(json!({ "text": s("DO_NOT_SYNC"), "radio": 0,
            "actions": { "do": { "player": 0, "cmd": ["jivesync"], "params": { "syncWith": 0, "syncWithString": 0, "unsyncWith": unsync } } },
            "nextWindow": "refresh" }));
    }
    slice_and_ship(r, menu);
}

/// `jiveSyncCommand`: unsync (if `unsyncWith`), then sync this player to `syncWith`.
pub fn jive_sync(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let mut messages = Vec::new();
    if let Some(u) = r.param_nonempty("unsyncWith").filter(|u| *u != "0") {
        server.unsync(&p);
        messages.push(s("UNSYNCING_FROM").replace("%s", u));
    }
    if let Some(w) = r.param_nonempty("syncWith").filter(|w| *w != "0") {
        if let Some(other) = server.resolve_client(w) {
            if other.id != p.id {
                server.sync(&other, &p, false);
                server
                    .notifier
                    .notify(Some(other.id), &["sync", &p.id.to_string()]);
            } else {
                // our own group was selected again: nothing to do
            }
            messages.push(s("SYNCING_WITH").replace("%s", r.param("syncWithString").unwrap_or("")));
        }
    }
    if !messages.is_empty() {
        crate::control::commands::show_briefly(server, &p, vec![messages.join("\n")], 3.0);
    }
    r.set_done();
}

// ---- sleep -----------------------------------------------------------------------------------

fn sleep_in_x(minutes: i64) -> Value {
    let text = if minutes == 0 {
        s("SLEEP_CANCEL")
    } else {
        s("X_MINUTES").replace("%s", &minutes.to_string())
    };
    json!({ "text": text, "actions": { "go": { "player": 0, "cmd": ["sleep", minutes * 60] } }, "nextWindow": "refresh", "setSelectedIndex": "1" })
}

/// `sleepSettingsQuery`.
pub fn sleep_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let sleep_time = p.state.lock().sleep_time;
    let now = crate::util::now();
    let mut menu = Vec::new();
    if sleep_time > now {
        let mins = ((sleep_time - now) / 60.0) as i64 + 1;
        menu.push(json!({ "text": s("SLEEPING_IN_X_MINUTES").replace("%s", &mins.to_string()), "style": "itemNoAction" }));
        menu.push(sleep_in_x(0));
    }
    let playing_with_duration = {
        let c = p.controller();
        let c = c.lock();
        c.is_playing(false) && c.playing_song_duration().unwrap_or(0.0) > 0.0
    };
    if playing_with_duration {
        menu.push(json!({ "text": s("SLEEP_AT_END_OF_SONG"), "actions": { "go": { "player": 0, "cmd": ["jiveendoftracksleep"] } }, "nextWindow": "refresh", "setSelectedIndex": 1 }));
    }
    for m in [15, 30, 45, 60, 90] {
        menu.push(sleep_in_x(m));
    }
    slice_and_ship(r, menu);
}

/// `endOfTrackSleepCommand`: sleep when the current song ends.
pub fn end_of_track_sleep(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let remaining = {
        let c = p.controller();
        let c = c.lock();
        if c.is_playing(false) {
            c.playing_song_duration()
                .map(|d| (d - c.playing_song_elapsed()).max(0.0))
        } else {
            None
        }
    };
    match remaining {
        Some(rem) => {
            p.state.lock().sleep_time = crate::util::now() + rem;
            server
                .notifier
                .notify(Some(p.id), &["sleep", &format!("{}", rem)]);
        }
        None => crate::control::commands::show_briefly(
            server,
            &p,
            vec![s("NOTHING_CURRENTLY_PLAYING")],
            3.0,
        ),
    }
    r.set_done();
}

// ---- audio settings --------------------------------------------------------------------------

/// A list of radio items setting `playerpref <pref> <i>`.
fn pref_radios(p: &Player, pref: &str, tokens: &[&str], numeric_value: bool) -> Vec<Value> {
    let current = p.prefs.get_i64(pref);
    tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let v: Value = if numeric_value { json!(i) } else { json!(i.to_string()) };
            json!({ "text": s(t), "radio": (current == i as i64) as i32, "actions": { "do": { "player": 0, "cmd": ["playerpref", pref, v] } } })
        })
        .collect()
}

/// `crossfadeSettingsQuery`.
pub fn crossfade_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let menu = pref_radios(
        &p,
        "transitionType",
        &[
            "TRANSITION_NONE",
            "TRANSITION_CROSSFADE",
            "TRANSITION_FADE_IN",
            "TRANSITION_FADE_OUT",
            "TRANSITION_FADE_IN_OUT",
        ],
        false,
    );
    slice_and_ship(r, menu);
}

/// `replaygainSettingsQuery`.
pub fn replaygain_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let menu = pref_radios(
        &p,
        "replayGainMode",
        &[
            "REPLAYGAIN_DISABLED",
            "REPLAYGAIN_TRACK_GAIN",
            "REPLAYGAIN_ALBUM_GAIN",
            "REPLAYGAIN_SMART_GAIN",
        ],
        false,
    );
    slice_and_ship(r, menu);
}

/// `stereoXLQuery`.
pub fn stereoxl_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let menu = pref_radios(
        &p,
        "stereoxl",
        &["CHOICE_OFF", "LOW", "MEDIUM", "HIGH"],
        true,
    );
    slice_and_ship(r, menu);
}

/// `lineOutQuery`.
pub fn lineout_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let menu = pref_radios(
        &p,
        "analogOutMode",
        &[
            "ANALOGOUTMODE_HEADPHONE",
            "ANALOGOUTMODE_SUBOUT",
            "ANALOGOUTMODE_ALWAYS_ON",
            "ANALOGOUTMODE_ALWAYS_OFF",
        ],
        true,
    );
    slice_and_ship(r, menu);
}

/// `fixedVolumeSettingsQuery`.
pub fn fixed_volume_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let fixed =
        p.prefs.exists("digitalVolumeControl") && p.prefs.get_i64("digitalVolumeControl") == 0;
    let menu = vec![
        json!({ "text": s("FIXED_VOLUME_100"), "checkbox": fixed as i32, "actions": {
        "on": { "player": 0, "cmd": ["playerpref", "digitalVolumeControl", 0] },
        "off": { "player": 0, "cmd": ["playerpref", "digitalVolumeControl", 1] },
    } }),
    ];
    slice_and_ship(r, menu);
}

/// `toneSettingsQuery` (`jivetonesettings cmd:bass|treble`).
pub fn tone_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let tone = r.param("cmd").unwrap_or("bass").to_string();
    if tone != "bass" && tone != "treble" {
        return r.bad_params();
    }
    let initial = {
        let st = p.state.lock();
        if tone == "bass" {
            st.bass
        } else {
            st.treble
        }
    };
    let menu = vec![
        json!({ "slider": 1, "min": -23, "max": 23, "adjust": 24, "initial": initial,
        "actions": { "do": { "player": 0, "cmd": ["playerpref", tone], "params": { "valtag": "value" } } } }),
    ];
    slice_and_ship(r, menu);
}

// ---- player display --------------------------------------------------------------------------

/// A slider setting a player pref (`minAutoBrightness` / `sensAutoBrightness`).
fn pref_slider(text: &str, pref: &str, current: i64, min: i64, max: i64) -> Value {
    json!({ "text": text, "count": 1, "offset": 0, "item_loop": [{
        "slider": 1, "min": min, "max": max, "initial": current,
        "actions": { "do": { "player": 0, "cmd": ["playerpref", pref], "params": { "valtag": "value" } } },
    }] })
}

/// `playerBrightnessMenu` (`jiveplayerbrightnesssettings`): radio lists for the brightness
/// while on, off and idle; Boom adds its automatic brightness sliders.
pub fn player_brightness_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let options = crate::display::brightness_options(&p);
    let mut menu = Vec::new();
    for (token, pref) in [
        ("SETUP_POWERONBRIGHTNESS_ABBR", "powerOnBrightness"),
        ("SETUP_POWEROFFBRIGHTNESS_ABBR", "powerOffBrightness"),
        ("SETUP_IDLEBRIGHTNESS_ABBR", "idleBrightness"),
    ] {
        let current = p.prefs.get_i64(pref);
        let radios: Vec<Value> = options
            .iter()
            .map(|(v, label)| {
                json!({ "text": label, "radio": (current == *v) as i32,
                "actions": { "do": { "cmd": ["playerpref", pref, v], "player": 0 } } })
            })
            .collect();
        menu.push(
            json!({ "text": s(token), "count": radios.len(), "offset": 0, "item_loop": radios }),
        );
    }
    if p.state.lock().caps.model == "boom" {
        menu.push(pref_slider(
            &s("SETUP_MINAUTOBRIGHTNESS"),
            "minAutoBrightness",
            p.prefs.get_i64("minAutoBrightness"),
            1,
            5,
        ));
        menu.push(pref_slider(
            &s("SETUP_SENSAUTOBRIGHTNESS"),
            "sensAutoBrightness",
            p.prefs.get_i64("sensAutoBrightness"),
            1,
            20,
        ));
    }
    slice_and_ship(r, menu);
}

/// `playerTextMenu` (`jiveplayertextsettings activeFont|idleFont`): one radio per font size.
pub fn player_text_settings(server: &Arc<Server>, r: &mut Request) {
    let Some(p) = player(server, r) else {
        return r.bad_dispatch();
    };
    let what = r.param("_whatFont").unwrap_or("activeFont").to_string();
    if what != "activeFont" && what != "idleFont" {
        return r.bad_params();
    }
    let current = p.prefs.get_i64(&format!("{what}_curr"));
    let menu: Vec<Value> = p
        .prefs
        .get_string_list(&what)
        .iter()
        .enumerate()
        .map(|(i, font)| json!({ "text": s(&font.to_uppercase()), "radio": (current == i as i64) as i32,
            "actions": { "do": { "cmd": ["playerpref", format!("{what}_curr"), i], "player": 0 } } }))
        .collect();
    slice_and_ship(r, menu);
}

// ---- album sort ------------------------------------------------------------------------------

/// The `jivealbumsort` server pref (LMS default `album`).
pub fn album_sort(server: &Server) -> String {
    let v = server.prefs.server.get_str("jivealbumsort");
    if v.is_empty() {
        "album".into()
    } else {
        v
    }
}

/// `albumSortSettingsMenu` (`jivealbumsortsettings`).
pub fn album_sort_settings(server: &Arc<Server>, r: &mut Request) {
    let current = album_sort(server);
    let methods = [
        ("album", "ALBUM"),
        ("artflow", "SORT_ARTISTYEARALBUM"),
        ("artistalbum", "SORT_ARTISTALBUM"),
    ];
    r.add_result("count", methods.len());
    r.add_result("offset", 0);
    for (key, token) in methods {
        let item = json!({ "text": s(token), "radio": (current == key) as i32,
            "actions": { "do": { "player": 0, "cmd": ["jivesetalbumsort"], "params": { "sortMe": key } } } });
        if let Value::Object(m) = item {
            r.add_loop_item("item_loop", m);
        }
    }
    r.set_done();
}

/// `jiveSetAlbumSort` (`jivesetalbumsort sortMe:<key>`).
pub fn set_album_sort(server: &Arc<Server>, r: &mut Request) {
    if let Some(sort) = r.param_nonempty("sortMe") {
        server.prefs.server.set("jivealbumsort", json!(sort));
    }
    r.set_done();
}

// ---- misc ------------------------------------------------------------------------------------

/// `jivePlaylistsCommand` (`jiveplaylists delete title:… url:… playlist_id:…`): confirmation menu.
pub fn jive_playlists(server: &Arc<Server>, r: &mut Request) {
    if player(server, r).is_none() {
        return r.bad_dispatch();
    }
    let cmd = r.param("_cmd").unwrap_or("delete").to_string();
    let title = r.param("title").unwrap_or("").to_string();
    let menu = json!([
        { "text": s("CANCEL"), "actions": { "go": { "player": 0, "cmd": ["jiveblankcommand"] } }, "nextWindow": "parent" },
        { "text": format!("{} {}", s(&cmd.to_uppercase()), title), "actions": { "go": { "player": 0, "cmd": ["playlists", "delete"], "params": {
            "playlist_id": r.param("playlist_id"), "title": title, "url": r.param("url"),
        } } }, "nextWindow": "grandparent" },
    ]);
    r.add_result("offset", 0);
    r.add_result("count", 2);
    r.add_result("item_loop", menu);
    r.set_done();
}

/// `dateQuery`: `date_epoch`, `utc_offset_minutes`, `is_dst`, `timezone`, `date`.
/// (`subscribe:` is accepted; the time is pushed again when the query re-runs.)
pub fn date(_: &Arc<Server>, r: &mut Request) {
    use chrono::{Local, Offset, TimeZone};
    let epoch = r
        .param_i64("set")
        .filter(|e| *e > 0)
        .unwrap_or_else(crate::util::now_secs);
    let local = Local
        .timestamp_opt(epoch, 0)
        .single()
        .unwrap_or_else(Local::now);
    let offset_minutes = local.offset().fix().local_minus_utc() / 60;
    r.add_result("date_epoch", epoch);
    r.add_result("utc_offset_minutes", offset_minutes);
    r.add_result("is_dst", 0);
    r.add_result("timezone", std::env::var("TZ").unwrap_or_default());
    r.add_result("date", "0000-00-00T00:00:00+00:00");
    r.set_done();
}

/// `firmwareUpgradeQuery`: no firmware is served, so never an upgrade.
pub fn firmware_upgrade(_: &Arc<Server>, r: &mut Request) {
    r.add_result("firmwareUpgrade", 0);
    r.set_done();
}

/// `extensionsQuery` (`jiveapplets` etc.): no extension repositories.
pub fn extensions(_: &Arc<Server>, r: &mut Request) {
    r.add_result("count", 0);
    r.set_done();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_sleep_items() {
        assert_eq!(pretty_time(25200), "7:00 AM");
        assert_eq!(pretty_time(0), "12:00 AM");
        assert_eq!(pretty_time(13 * 3600 + 5 * 60), "1:05 PM");
        let v = sleep_in_x(15);
        assert_eq!(v["text"], "15 minutes");
        assert_eq!(v["actions"]["go"]["cmd"], json!(["sleep", 900]));
        assert_eq!(sleep_in_x(0)["text"], "Cancel sleep");
    }

    #[test]
    fn alarm_display_strings() {
        let mut a = Alarm {
            id: "x".into(),
            time: 25200,
            days: [true; 7],
            enabled: true,
            repeat: true,
            volume: None,
            url: None,
            shufflemode: 0,
            created: 0.0,
            last_fired_day: None,
        };
        assert_eq!(alarm_display(&a), "7:00 AM");
        a.days = [false, true, false, false, false, false, true];
        a.enabled = false;
        let d = alarm_display(&a);
        assert!(d.starts_with(&s("ALARM_OFF")), "{}", d);
        assert!(d.contains(&s("ALARM_SHORT_DAY_1")));
    }
}
