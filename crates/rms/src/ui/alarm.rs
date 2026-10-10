//! Alarm clock settings on the player (`Slim::Buttons::Alarm`): the list of alarms with
//! "All Alarms" on/off, "Add Alarm" and the alarm volume; for each alarm its on/off
//! switch, time (`INPUT.Time`), days, sound (playlist), shuffle, repeat and removal.
//! Changes are saved through the `alarm` command as they are made.

use super::{List, Mode, Transition, Ui};
use crate::control::jive::settings::{alarm_display, alarms_of};
use crate::strings::s;
use serde_json::{json, Value};

/// Alarm sound choices of one category: (category, [(title, url)], singleton).
type PlaylistGroup = (String, Vec<(String, Option<String>)>, bool);

/// The alarm being looked at (a new alarm has no id until its time is set).
#[derive(Debug, Clone)]
pub(super) struct AlarmDraft {
    id: Option<String>,
    time: Option<u32>,
    /// 0 = Sunday … 6 = Saturday
    days: [bool; 7],
    enabled: bool,
    repeat: bool,
    url: Option<String>,
    shufflemode: u8,
}

impl AlarmDraft {
    fn new() -> AlarmDraft {
        AlarmDraft {
            id: None,
            time: None,
            days: [true; 7],
            enabled: false,
            repeat: true,
            url: None,
            shufflemode: 0,
        }
    }
}

/// `checkBoxOverlay` / `radioButtonOverlay`.
fn check(on: bool) -> &'static str {
    if on {
        "\x0f"
    } else {
        "\x0e"
    }
}

fn radio(on: bool) -> &'static str {
    if on {
        "\x0d"
    } else {
        "\x0c"
    }
}

fn on_off(on: bool) -> String {
    s(if on { "ALARM_ON" } else { "ALARM_OFF" })
}

/// A menu item of the alarm screens: `act` is what right does.
fn item(text: String, act: &str, overlay: Option<String>) -> Value {
    let mut v = json!({ "text": text, "rms": format!("alarm:{act}") });
    if let Some(o) = overlay {
        v["rms_overlay"] = json!(o);
    }
    v
}

impl Ui {
    /// The alarm screens' items for a menu id (`buildTopMenu`, `@alarmMenu`, …).
    async fn alarm_menu(&self, id: &str) -> Vec<Value> {
        let p = &self.p;
        let d = self.alarm.clone().unwrap_or_else(AlarmDraft::new);
        let arrow = || None;
        match id {
            "top" => {
                let alarms = alarms_of(p);
                let mut items: Vec<Value> = alarms
                    .iter()
                    .enumerate()
                    .map(|(n, a)| {
                        let mut it = item(
                            format!("{} {} ({})", s("ALARM_ALARM"), n + 1, alarm_display(a)),
                            "open",
                            arrow(),
                        );
                        it["alarm"] = json!(a.id);
                        it["header"] = json!(format!("{} {}", s("ALARM_ALARM"), n + 1));
                        it
                    })
                    .collect();
                items.push(item(s("ALARM_ADD"), "add", arrow()));
                let fixed = p.prefs.exists("digitalVolumeControl")
                    && p.prefs.get_i64("digitalVolumeControl") == 0;
                if !fixed {
                    items.push(item(s("ALARM_VOLUME"), "volume", arrow()));
                }
                let all = item(
                    s("ALARM_ALL_ALARMS"),
                    "all",
                    Some(on_off(p.prefs.get_i64("alarmsEnabled") != 0)),
                );
                if alarms.is_empty() {
                    items.push(all);
                } else {
                    items.insert(0, all);
                }
                items
            }
            "alarm" | "add" => {
                let mut items = vec![
                    item(s("ALARM_ALARM"), "enabled", Some(on_off(d.enabled))),
                    item(s("ALARM_SET_TIME"), "time", arrow()),
                    item(s("ALARM_SET_DAYS"), "menu:days", arrow()),
                    item(s("ALARM_SELECT_PLAYLIST"), "menu:sound", arrow()),
                    item(s("SHUFFLE"), "menu:shuffle", arrow()),
                    item(
                        s("ALARM_ALARM_REPEAT"),
                        "repeat",
                        Some(check(d.repeat).into()),
                    ),
                    item(s("ALARM_DELETE"), "menu:delete", arrow()),
                ];
                if id == "add" {
                    // a new alarm starts with its time and has nothing to delete
                    items.pop();
                    let first = items.remove(0);
                    items.push(first);
                }
                items
            }
            "days" => {
                let mut items = vec![item(
                    s("ALARM_EVERY_DAY"),
                    "day:all",
                    Some(check(d.days.iter().all(|x| *x)).into()),
                )];
                for day in [1, 2, 3, 4, 5, 6, 0] {
                    items.push(item(
                        s(&format!("ALARM_DAY{day}")),
                        &format!("day:{day}"),
                        Some(check(d.days[day]).into()),
                    ));
                }
                items
            }
            "shuffle" => ["SHUFFLE_OFF", "SHUFFLE_ON_SONGS", "SHUFFLE_ON_ALBUMS"]
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    item(
                        s(t),
                        &format!("shuffle:{i}"),
                        Some(check(d.shufflemode as usize == i).into()),
                    )
                })
                .collect(),
            "delete" => vec![
                item(s("CANCEL"), "cancel", None),
                item(s("ALARM_DELETE"), "delete", None),
            ],
            other => {
                // "sound" or "sound:<category>" (`buildPlaylistMenu`)
                let groups = self.alarm_playlists().await;
                let radio_item = |title: &str, url: &Option<String>| {
                    let mut it = item(
                        title.to_string(),
                        "sound",
                        Some(radio(*url == d.url).into()),
                    );
                    it["url"] = json!(url);
                    it
                };
                match other.strip_prefix("sound:") {
                    Some(cat) => groups
                        .iter()
                        .find(|(c, ..)| c == cat)
                        .map(|(_, list, _)| list.iter().map(|(t, u)| radio_item(t, u)).collect())
                        .unwrap_or_default(),
                    None => groups
                        .iter()
                        .map(|(cat, list, single)| {
                            if list.len() == 1 && *single {
                                radio_item(&list[0].0, &list[0].1)
                            } else {
                                item(cat.clone(), &format!("menu:sound:{cat}"), arrow())
                            }
                        })
                        .collect(),
                }
            }
        }
    }

    /// `Slim::Utils::Alarm->getPlaylists`: (category, [(title, url)], singleton).
    async fn alarm_playlists(&self) -> Vec<PlaylistGroup> {
        let res = self
            .request(
                ["alarm", "playlists", "0", "500"]
                    .iter()
                    .map(|w| w.to_string())
                    .collect(),
            )
            .await;
        let mut groups: Vec<PlaylistGroup> = Vec::new();
        for it in res
            .get("item_loop")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let cat = it
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let title = it
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let url = it.get("url").and_then(Value::as_str).map(str::to_string);
            let single = it
                .get("singleton")
                .map(|v| v == "1" || v == 1)
                .unwrap_or(false);
            match groups.iter_mut().find(|g| g.0 == cat) {
                Some(g) => g.1.push((title, url)),
                None => groups.push((cat, vec![(title, url)], single)),
            }
        }
        groups
    }

    async fn alarm_list(&self, id: &str, header: String) -> Mode {
        let mut l = List::fixed("settings", header, self.alarm_menu(id).await, None);
        l.menu_id = Some(id.to_string());
        Mode::List(l)
    }

    /// The alarm settings entry (Settings → Alarm Clock).
    pub(super) async fn alarm_open(&mut self) {
        self.alarm = None;
        let m = self.alarm_list("top", s("ALARM")).await;
        self.push(m);
    }

    /// Rebuild every alarm screen on the stack after a change, keeping positions.
    async fn alarm_refresh(&mut self) {
        let ids: Vec<(usize, String)> = self
            .stack
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m {
                Mode::List(l) => l.menu_id.clone().map(|id| (i, id)),
                _ => None,
            })
            .collect();
        let current = self.alarm.as_ref().and_then(|a| a.id.clone());
        for (i, id) in ids {
            let items = self.alarm_menu(&id).await;
            if let Some(Mode::List(l)) = self.stack.get_mut(i) {
                l.items = items.into_iter().map(Some).collect();
                l.index = l.index.min(l.items.len().saturating_sub(1));
                // `buildTopMenu($client, $alarm->id)`: the saved alarm is selected
                if id == "top" {
                    if let Some(pos) = l.items.iter().position(|it| {
                        it.as_ref()
                            .and_then(|it| it.get("alarm"))
                            .and_then(Value::as_str)
                            == current.as_deref()
                    }) {
                        l.index = pos;
                    }
                }
            }
        }
    }

    /// `saveAlarm`: alarms are saved once they have a time.
    async fn alarm_save(&mut self) {
        let Some(d) = self.alarm.clone() else { return };
        let Some(time) = d.time else { return };
        let dow = (0..7)
            .filter(|i| d.days[*i])
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut t: Vec<String> = vec!["alarm".into()];
        match &d.id {
            Some(id) => {
                t.push("update".into());
                t.push(format!("id:{id}"));
            }
            None => t.push("add".into()),
        }
        t.extend([
            format!("time:{time}"),
            format!("dow:{dow}"),
            format!("enabled:{}", d.enabled as i32),
            format!("repeat:{}", d.repeat as i32),
            format!("shufflemode:{}", d.shufflemode),
            format!("url:{}", d.url.clone().unwrap_or_else(|| "0".into())),
        ]);
        let res = self.request(t).await;
        if d.id.is_none() {
            if let (Some(a), Some(id)) =
                (self.alarm.as_mut(), res.get("id").and_then(Value::as_str))
            {
                a.id = Some(id.to_string());
            }
        }
    }

    /// The time editor closed on an alarm (`setAlarmTime`).
    pub(super) async fn alarm_time_saved(&mut self, time: u32) {
        let Some(a) = self.alarm.as_mut() else { return };
        a.time = Some(time);
        a.enabled = true;
        self.show_briefly(vec![s("ALARM_SAVING")], 1.0);
        self.alarm_save().await;
        self.alarm_refresh().await;
    }

    /// Right on an alarm screen item (`exitRightHandler`).
    pub(super) async fn alarm_select(&mut self, item: &Value, act: &str) {
        let title = super::menu::text(item);
        match act {
            "open" => {
                let id = item.get("alarm").and_then(Value::as_str).unwrap_or("");
                self.alarm = alarms_of(&self.p)
                    .into_iter()
                    .find(|a| a.id == id)
                    .map(|a| AlarmDraft {
                        id: Some(a.id),
                        time: Some(a.time),
                        days: a.days,
                        enabled: a.enabled,
                        repeat: a.repeat,
                        url: a.url,
                        shufflemode: a.shufflemode,
                    });
                let header = item
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or(&title)
                    .to_string();
                let m = self.alarm_list("alarm", header).await;
                self.push(m);
            }
            "add" => {
                self.alarm = Some(AlarmDraft::new());
                let m = self.alarm_list("add", title).await;
                self.push(m);
            }
            "volume" => {
                let v = self.p.prefs.get_i64("alarmDefaultVolume");
                self.push(Mode::Bar(super::Bar {
                    header: title,
                    value: v,
                    min: 0,
                    max: 100,
                    action: json!({ "cmd": ["alarm", "defaultvolume"], "params": { "valtag": "volume" } }),
                }));
            }
            "all" => {
                let on = self.p.prefs.get_i64("alarmsEnabled") != 0;
                self.cmd(&["alarm", if on { "disableall" } else { "enableall" }])
                    .await;
                self.alarm_refresh().await;
            }
            "time" => {
                let value = self.alarm.as_ref().and_then(|a| a.time).unwrap_or(43200);
                let fmt = self.server.prefs.server.get_str("timeFormat");
                self.push(Mode::Time(super::time::TimeInput::new(title, value, &fmt)));
            }
            "enabled" | "repeat" => {
                if let Some(a) = self.alarm.as_mut() {
                    if act == "enabled" {
                        a.enabled = !a.enabled;
                    } else {
                        a.repeat = !a.repeat;
                    }
                }
                self.alarm_save().await;
                self.alarm_refresh().await;
            }
            "sound" => {
                let url = item.get("url").and_then(Value::as_str).map(str::to_string);
                if let Some(a) = self.alarm.as_mut() {
                    a.url = url;
                }
                self.alarm_save().await;
                self.alarm_refresh().await;
            }
            "cancel" => self.pop(),
            "delete" => {
                if let Some(id) = self.alarm.as_ref().and_then(|a| a.id.clone()) {
                    self.cmd(&["alarm", "delete", &format!("id:{id}")]).await;
                }
                self.alarm = None;
                self.show_briefly(vec![s("ALARM_DELETING")], 1.0);
                // back to the list of alarms
                while self.stack.len() > 1
                    && !matches!(self.stack.last(), Some(Mode::List(l)) if l.menu_id.as_deref() == Some("top"))
                {
                    self.stack.pop();
                }
                self.transition = Some(Transition::PushRight);
                self.alarm_refresh().await;
            }
            other => {
                if let Some(menu) = other.strip_prefix("menu:") {
                    let m = self.alarm_list(menu, title).await;
                    self.push(m);
                } else if let Some(day) = other.strip_prefix("day:") {
                    if let Some(a) = self.alarm.as_mut() {
                        match day.parse::<usize>() {
                            Ok(d) if d < 7 => a.days[d] = !a.days[d],
                            _ => {
                                let all = a.days.iter().all(|x| *x);
                                a.days = [!all; 7];
                            }
                        }
                    }
                    self.alarm_save().await;
                    self.alarm_refresh().await;
                } else if let Some(mode) = other.strip_prefix("shuffle:") {
                    if let Some(a) = self.alarm.as_mut() {
                        a.shufflemode = mode.parse().unwrap_or(0);
                    }
                    self.alarm_save().await;
                    self.alarm_refresh().await;
                } else {
                    self.transition = Some(Transition::BumpRight);
                }
            }
        }
    }
}
