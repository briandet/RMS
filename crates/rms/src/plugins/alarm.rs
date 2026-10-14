//! Alarm clock (`Slim::Utils::Alarm`): alarms are stored per player in the `alarms` pref.

use crate::control::request::{normalize, Request, Results};
use crate::player::Player;
use crate::server::Server;
use chrono::{Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Alarm {
    pub id: String,
    /// seconds after midnight
    pub time: u32,
    /// days 0 (Sunday) .. 6
    pub days: [bool; 7],
    pub enabled: bool,
    pub repeat: bool,
    pub volume: Option<i32>,
    /// playlist URL; None = current playlist
    pub url: Option<String>,
    pub shufflemode: u8,
    #[serde(default)]
    pub created: f64,
    /// epoch day on which this alarm last fired (to fire once per day)
    #[serde(default)]
    pub last_fired_day: Option<i64>,
}

fn load(p: &Player) -> BTreeMap<String, Alarm> {
    p.prefs
        .get("alarms")
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

fn store(p: &Player, alarms: &BTreeMap<String, Alarm>) {
    p.prefs
        .set("alarms", serde_json::to_value(alarms).unwrap_or(json!({})));
}

fn dow_string(a: &Alarm) -> String {
    (0..7)
        .filter(|&d| a.days[d])
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// `alarm add|update|delete|enableall|disableall|defaultvolume`.  Returns the alarm id.
pub fn command(
    _server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: &str,
    r: &Request,
) -> Result<Option<String>, ()> {
    let mut alarms = load(p);
    let apply = |a: &mut Alarm, r: &Request| -> Result<(), ()> {
        if let Some(t) = r.param("time") {
            a.time = t.parse::<f64>().map_err(|_| ())? as u32;
        }
        if let Some(d) = r.param("dow") {
            if !regex::Regex::new(r"^[0-6](?:,[0-6])*$")
                .unwrap()
                .is_match(d)
                && !d.is_empty()
            {
                return Err(());
            }
            for i in 0..7 {
                a.days[i] = d.contains(&i.to_string());
            }
        }
        if let Some(d) = r.param_i64("dowAdd") {
            a.days[(d.clamp(0, 6)) as usize] = true;
        }
        if let Some(d) = r.param_i64("dowDel") {
            a.days[(d.clamp(0, 6)) as usize] = false;
        }
        if let Some(e) = r.param("enabled") {
            a.enabled = e != "0";
        }
        if let Some(e) = r.param("repeat") {
            a.repeat = e != "0";
        }
        if let Some(v) = r.param_i64("volume") {
            a.volume = Some(v.clamp(0, 100) as i32);
        }
        if let Some(s) = r.param_i64("shufflemode") {
            a.shufflemode = s.clamp(0, 2) as u8;
        }
        if let Some(u) = r.param("url").or(r.param("playlisturl")) {
            a.url = if u == "0" || u.is_empty() {
                None
            } else {
                Some(u.to_string())
            };
        }
        Ok(())
    };
    match cmd {
        "add" => {
            if r.param("time")
                .and_then(|t| t.parse::<f64>().ok())
                .is_none()
            {
                return Err(());
            }
            let id = format!("{:08x}", rand::random::<u32>());
            let mut a = Alarm {
                id: id.clone(),
                time: 0,
                days: [true; 7],
                enabled: true,
                repeat: true,
                volume: None,
                url: None,
                shufflemode: 0,
                created: crate::util::now(),
                last_fired_day: None,
            };
            apply(&mut a, r)?;
            alarms.insert(id.clone(), a);
            store(p, &alarms);
            Ok(Some(id))
        }
        "update" => {
            let id = r.param("id").ok_or(())?.to_string();
            let a = alarms.get_mut(&id).ok_or(())?;
            apply(a, r)?;
            store(p, &alarms);
            Ok(Some(id))
        }
        "delete" => {
            let id = r.param("id").ok_or(())?.to_string();
            alarms.remove(&id);
            store(p, &alarms);
            Ok(Some(id))
        }
        "enableall" | "disableall" => {
            p.prefs
                .set("alarmsEnabled", json!((cmd == "enableall") as i32));
            Ok(None)
        }
        "defaultvolume" => {
            let v = r.param_i64("volume").ok_or(())?;
            p.prefs.set("alarmDefaultVolume", json!(v));
            Ok(None)
        }
        _ => Err(()),
    }
}

pub fn alarms_query(p: &Arc<Player>, r: &mut Request) {
    let alarms = load(p);
    let filter = r.param("filter").unwrap_or("enabled").to_string();
    let dow = r.param_i64("dow");
    let mut list: Vec<&Alarm> = alarms.values().collect();
    list.sort_by(|a, b| {
        a.created
            .partial_cmp(&b.created)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let list: Vec<&Alarm> = list
        .into_iter()
        .filter(|a| match dow {
            Some(d) => a.days[d.clamp(0, 6) as usize],
            None => filter == "all" || (filter == "enabled" && a.enabled),
        })
        .collect();
    r.add_result("fade", p.prefs.get_i64("alarmfadeseconds"));
    r.add_result("count", list.len());
    let (valid, start, end) =
        normalize(r.param_i64("_index"), r.param_i64("_quantity"), list.len());
    if valid {
        for a in &list[start..=end] {
            let mut i = Results::new();
            i.insert("id".into(), json!(a.id));
            i.insert("dow".into(), json!(dow_string(a)));
            i.insert("enabled".into(), json!(a.enabled as i32));
            i.insert("repeat".into(), json!(a.repeat as i32));
            i.insert("shufflemode".into(), json!(a.shufflemode));
            i.insert("time".into(), json!(a.time));
            i.insert(
                "volume".into(),
                json!(a
                    .volume
                    .unwrap_or(p.prefs.get_i64("alarmDefaultVolume") as i32)),
            );
            i.insert(
                "url".into(),
                json!(a.url.clone().unwrap_or_else(|| "CURRENT_PLAYLIST".into())),
            );
            r.add_loop_item("alarms_loop", i);
        }
    }
}

/// Next due time (epoch) of an alarm after `now`.
fn next_due(a: &Alarm, now: chrono::DateTime<Local>) -> Option<i64> {
    for add in 0..8 {
        let day = now.date_naive() + chrono::Duration::days(add);
        let dow = day.weekday().num_days_from_sunday() as usize;
        if !a.days[dow] {
            continue;
        }
        let t = day.and_hms_opt(a.time / 3600 % 24, a.time / 60 % 60, a.time % 60)?;
        let t = t.and_local_timezone(Local).single()?;
        if t > now {
            return Some(t.timestamp());
        }
    }
    None
}

/// `alarm_state` / `alarm_next` for status.
pub fn status_info(p: &Arc<Player>) -> (&'static str, i64) {
    if p.prefs.get_i64("alarmsEnabled") == 0 {
        return ("none", 0);
    }
    let now = Local::now();
    let next = load(p)
        .values()
        .filter(|a| a.enabled)
        .filter_map(|a| next_due(a, now))
        .min();
    match next {
        Some(t) if t - now.timestamp() < 86400 => ("set", t),
        _ => ("none", 0),
    }
}

/// Background scheduler: fire alarms at their time.
pub async fn run(server: Arc<Server>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tick.tick().await;
        let now = Local::now();
        let secs = now.num_seconds_from_midnight();
        let today = now.date_naive().num_days_from_ce() as i64;
        let dow = now.weekday().num_days_from_sunday() as usize;
        for p in server.connected_players() {
            if p.prefs.get_i64("alarmsEnabled") == 0 {
                continue;
            }
            let mut alarms = load(&p);
            let mut changed = false;
            for a in alarms.values_mut() {
                if !a.enabled || !a.days[dow] || a.last_fired_day == Some(today) {
                    continue;
                }
                if secs >= a.time && secs < a.time + 60 {
                    a.last_fired_day = Some(today);
                    if !a.repeat {
                        a.enabled = false;
                    }
                    changed = true;
                    fire(&server, &p, a.clone());
                }
            }
            if changed {
                store(&p, &alarms);
            }
        }
    }
}

fn fire(server: &Arc<Server>, p: &Arc<Player>, a: Alarm) {
    info!("{}: alarm {} sounding", p.id, a.id);
    let server = server.clone();
    let p = p.clone();
    crate::util::spawn(async move {
        use crate::control::dispatch::execute_tokens;
        use crate::control::request::Source;
        let id = Some(p.id);
        execute_tokens(
            &server,
            id,
            vec!["power".into(), "1".into()],
            Source::Internal,
        )
        .await;
        let vol = a
            .volume
            .unwrap_or(p.prefs.get_i64("alarmDefaultVolume") as i32);
        p.set_volume(vol, false);
        if a.shufflemode > 0 {
            execute_tokens(
                &server,
                id,
                vec![
                    "playlist".into(),
                    "shuffle".into(),
                    a.shufflemode.to_string(),
                ],
                Source::Internal,
            )
            .await;
        }
        match &a.url {
            Some(u) => {
                execute_tokens(
                    &server,
                    id,
                    vec!["playlist".into(), "play".into(), u.clone()],
                    Source::Internal,
                )
                .await;
            }
            None => {
                execute_tokens(&server, id, vec!["play".into()], Source::Internal).await;
            }
        }
        server.notifier.notify(id, &["alarm", "sound", &a.id]);
        // stop after the alarm timeout
        let timeout = p.prefs.get_i64("alarmTimeoutSeconds");
        if timeout > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(timeout as u64)).await;
            execute_tokens(&server, id, vec!["stop".into()], Source::Internal).await;
            server.notifier.notify(id, &["alarm", "end", &a.id]);
        }
    });
}
