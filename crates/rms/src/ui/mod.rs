//! The on-player user interface of Squeezeboxes with displays (`Slim::Buttons::*`):
//! IR remote and front panel buttons drive a stack of modes — the home menu, browse lists,
//! the Now Playing playlist, text entry, sliders, volume — drawn on the player's screen
//! with Lyrion's push/bump animations.
//!
//! Buttons are mapped to functions per mode with Lyrion's own `IR/Default.map`.  The menus
//! themselves are the Jive menus the server already provides to Squeezer/SqueezePlay
//! (`menu`, `browselibrary`, `trackinfo`, favorites, radio, settings …), read the way
//! SqueezePlay's SlimBrowser reads them.  Each player has one task that handles its
//! button events in order and publishes the screen to show in [`View`].

mod alarm;
pub mod ir;
pub mod irmap;
pub mod menu;
mod scanner;
pub mod scroll;
mod time;

use crate::control::dispatch::execute_tokens;
use crate::control::request::Source;
use crate::display::Parts;
use crate::player::Player;
use crate::server::Server;
use crate::strings::s;
use irmap::maps;
use menu::Kind;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tracing::debug;

/// Display animations (`pushLeft`, `bumpUp`, …), sent as the `grfe` transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    PushLeft,
    PushRight,
    PushUp,
    PushDown,
    BumpLeft,
    BumpRight,
    BumpUp,
    BumpDown,
}

impl Transition {
    /// `grfe` transition character and whether the parameter is the font extent.
    pub fn code(self) -> (u8, bool) {
        match self {
            Transition::PushLeft => (b'r', false),
            Transition::PushRight => (b'l', false),
            Transition::PushUp => (b'u', true),
            Transition::PushDown => (b'd', true),
            Transition::BumpLeft => (b'L', false),
            Transition::BumpRight => (b'R', false),
            Transition::BumpUp => (b'D', true),
            Transition::BumpDown => (b'U', true),
        }
    }
}

/// What the display should show for the UI.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// Screen of the current mode; `None` shows the player's own screen (Now Playing, or
    /// the clock while off).
    pub parts: Option<Parts>,
    /// Animation for the next frame.
    pub transition: Option<Transition>,
    /// Changes whenever `parts` or `transition` change.
    pub generation: u64,
}

#[derive(Debug)]
pub enum Event {
    /// IR code with the player's timestamp (1 kHz ticks).
    Ir { code: u32, time: u32 },
    /// `button` command: a button or function name.
    Button(String),
    /// Transporter knob position.
    Knob(i32),
}

/// Deliver an event to the player's UI task, starting it if needed.
pub fn send(server: &Arc<Server>, p: &Arc<Player>, ev: Event) {
    let mut tx = p.ui_tx.lock();
    if tx.as_ref().is_none_or(|t| t.is_closed()) {
        let (t, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(server.clone(), p.clone(), rx));
        *tx = Some(t);
    }
    if let Some(t) = tx.as_ref() {
        let _ = t.send(ev);
    }
}

/// Start the UI of a player with a display (so it shows the home menu right away).
pub fn start(server: &Arc<Server>, p: &Arc<Player>) {
    let mut tx = p.ui_tx.lock();
    if tx.as_ref().is_none_or(|t| t.is_closed()) {
        let (t, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(server.clone(), p.clone(), rx));
        *tx = Some(t);
    }
}

// ---- modes ------------------------------------------------------------------------------------

/// Items fetched per request when browsing.
const CHUNK: usize = 100;

#[derive(Debug, Clone)]
struct List {
    /// IR map mode (`home`, `browsemenu`, `settings`, `browsedb`, `trackinfo`).
    map: &'static str,
    header: String,
    /// Loaded items (`None` = not fetched yet).
    items: Vec<Option<Value>>,
    index: usize,
    base: Option<Value>,
    /// Command (without range) to fetch more items, for lists from requests.
    source: Option<Vec<String>>,
    /// Items carry `textkey`s (sorted lists, for number keys).
    sorted: bool,
    /// Show the "n of N" count until then.
    count_until: f64,
    /// Lists built by RMS itself (alarm screens) and rebuilt after changes.
    menu_id: Option<String>,
}

impl List {
    fn fixed(map: &'static str, header: String, items: Vec<Value>, base: Option<Value>) -> List {
        let sorted = !items.is_empty() && items.iter().all(|i| i.get("textkey").is_some());
        List {
            map,
            header,
            items: items.into_iter().map(Some).collect(),
            index: 0,
            base,
            source: None,
            sorted,
            count_until: 0.0,
            menu_id: None,
        }
    }

    fn item(&self) -> Option<&Value> {
        self.items.get(self.index).and_then(Option::as_ref)
    }
}

/// `INPUT.Text`.
#[derive(Debug, Clone)]
struct TextInput {
    header: String,
    item: Value,
    base: Option<Value>,
    upper_only: bool,
    /// Indices into the character list; the last one is the character being edited.
    array: Vec<usize>,
    /// Multi-tap: move on to the next character at this time.
    next_char_at: Option<Instant>,
}

/// `INPUT.Bar` for a slider item.
#[derive(Debug, Clone)]
struct Bar {
    header: String,
    value: i64,
    min: i64,
    max: i64,
    action: Value,
}

#[derive(Debug, Clone)]
enum Mode {
    List(List),
    /// Now Playing / current playlist browser, at a playlist position.
    Playlist(usize),
    Text(TextInput),
    Bar(Bar),
    /// `INPUT.Volume`, left after `timeout` seconds without buttons.
    Volume(f64),
    /// The song scanner (its `INPUT.Bar`).
    Scanner(scanner::Scanner),
    /// `INPUT.Time`.
    Time(time::TimeInput),
}

impl Mode {
    fn map_name(&self) -> &'static str {
        match self {
            Mode::List(l) => l.map,
            Mode::Playlist(_) => "playlist",
            Mode::Text(_) => "INPUT.Text",
            Mode::Bar(_) => "INPUT.Bar",
            Mode::Volume(_) => "INPUT.Volume",
            Mode::Scanner(_) => "INPUT.Bar",
            Mode::Time(_) => "INPUT.Time",
        }
    }
}

/// Screensavers (`Slim::Buttons::ScreenSaver`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Saver {
    /// Now Playing while playing (`screensaver`).
    Playing,
    /// Now Playing or the clock while stopped (`idlesaver`).
    Idle { clock: bool },
}

/// `@UpperChars` / `@BothChars` of `INPUT.Text` (index 0 = the "done" arrow).
const UPPER_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ .,'?!@-_#$%^&(){}[]\\|;:\"<>*=+`/0123456789";
const BOTH_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ .,'?!@-_#$%^&(){}[]\\|;:\"<>*=+`/0123456789";

fn chars(upper: bool) -> Vec<Option<char>> {
    std::iter::once(None)
        .chain(
            (if upper { UPPER_CHARS } else { BOTH_CHARS })
                .chars()
                .map(Some),
        )
        .collect()
}

/// `@numberLetters` (lists, upper case text entry) and `@numberLettersMixed`.
const NUMBER_LETTERS: [&str; 10] = [
    " 0", ".,'?!@-1", "ABC2", "DEF3", "GHI4", "JKL5", "MNO6", "PQRS7", "TUV8", "WXYZ9",
];
const NUMBER_LETTERS_MIXED: [&str; 10] = [
    " 0",
    ".,'?!@-1",
    "abcABC2",
    "defDEF3",
    "ghiGHI4",
    "jklJKL5",
    "mnoMNO6",
    "pqrsPQRS7",
    "tuvTUV8",
    "wxyzWXYZ9",
];

/// Seconds before multi-tap letters / number jumps start over (`displaytexttimeout`).
const DISPLAY_TEXT_TIMEOUT: f64 = 1.0;

// ---- the UI task -----------------------------------------------------------------------------

struct Ui {
    server: Arc<Server>,
    p: Arc<Player>,
    ir: ir::IrState,
    stack: Vec<Mode>,
    nodes: HashMap<String, Vec<Value>>,
    saver: Option<Saver>,
    was_on: bool,
    scroll: scroll::ScrollParams,
    /// numberLetter: (digit, index into its letters, time)
    letter: (u8, usize, f64),
    /// numberScroll in unsorted lists: (next index base, time)
    digit: (usize, f64),
    playing_index: usize,
    transition: Option<Transition>,
    published: Option<Parts>,
    last_knob: Option<i32>,
    /// The alarm open in the alarm screens.
    alarm: Option<alarm::AlarmDraft>,
}

async fn run(server: Arc<Server>, p: Arc<Player>, mut rx: mpsc::UnboundedReceiver<Event>) {
    let mut ui = Ui {
        server,
        was_on: p.power(),
        p,
        ir: ir::IrState::default(),
        stack: Vec::new(),
        nodes: HashMap::new(),
        saver: None,
        scroll: scroll::ScrollParams::default(),
        letter: (255, 0, 0.0),
        digit: (0, 0.0),
        playing_index: 0,
        transition: None,
        published: None,
        last_knob: None,
        alarm: None,
    };
    ui.reset_home().await;
    ui.playing_index = ui.playing_song_index();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let timer = [
            ui.ir.next_timer(),
            ui.text_timer(),
            ui.time_timer(),
            ui.scanner_timer(),
        ]
        .into_iter()
        .flatten()
        .min();
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(ev) => ui.handle(ev).await,
                None => break,
            },
            _ = tokio::time::sleep_until(timer.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600))), if timer.is_some() => {
                ui.fire_timers().await;
            }
            _ = tick.tick() => {
                if ui.server.player(&ui.p.id).is_none() {
                    break;
                }
                ui.periodic().await;
            }
        }
        ui.publish();
    }
}

/// `name_arg` → (`name`, `arg`), as `Slim::Buttons::Common::getFunction` splits them.
fn split_function(f: &str) -> (&str, Option<&str>) {
    match f.split_once('_') {
        Some((n, a)) => (n, Some(a)),
        None => (f, None),
    }
}

impl Ui {
    fn now() -> f64 {
        crate::util::now()
    }

    async fn request(&self, tokens: Vec<String>) -> Map<String, Value> {
        debug!("{} ui: {:?}", self.p.id, tokens);
        execute_tokens(&self.server, Some(self.p.id), tokens, Source::Internal)
            .await
            .results
    }

    async fn cmd(&self, words: &[&str]) {
        self.request(words.iter().map(|w| w.to_string()).collect())
            .await;
    }

    fn show_briefly(&self, lines: Vec<String>, duration: f64) {
        crate::control::commands::show_briefly(&self.server, &self.p, lines, duration);
    }

    fn playing_song_index(&self) -> usize {
        self.p.controller().lock().playing_song_index()
    }

    fn playlist_len(&self) -> usize {
        self.p.controller().lock().playlist.len()
    }

    fn top_map(&self) -> &'static str {
        if !self.p.power() {
            return "off";
        }
        match self.saver {
            Some(Saver::Playing) => "screensaver",
            Some(Saver::Idle { .. }) => "idlesaver",
            None => self.stack.last().map(Mode::map_name).unwrap_or("home"),
        }
    }

    /// `lookupFunction`'s search order: the mode, the mode below an `INPUT.*` mode, the
    /// class of a `class.name` mode.
    fn mode_chain(&self) -> Vec<&'static str> {
        let top = self.top_map();
        let mut chain = vec![top];
        if matches!(self.stack.last(), Some(Mode::Scanner(_))) && self.saver.is_none() {
            chain.push("songscanner");
        } else if top.starts_with("INPUT.") && self.saver.is_none() {
            if let Some(below) = self.stack.iter().rev().nth(1) {
                chain.push(below.map_name());
            }
        }
        chain
    }

    // ---- home menu --------------------------------------------------------------------------

    /// Fetch the Jive menu tree and arrange the home menu like the player's own
    /// (`menuItem` pref order: Now Playing, Browse Music, Radio, Favorites, …, Settings).
    async fn load_nodes(&mut self) {
        let res = self
            .request(vec![
                "menu".into(),
                "0".into(),
                "1000".into(),
                "direct:1".into(),
            ])
            .await;
        let items: Vec<Value> = res
            .get("item_loop")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut nodes: HashMap<String, Vec<Value>> = HashMap::new();
        for it in items {
            let id = it
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // the player's own power button and the Jive-only "settings" duplicates
            if id == "playerpower" {
                continue;
            }
            // the player has its own alarm screens
            let mut it = it;
            if id == "settingsAlarm" {
                it = json!({ "text": it["text"], "id": id, "node": it["node"], "weight": it["weight"], "rms": "alarm" });
            }
            let node = it
                .get("node")
                .and_then(Value::as_str)
                .unwrap_or("home")
                .to_string();
            nodes.entry(node).or_default().push(it);
        }
        let weight = |v: &Value| {
            v.get("weight")
                .and_then(|w| {
                    w.as_f64()
                        .or_else(|| w.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(100.0)
        };
        for list in nodes.values_mut() {
            list.sort_by(|a, b| {
                weight(a)
                    .partial_cmp(&weight(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        // Settings → Advanced
        nodes.entry("settings".into()).or_default().push(json!({
            "text": s("ADVANCED_SETTINGS"), "id": "advancedSettings", "isANode": 1, "weight": 1000,
        }));
        let home_items = nodes.remove("home").unwrap_or_default();
        let by_id = |id: &str| {
            home_items
                .iter()
                .find(|i| i.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
        };
        let mut home = Vec::new();
        let mut used = vec![];
        for entry in self.p.prefs.get_string_list("menuItem") {
            let item = match entry.as_str() {
                "NOW_PLAYING" => Some(json!({ "text": s("NOW_PLAYING"), "rms": "nowplaying" })),
                "BROWSE_MUSIC" => by_id("myMusic"),
                "RADIO" => by_id("radios"),
                "FAVORITES" => by_id("favorites"),
                "PLUGIN_MY_APPS_MODULE_NAME" => by_id("myApps"),
                "ALARM" => Some(json!({ "text": s("ALARM"), "rms": "alarm" })),
                "PLUGIN_APP_GALLERY_MODULE_NAME" => by_id("appGallery"),
                "SETTINGS" => {
                    Some(json!({ "text": s("SETTINGS"), "id": "settings", "isANode": 1 }))
                }
                _ => None,
            };
            if let Some(i) = item {
                if let Some(id) = i.get("id").and_then(Value::as_str) {
                    used.push(id.to_string());
                }
                home.push(i);
            }
        }
        // other home items (Search, …) go before Settings
        let settings_pos = home
            .iter()
            .position(|i| i.get("id").and_then(Value::as_str) == Some("settings"))
            .unwrap_or(home.len());
        let extra: Vec<Value> = home_items
            .iter()
            .filter(|i| {
                !used
                    .iter()
                    .any(|u| Some(u.as_str()) == i.get("id").and_then(Value::as_str))
            })
            .cloned()
            .collect();
        for (n, it) in extra.into_iter().enumerate() {
            home.insert(settings_pos + n, it);
        }
        nodes.insert("home".into(), home);
        self.nodes = nodes;
    }

    fn home_list(&self) -> List {
        List::fixed(
            "home",
            s("HOME"),
            self.nodes.get("home").cloned().unwrap_or_default(),
            None,
        )
    }

    /// `setMode(home)`.
    async fn reset_home(&mut self) {
        // `setMode(home)` clears the whole mode stack, a screensaver included
        self.saver = None;
        self.load_nodes().await;
        self.stack = vec![Mode::List(self.home_list())];
    }

    /// Home → Now Playing (`Home::jump(NOW_PLAYING)` + `pushMode(playlist)`).
    async fn go_now_playing(&mut self) {
        self.reset_home().await;
        if let Some(Mode::List(home)) = self.stack.first_mut() {
            if let Some(i) = home.items.iter().position(|i| {
                i.as_ref()
                    .and_then(|i| i.get("rms"))
                    .and_then(Value::as_str)
                    == Some("nowplaying")
            }) {
                home.index = i;
            }
        }
        let idx = self.playing_song_index();
        self.stack.push(Mode::Playlist(idx));
    }

    fn find_home_item(&self, id: &str) -> Option<Value> {
        self.nodes
            .values()
            .flatten()
            .find(|i| i.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
    }

    // ---- events -----------------------------------------------------------------------------

    async fn handle(&mut self, ev: Event) {
        match ev {
            Event::Ir { code, time } => {
                let mode = self.top_map();
                match self.ir.process(code, time, mode) {
                    Ok(buttons) => {
                        for b in buttons {
                            self.execute_button(&b, true, false).await;
                        }
                    }
                    Err(()) => self.server.notifier.notify_owned(
                        Some(self.p.id),
                        vec!["unknownir".into(), format!("{code:08x}"), time.to_string()],
                    ),
                }
            }
            Event::Button(name) => self.execute_button(&name, false, true).await,
            Event::Knob(pos) => {
                let last = self.last_knob.replace(pos);
                if let Some(last) = last {
                    let b = if pos > last {
                        "knob_right"
                    } else if pos < last {
                        "knob_left"
                    } else {
                        return;
                    };
                    self.execute_button(b, false, false).await;
                }
            }
        }
    }

    async fn fire_timers(&mut self) {
        let mode = self.top_map();
        for b in self.ir.fire_timers(mode) {
            self.execute_button(&b, true, false).await;
        }
        if matches!(self.stack.last(), Some(Mode::Scanner(_))) {
            self.scanner_tick().await;
        }
        self.time_fire().await;
        // multi-tap text entry: move to the next character
        if let Some(Mode::Text(t)) = self.stack.last_mut() {
            if t.next_char_at.is_some_and(|at| at <= Instant::now()) {
                t.next_char_at = None;
                t.array.push(0);
            }
        }
    }

    /// The song scanner's 0.1 s timer.
    fn scanner_timer(&self) -> Option<Instant> {
        matches!(self.stack.last(), Some(Mode::Scanner(_)))
            .then(|| Instant::now() + Duration::from_millis(100))
    }

    fn text_timer(&self) -> Option<Instant> {
        match self.stack.last() {
            Some(Mode::Text(t)) => t.next_char_at,
            _ => None,
        }
    }

    /// `Slim::Hardware::IR::executeButton`.
    async fn execute_button(&mut self, button: &str, from_ir: bool, or_function: bool) {
        let chain = self.mode_chain();
        let func = maps()
            .function(&chain, button)
            .map(str::to_string)
            .or_else(|| or_function.then(|| button.to_string()));
        let Some(func) = func else {
            debug!("{}: button {} not mapped in {:?}", self.p.id, button, chain);
            return;
        };
        if !from_ir {
            self.ir.last_ir_time = 0.0;
        }
        self.ir.last_button = button.to_string();
        if !func.contains("brightness") && func != "dead" {
            self.p.state.lock().last_ir = Self::now();
        }
        debug!("{}: button {} → {}", self.p.id, button, func);
        Box::pin(self.run_function(&func, button)).await;
    }

    /// Re-run the last button in the mode now on top (`resendButton`).
    async fn resend_button(&mut self) {
        let b = self.ir.last_button.clone();
        let b = b
            .trim_end_matches(".down")
            .trim_end_matches(".up")
            .to_string();
        Box::pin(self.execute_button(&b, self.ir.last_ir_time != 0.0, false)).await;
    }

    async fn run_function(&mut self, func: &str, button: &str) {
        let (name, arg) = split_function(func);
        if name == "dead" {
            return;
        }
        // screensavers
        if self.saver.is_some() && self.p.power() {
            match name {
                "done" => {
                    self.saver = None;
                    match arg {
                        Some("passback") => self.resend_button().await,
                        Some("passbackplaylist")
                            if matches!(self.stack.last(), Some(Mode::Playlist(_))) =>
                        {
                            self.resend_button().await
                        }
                        _ => {}
                    }
                    return;
                }
                "playdisp" => return self.playdisp(arg).await,
                _ => {}
            }
        }
        if self.p.power() && self.saver.is_none() && self.mode_function(name, arg, button).await {
            return;
        }
        self.common_function(name, arg, button).await;
    }

    // ---- mode functions ---------------------------------------------------------------------

    /// Functions of the mode on top; false when the mode doesn't handle `name`.
    async fn mode_function(&mut self, name: &str, arg: Option<&str>, button: &str) -> bool {
        let repeat = button.ends_with(".repeat");
        match self.stack.last() {
            Some(Mode::List(_)) => match name {
                "up" | "down" => {
                    let dir: i64 = if name == "up" { -1 } else { 1 };
                    let steps = arg.and_then(|a| a.parse::<i64>().ok()).unwrap_or(1);
                    self.list_move(dir * steps, repeat).await;
                }
                "left" => self.pop(),
                "right" => self.list_right().await,
                "numberScroll" => {
                    self.list_number(arg.and_then(|a| a.parse().ok()).unwrap_or(0))
                        .await
                }
                "play" if arg.is_some() => self.list_play(arg.unwrap_or("0")).await,
                // Home: play and add go right; add on Now Playing clears the playlist
                "play" | "add" if self.top_map() == "home" => {
                    let on_np = self.current_item().is_some_and(|(i, _, _)| {
                        i.get("rms").and_then(Value::as_str) == Some("nowplaying")
                    });
                    if name == "add" && on_np {
                        self.show_briefly(vec![String::new(), s("CLEARING_PLAYLIST")], 1.0);
                        self.cmd(&["playlist", "clear"]).await;
                    } else {
                        self.list_right().await;
                    }
                }
                "create" => {}
                _ => return false,
            },
            Some(Mode::Playlist(_)) => match name {
                "up" | "down" => {
                    let count = self.playlist_len();
                    if count < 2 {
                        if !repeat {
                            self.transition = Some(if name == "up" {
                                Transition::BumpUp
                            } else {
                                Transition::BumpDown
                            });
                        }
                    } else {
                        let dir: i64 = if name == "up" { -1 } else { 1 };
                        let hold = self.ir.hold_time();
                        let Some(Mode::Playlist(idx)) = self.stack.last_mut() else {
                            return true;
                        };
                        let new = self.scroll.scroll(dir, count, *idx, hold);
                        if new != *idx {
                            *idx = new;
                            self.transition = Some(if dir < 0 {
                                Transition::PushUp
                            } else {
                                Transition::PushDown
                            });
                        }
                    }
                }
                "left" => self.pop(),
                "right" => {
                    let Some(Mode::Playlist(idx)) = self.stack.last() else {
                        return true;
                    };
                    let idx = *idx;
                    if self.playlist_len() == 0 {
                        self.transition = Some(Transition::BumpRight);
                    } else {
                        let tokens = vec![
                            "trackinfo".into(),
                            "items".into(),
                            "menu:1".into(),
                            format!("playlist_index:{idx}"),
                        ];
                        self.push_request("trackinfo", String::new(), tokens).await;
                    }
                }
                "numberScroll" => {
                    let count = self.playlist_len();
                    let digit: usize = arg.and_then(|a| a.parse().ok()).unwrap_or(0);
                    let new = self.number_jump(digit, count);
                    if let Some(Mode::Playlist(idx)) = self.stack.last_mut() {
                        *idx = new;
                    }
                }
                "add" => {
                    // removes the browsed song from the playlist
                    let Some(Mode::Playlist(idx)) = self.stack.last() else {
                        return true;
                    };
                    let idx = *idx;
                    if self.playlist_len() > 0 {
                        let title = self.playlist_title(idx);
                        self.cmd(&["playlist", "delete", &idx.to_string()]).await;
                        self.show_briefly(vec![s("REMOVING_FROM_PLAYLIST"), title], 1.0);
                    }
                }
                "zap" => {
                    let Some(Mode::Playlist(idx)) = self.stack.last() else {
                        return true;
                    };
                    let idx = idx.to_string();
                    self.cmd(&["playlist", "zap", &idx]).await;
                }
                "play" => {
                    let Some(Mode::Playlist(idx)) = self.stack.last() else {
                        return true;
                    };
                    let idx = *idx;
                    let showing = idx == self.playing_song_index();
                    let paused = self.p.controller().lock().mode() == "pause";
                    if showing && paused {
                        self.cmd(&["pause"]).await;
                    } else {
                        self.cmd(&["playlist", "jump", &idx.to_string()]).await;
                    }
                }
                "playdisp" => self.playdisp(arg).await,
                _ => return false,
            },
            Some(Mode::Text(_)) => match name {
                "up" | "down" => self.text_change(if name == "up" { -1 } else { 1 }),
                "backspace" => {
                    let Some(Mode::Text(t)) = self.stack.last_mut() else {
                        return true;
                    };
                    t.next_char_at = None;
                    self.letter.2 = 0.0;
                    if t.array.len() <= 1 {
                        self.pop();
                    } else {
                        t.array.pop();
                    }
                }
                "nextChar" => {
                    let Some(Mode::Text(t)) = self.stack.last_mut() else {
                        return true;
                    };
                    t.next_char_at = None;
                    self.letter.2 = 0.0;
                    if t.array.last() == Some(&0) {
                        self.text_submit().await;
                    } else {
                        t.array.push(0);
                    }
                }
                "numberLetter" => {
                    self.text_number_letter(arg.and_then(|a| a.parse().ok()).unwrap_or(0))
                }
                "letter" => {
                    let ch = match arg {
                        Some("space") => ' ',
                        Some(a) => a.chars().next().unwrap_or(' '),
                        None => ' ',
                    };
                    let Some(Mode::Text(t)) = self.stack.last_mut() else {
                        return true;
                    };
                    t.next_char_at = None;
                    if let Some(i) = chars(t.upper_only).iter().position(|c| *c == Some(ch)) {
                        if let Some(last) = t.array.last_mut() {
                            *last = i;
                        }
                        t.array.push(0);
                    }
                }
                "exit" => self.text_submit().await,
                "passback" => self.passback().await,
                _ => return false,
            },
            Some(Mode::Bar(_)) => match name {
                "up" | "down" => {
                    let mut dir: i64 = if name == "up" { 1 } else { -1 };
                    if self.ir.hold_time() > 0.0 {
                        dir *= self.ir.repeat_count(50.0, 8.0);
                    }
                    let Some(Mode::Bar(b)) = self.stack.last_mut() else {
                        return true;
                    };
                    let new = (b.value + dir).clamp(b.min, b.max);
                    if new != b.value {
                        b.value = new;
                        let valtag = b
                            .action
                            .pointer("/params/valtag")
                            .and_then(Value::as_str)
                            .unwrap_or("value")
                            .to_string();
                        let mut t = menu::tokens(&b.action, None, None);
                        t.retain(|w| !w.starts_with("valtag:"));
                        t.push(format!("{valtag}:{new}"));
                        self.request(t).await;
                    }
                }
                "exit" => self.pop(),
                "passback" => self.passback().await,
                _ => return false,
            },
            Some(Mode::Scanner(_)) => return self.scanner_function(name, arg).await,
            Some(Mode::Time(_)) => return self.time_function(name, arg).await,
            Some(Mode::Volume(_)) => match name {
                "up" | "down" => self.volume_step(name == "up").await,
                "exit" => {
                    self.stack.pop();
                    if arg == Some("passback") {
                        self.resend_button().await;
                    }
                }
                _ => return false,
            },
            None => return false,
        }
        true
    }

    /// `passback`: run the button in the mode below.
    async fn passback(&mut self) {
        let below = self
            .stack
            .iter()
            .rev()
            .nth(1)
            .map(Mode::map_name)
            .unwrap_or("home");
        let b = self.ir.last_button.clone();
        if let Some(f) = maps().function(&[below], &b).map(str::to_string) {
            let (name, arg) = split_function(&f);
            let (name, arg) = (name.to_string(), arg.map(str::to_string));
            self.common_function(&name, arg.as_deref(), &b).await;
        }
    }

    fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.transition = Some(Transition::PushRight);
        } else {
            self.transition = Some(Transition::BumpLeft);
        }
    }

    fn push(&mut self, mode: Mode) {
        // a new mode replaces the screensaver
        self.saver = None;
        self.stack.push(mode);
        self.transition = Some(Transition::PushLeft);
    }

    // ---- lists ------------------------------------------------------------------------------

    /// Fetch the chunk of the top list containing `index` if it isn't loaded.
    async fn ensure_loaded(&mut self, index: usize) {
        let Some(Mode::List(l)) = self.stack.last() else {
            return;
        };
        if index >= l.items.len() || l.items[index].is_some() {
            return;
        }
        let Some(source) = l.source.clone() else {
            return;
        };
        let start = index / CHUNK * CHUNK;
        let res = self.fetch(&source, start).await;
        if let Some(Mode::List(l)) = self.stack.last_mut() {
            if let Some(items) = res.get("item_loop").and_then(Value::as_array) {
                for (i, it) in items.iter().enumerate() {
                    if let Some(slot) = l.items.get_mut(start + i) {
                        *slot = Some(it.clone());
                    }
                }
            }
            // never leave a hole: an unexpected short reply ends the list there
            if l.items[index].is_none() {
                l.items.truncate(index);
                l.index = l.index.min(l.items.len().saturating_sub(1));
            }
        }
    }

    async fn fetch(&self, source: &[String], start: usize) -> Map<String, Value> {
        let mut t = source[..source
            .iter()
            .position(|w| w.contains(':'))
            .unwrap_or(source.len())]
            .to_vec();
        t.push(start.to_string());
        t.push(CHUNK.to_string());
        t.extend(source.iter().filter(|w| w.contains(':')).cloned());
        self.request(t).await
    }

    /// Run a `go` request and push its result as a list.
    async fn push_request(&mut self, map: &'static str, header: String, source: Vec<String>) {
        let res = self.fetch(&source, 0).await;
        let count = res
            .get("count")
            .and_then(Value::as_u64)
            .map(|c| c as usize)
            .unwrap_or_else(|| {
                res.get("item_loop")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            });
        let mut items: Vec<Option<Value>> = vec![None; count];
        if let Some(loop_) = res.get("item_loop").and_then(Value::as_array) {
            for (i, it) in loop_.iter().enumerate() {
                if i < items.len() {
                    items[i] = Some(it.clone());
                }
            }
        }
        let header = res
            .get("title")
            .and_then(Value::as_str)
            .or_else(|| {
                res.get("window")
                    .and_then(|w| w.get("text"))
                    .and_then(Value::as_str)
            })
            .map(str::to_string)
            .filter(|t| header.is_empty() || !t.is_empty())
            .unwrap_or(header);
        let sorted = items
            .first()
            .and_then(Option::as_ref)
            .is_some_and(|i| i.get("textkey").is_some());
        let base = res.get("base").cloned();
        // a list that only has items without ids is a settings list
        self.push(Mode::List(List {
            map,
            header,
            items,
            index: 0,
            base,
            source: Some(source),
            sorted,
            count_until: 0.0,
            menu_id: None,
        }));
    }

    async fn list_move(&mut self, dir: i64, repeat: bool) {
        let hold = self.ir.hold_time();
        let Some(Mode::List(l)) = self.stack.last_mut() else {
            return;
        };
        let len = l.items.len();
        if len < 2 {
            if !repeat {
                self.transition = Some(if dir < 0 {
                    Transition::BumpUp
                } else {
                    Transition::BumpDown
                });
            }
            return;
        }
        let new = self.scroll.scroll(dir, len, l.index, hold);
        if new != l.index {
            l.index = new;
            l.count_until = Self::now() + 1.5;
            self.transition = Some(if dir < 0 {
                Transition::PushUp
            } else {
                Transition::PushDown
            });
            self.ensure_loaded(new).await;
        }
    }

    fn current_item(&self) -> Option<(Value, Option<Value>, String)> {
        let Some(Mode::List(l)) = self.stack.last() else {
            return None;
        };
        l.item()
            .map(|i| (i.clone(), l.base.clone(), l.header.clone()))
    }

    /// Right: enter the item.
    async fn list_right(&mut self) {
        let Some((item, base, _)) = self.current_item() else {
            self.transition = Some(Transition::BumpRight);
            return;
        };
        let title = menu::text(&item);
        match menu::kind(&item, base.as_ref()) {
            Kind::Rms(r) if r == "nowplaying" => {
                let idx = self.playing_song_index();
                self.push(Mode::Playlist(idx));
            }
            Kind::Rms(r) if r == "alarm" => self.alarm_open().await,
            Kind::Rms(r) if r.starts_with("alarm:") => {
                self.alarm_select(&item, &r["alarm:".len()..]).await
            }
            Kind::Rms(_) | Kind::Text => self.transition = Some(Transition::BumpRight),
            Kind::Node(id) => {
                if id == "home" || self.nodes.is_empty() {
                    self.load_nodes().await;
                }
                let items = self.nodes.get(&id).cloned().unwrap_or_default();
                let map = if id == "settings" || id.to_lowercase().contains("settings") {
                    "settings"
                } else {
                    "browsemenu"
                };
                self.push(Mode::List(List::fixed(map, title, items, None)));
            }
            Kind::Inline => {
                let items = item
                    .get("item_loop")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let b = item.get("base").cloned().or(base);
                // a group holding just a slider opens the slider directly
                if items.len() == 1 && menu::kind(&items[0], b.as_ref()) == Kind::Slider {
                    self.push(Self::bar_for(&items[0], title));
                } else {
                    self.push(Mode::List(List::fixed("settings", title, items, b)));
                }
            }
            Kind::Choice => {
                let selected = item
                    .get("selectedIndex")
                    .and_then(Value::as_i64)
                    .unwrap_or(1)
                    - 1;
                let choices = item
                    .pointer("/actions/do/choices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let items: Vec<Value> = item
                    .get("choiceStrings")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .map(|(i, t)| {
                        json!({ "text": t, "radio": (i as i64 == selected) as i32,
                        "actions": { "do": choices.get(i).cloned().unwrap_or(Value::Null) } })
                    })
                    .collect();
                let mut l = List::fixed("settings", title, items, None);
                l.index = selected.max(0) as usize;
                self.push(Mode::List(l));
            }
            Kind::Checkbox(on) => {
                if let Some(a) = item
                    .pointer(if on { "/actions/off" } else { "/actions/on" })
                    .cloned()
                {
                    self.request(menu::tokens(&a, None, None)).await;
                }
                self.update_item(|i| {
                    i["checkbox"] = json!((!on) as i32);
                });
            }
            Kind::Radio(_) => {
                if let Some(a) = menu::action(&item, base.as_ref(), "do") {
                    self.request(menu::tokens(&a, None, None)).await;
                }
                if let Some(Mode::List(l)) = self.stack.last_mut() {
                    let idx = l.index;
                    for (i, it) in l.items.iter_mut().enumerate() {
                        if let Some(it) = it {
                            if it.get("radio").is_some() {
                                it["radio"] = json!((i == idx) as i32);
                            }
                        }
                    }
                }
            }
            Kind::Slider => {
                let header = match (title.is_empty(), self.stack.last()) {
                    (true, Some(Mode::List(l))) => l.header.clone(),
                    _ => title,
                };
                self.push(Self::bar_for(&item, header));
            }
            Kind::Input => {
                let allowed = item
                    .pointer("/input/allowedChars")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let upper_only = !allowed.chars().any(|c| c.is_ascii_lowercase());
                let initial = item
                    .pointer("/input/initialText")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let cs = chars(upper_only);
                let mut array: Vec<usize> = initial
                    .chars()
                    .filter_map(|c| cs.iter().position(|x| *x == Some(c)))
                    .collect();
                array.push(0);
                self.push(Mode::Text(TextInput {
                    header: item
                        .pointer("/window/text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or(title),
                    item,
                    base,
                    upper_only,
                    array,
                    next_char_at: None,
                }));
            }
            Kind::Go => {
                let action = if menu::is_audio(&item) {
                    menu::action(&item, base.as_ref(), "more")
                        .or_else(|| menu::action(&item, base.as_ref(), "go"))
                } else {
                    menu::action(&item, base.as_ref(), "go")
                };
                let Some(action) = action else { return };
                self.go(action, title, None).await;
            }
            Kind::Do => {
                if let Some(a) = menu::action(&item, base.as_ref(), "do") {
                    self.do_action(&a, None).await;
                }
            }
        }
    }

    fn bar_for(item: &Value, header: String) -> Mode {
        let get = |k: &str| {
            item.get(k).and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
        };
        Mode::Bar(Bar {
            header,
            value: get("initial").unwrap_or(0),
            min: get("min").unwrap_or(0),
            max: get("max").unwrap_or(100),
            action: item.pointer("/actions/do").cloned().unwrap_or(Value::Null),
        })
    }

    fn update_item(&mut self, f: impl FnOnce(&mut Value)) {
        if let Some(Mode::List(l)) = self.stack.last_mut() {
            let idx = l.index;
            if let Some(Some(it)) = l.items.get_mut(idx) {
                f(it);
            }
        }
    }

    /// Execute a `go` action: a menu request becomes a new list; actions that open another
    /// window (`nextWindow`) are run like `do` actions.
    async fn go(&mut self, action: Value, title: String, input: Option<&str>) {
        if menu::next_window(&action).is_some() {
            return self.do_action(&action, input).await;
        }
        let map = if action
            .get("cmd")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(Value::as_str)
            == Some("trackinfo")
        {
            "trackinfo"
        } else {
            "browsedb"
        };
        let source = menu::tokens(&action, None, input);
        self.push_request(map, title, source).await;
    }

    async fn do_action(&mut self, action: &Value, input: Option<&str>) {
        self.request(menu::tokens(action, None, input)).await;
        match menu::next_window(action) {
            Some("nowPlaying") | Some("playlist") => self.go_now_playing().await,
            Some("parent") | Some("parentNoRefresh") => self.pop(),
            Some("grandparent") => {
                self.pop();
                self.pop();
            }
            Some("home") => {
                self.reset_home().await;
                self.transition = Some(Transition::PushRight);
            }
            Some("refresh") | Some("refreshOrigin") => self.refresh().await,
            _ => {}
        }
    }

    /// Reload the top list (keeping the position).
    async fn refresh(&mut self) {
        let Some(Mode::List(l)) = self.stack.last() else {
            return;
        };
        let (Some(source), index, map, header) =
            (l.source.clone(), l.index, l.map, l.header.clone())
        else {
            return;
        };
        let saved = self.transition;
        self.stack.pop();
        self.push_request(map, header, source).await;
        self.transition = saved;
        if let Some(Mode::List(l)) = self.stack.last_mut() {
            l.index = index.min(l.items.len().saturating_sub(1));
        }
        self.ensure_loaded(index).await;
    }

    /// `play_0` (play), `play_1` (add), `play_2` (insert) in browse lists.
    async fn list_play(&mut self, which: &str) {
        let Some((item, base, _)) = self.current_item() else {
            return;
        };
        let title = menu::text(&item);
        let names: &[&str] = match which {
            "0" => &["play"],
            "1" => &["add"],
            _ => &["add-hold", "add"],
        };
        let mut action = names
            .iter()
            .find_map(|n| menu::action(&item, base.as_ref(), n));
        if action.is_none() && which != "0" {
            // tracks: add / insert the track itself
            if let Some(cp) = item.get("commonParams").and_then(Value::as_object) {
                let cmd = if which == "1" { "add" } else { "insert" };
                let mut params = cp.clone();
                params.insert("cmd".into(), json!(cmd));
                action = Some(json!({ "cmd": ["playlistcontrol"], "params": params }));
            }
        }
        match action {
            Some(a) => {
                self.request(menu::tokens(&a, None, None)).await;
                let line1 = match which {
                    "0" => s("NOW_PLAYING_FROM"),
                    "1" => s("ADDING_TO_PLAYLIST"),
                    _ => s("INSERT_TO_PLAYLIST"),
                };
                self.show_briefly(vec![line1, title], 1.5);
            }
            // menus and settings: play and add act like right
            None => {
                if menu::kind(&item, base.as_ref()) != Kind::Text {
                    self.list_right().await;
                }
            }
        }
    }

    /// Number keys in lists (`numberScroll`).
    async fn list_number(&mut self, digit: usize) {
        let Some(Mode::List(l)) = self.stack.last() else {
            return;
        };
        let (len, sorted) = (l.items.len(), l.sorted);
        if len <= 1 {
            return;
        }
        let new = if sorted {
            let letter = self.number_letter(digit as u8, &NUMBER_LETTERS);
            // binary search on the items' first letters
            let (mut low, mut high) = (-1i64, len as i64);
            let mut i = 0i64;
            while high - low > 1 {
                i = (high + low) / 2;
                self.ensure_loaded(i as usize).await;
                let key = match self.stack.last() {
                    Some(Mode::List(l)) => {
                        l.items.get(i as usize).and_then(Option::as_ref).map(|it| {
                            it.get("textkey")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| menu::text(it))
                        })
                    }
                    _ => None,
                }
                .unwrap_or_default()
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or(' ');
                if key == letter {
                    break;
                } else if letter < key {
                    high = i;
                } else {
                    low = i;
                }
            }
            // back up to the first item with that letter
            let mut idx = i.max(0) as usize;
            if let Some(Mode::List(l)) = self.stack.last() {
                let first = |it: &Option<Value>| {
                    it.as_ref()
                        .and_then(|it| {
                            it.get("textkey")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .or_else(|| Some(menu::text(it)))
                        })
                        .and_then(|t| t.chars().next())
                        .map(|c| c.to_ascii_uppercase())
                };
                while idx > 0 && first(&l.items[idx - 1]) == Some(letter) {
                    idx -= 1;
                }
                if first(&l.items[idx]).is_some_and(|c| c < letter) && idx + 1 < len {
                    idx += 1;
                }
            }
            idx
        } else {
            self.number_jump(digit, len)
        };
        if let Some(Mode::List(l)) = self.stack.last_mut() {
            l.index = new.min(len - 1);
            l.count_until = Self::now() + 1.5;
        }
        self.ensure_loaded(new.min(len - 1)).await;
    }

    /// Unsorted `numberScroll`: digits jump to that item (1-based), several digits within a
    /// second build a larger number.
    fn number_jump(&mut self, digit: usize, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        let d = (digit + 9) % 10;
        let i = if len <= 10 {
            d
        } else {
            let now = Self::now();
            let i = if self.digit.1 + DISPLAY_TEXT_TIMEOUT < now {
                d
            } else {
                self.digit.0 * 10 + digit.saturating_sub(1)
            };
            self.digit = (i.min(len - 1) + 1, now);
            i
        };
        i.min(len - 1)
    }

    /// `numberLetter`: multi-tap letter for a digit.
    fn number_letter(&mut self, digit: u8, table: &[&str; 10]) -> char {
        let now = Self::now();
        let letters: Vec<char> = table[(digit % 10) as usize].chars().collect();
        let index = if digit != self.letter.0 || self.letter.2 + DISPLAY_TEXT_TIMEOUT < now {
            0
        } else {
            (self.letter.1 + 1) % letters.len()
        };
        self.letter = (digit, index, now);
        letters[index]
    }

    // ---- text input -------------------------------------------------------------------------

    fn text_change(&mut self, dir: i64) {
        let hold = self.ir.hold_time();
        let Some(Mode::Text(t)) = self.stack.last_mut() else {
            return;
        };
        t.next_char_at = None;
        let len = chars(t.upper_only).len();
        let cur = *t.array.last().unwrap_or(&0);
        let new = self.scroll.scroll(dir, len, cur, hold);
        if let Some(last) = t.array.last_mut() {
            *last = new;
        }
    }

    fn text_number_letter(&mut self, digit: u8) {
        let Some(Mode::Text(t)) = self.stack.last() else {
            return;
        };
        let upper = t.upper_only;
        let now = Self::now();
        // a different digit within the timeout moves on first (`testSkipNextNumberLetter`)
        let skip = digit != self.letter.0 && self.letter.2 + DISPLAY_TEXT_TIMEOUT > now;
        let c = self.number_letter(
            digit,
            if upper {
                &NUMBER_LETTERS
            } else {
                &NUMBER_LETTERS_MIXED
            },
        );
        let Some(Mode::Text(t)) = self.stack.last_mut() else {
            return;
        };
        if skip && t.array.last() != Some(&0) {
            t.array.push(0);
        }
        if let Some(i) = chars(upper).iter().position(|x| *x == Some(c)) {
            if let Some(last) = t.array.last_mut() {
                *last = i;
            }
            t.next_char_at = Some(Instant::now() + Duration::from_secs_f64(DISPLAY_TEXT_TIMEOUT));
        }
    }

    fn text_value(t: &TextInput) -> String {
        let cs = chars(t.upper_only);
        t.array
            .iter()
            .filter_map(|i| cs.get(*i).copied().flatten())
            .collect()
    }

    async fn text_submit(&mut self) {
        let Some(Mode::Text(t)) = self.stack.last().cloned() else {
            return;
        };
        let value = Self::text_value(&t);
        if value.trim().is_empty() {
            self.transition = Some(Transition::BumpRight);
            return;
        }
        let base = t.base.as_ref();
        if let Some(a) = menu::action(&t.item, base, "go") {
            self.go(a, value.clone(), Some(&value)).await;
        } else if let Some(a) = menu::action(&t.item, base, "do") {
            self.stack.pop();
            self.do_action(&a, Some(&value)).await;
            self.transition = Some(Transition::PushRight);
        }
    }

    // ---- common functions -------------------------------------------------------------------

    async fn volume_step(&mut self, up: bool) {
        let mut inc: i64 = 1;
        if self.ir.hold_time() > 0.0 {
            inc = self.ir.repeat_count(50.0, 8.0);
        }
        if inc == 0 {
            crate::display::show_volume(&self.p);
            return;
        }
        let v = if up {
            format!("+{inc}")
        } else {
            format!("-{inc}")
        };
        self.cmd(&["mixer", "volume", &v]).await;
    }

    async fn playdisp(&mut self, arg: Option<&str>) {
        match arg {
            Some(n) if n.parse::<usize>().is_ok() => {
                self.p
                    .prefs
                    .set("playingDisplayMode", json!(n.parse::<usize>().unwrap_or(0)));
            }
            _ => {
                let showing_np = self.saver.is_some()
                    || matches!(self.stack.last(), Some(Mode::Playlist(i)) if *i == self.playing_song_index());
                if showing_np && self.playlist_len() > 0 {
                    crate::display::cycle_playing_display_mode(&self.p);
                } else {
                    self.saver = None;
                    self.go_now_playing().await;
                    self.transition = Some(Transition::PushLeft);
                }
            }
        }
    }

    fn playlist_title(&self, idx: usize) -> String {
        let entry = self.p.controller().lock().playlist.song(idx).cloned();
        entry
            .map(|e| {
                let info = self.server.track_info_for(&e);
                crate::display::display_title(&self.p, Some(&self.server), &info, None)
            })
            .unwrap_or_default()
    }

    /// `Slim::Buttons::Common` functions available in every mode.
    async fn common_function(&mut self, name: &str, arg: Option<&str>, button: &str) {
        let p = self.p.clone();
        match name {
            "power" => match arg {
                Some("on") => self.cmd(&["power", "1"]).await,
                Some("off") => self.cmd(&["power", "0"]).await,
                _ => self.cmd(&["power"]).await,
            },
            "modefunction" => {
                if arg == Some("off->play") {
                    self.cmd(&["power", "1"]).await;
                    self.cmd(&["play"]).await;
                }
            }
            "pause" => {
                if self.playlist_len() == 0 {
                    return;
                }
                let paused = p.controller().lock().mode() == "pause";
                self.cmd(&["pause", if paused { "0" } else { "1" }]).await;
            }
            "stop" => self.cmd(&["stop"]).await,
            "jump" => {
                if self.playlist_len() == 0 {
                    return;
                }
                match arg {
                    Some("rew") => {
                        let (elapsed, stopped) = {
                            let c = p.controller();
                            let c = c.lock();
                            (c.playing_song_elapsed(), c.is_stopped())
                        };
                        let to = if elapsed < 5.0 || stopped { "-1" } else { "+0" };
                        self.cmd(&["playlist", "jump", to]).await;
                    }
                    Some("fwd") => self.cmd(&["playlist", "jump", "+1"]).await,
                    _ => self.cmd(&["playlist", "jump", "+0"]).await,
                }
            }
            "brightness" => match arg {
                Some("up") => crate::display::adjust_brightness(&p, 1),
                Some("down") => crate::display::adjust_brightness(&p, -1),
                _ => crate::display::toggle_brightness(&p),
            },
            "textsize" => match arg {
                Some("small") => crate::display::set_text_size(&p, Some(0)),
                Some("medium") => crate::display::set_text_size(&p, Some(1)),
                Some("large") => crate::display::set_text_size(&p, None),
                _ => crate::display::toggle_text_size(&p),
            },
            "playdisp" => self.playdisp(arg).await,
            "titleFormat" => {
                let n = p.prefs.get_string_list("titleFormat").len().max(1) as i64;
                let cur = p.prefs.get_i64("titleFormatCurr");
                p.prefs.set("titleFormatCurr", json!((cur + 1) % n));
            }
            "visual" => {
                let n = p.prefs.get_string_list("visualModes").len().max(1) as i64;
                let cur = p.prefs.get_i64("visualMode");
                p.prefs.set("visualMode", json!((cur + 1) % n));
            }
            "muting" => self.cmd(&["mixer", "muting"]).await,
            "volume" => {
                let front = arg == Some("front");
                let timeout = if front { 3.0 } else { 1.0 };
                if !matches!(self.stack.last(), Some(Mode::Volume(_))) {
                    self.stack.push(Mode::Volume(timeout));
                }
                // passthrough: the button again, now in volume mode
                let up = button.starts_with("volup")
                    || button.starts_with("arrow_up")
                    || button.starts_with("knob_right");
                self.volume_step(up).await;
            }
            "bass" | "treble" | "pitch" => {
                let inc = if self.ir.hold_time() > 0.0 {
                    self.ir.repeat_count(50.0, 8.0)
                } else {
                    1
                };
                if inc > 0 {
                    let v = if arg == Some("up") {
                        format!("+{inc}")
                    } else {
                        format!("-{inc}")
                    };
                    self.cmd(&["mixer", name, &v]).await;
                }
            }
            "sleep" | "snooze" => self.sleep().await,
            "song" if arg == Some("scanner") => self.song_scanner().await,
            "shuffle" => {
                match arg {
                    Some("on") => self.cmd(&["playlist", "shuffle", "1"]).await,
                    Some("off") => self.cmd(&["playlist", "shuffle", "0"]).await,
                    _ => self.cmd(&["playlist", "shuffle"]).await,
                }
                let line = match p.controller().lock().playlist.shuffle {
                    2 => s("SHUFFLE_ON_ALBUMS"),
                    1 => s("SHUFFLE_ON_SONGS"),
                    _ => s("SHUFFLE_OFF"),
                };
                self.show_briefly(vec![String::new(), line], 1.0);
            }
            "repeat" => {
                match arg {
                    Some(n @ ("0" | "1" | "2")) => self.cmd(&["playlist", "repeat", n]).await,
                    _ => self.cmd(&["playlist", "repeat"]).await,
                }
                let line = match p.controller().lock().playlist.repeat {
                    2 => s("REPEAT_ALL"),
                    1 => s("REPEAT_ONE"),
                    _ => s("REPEAT_OFF"),
                };
                self.show_briefly(vec![String::new(), line], 1.0);
            }
            "preset" => {
                let release = arg.is_some_and(|a| a.starts_with("release"));
                let digit = arg.unwrap_or("").trim_start_matches("release");
                let num = if digit == "0" || digit.is_empty() {
                    "10".to_string()
                } else {
                    digit.to_string()
                };
                if !release {
                    self.show_briefly(vec![s("PRESET_HOLD_TO_SAVE").replace("%s", &num)], 5.0);
                } else if self.ir.hold_time() < 5.0 {
                    self.play_preset(&num).await;
                }
            }
            "playPreset" => self.play_preset(arg.unwrap_or("1")).await,
            "favorites" => match arg {
                Some(a) if a.starts_with("add") => {
                    self.favorites_add(a.trim_start_matches("add")).await
                }
                _ => {
                    if let Some(item) = self.find_home_item("favorites") {
                        self.saver = None;
                        if let Some(a) = menu::action(&item, None, "go") {
                            self.go(a, menu::text(&item), None).await;
                        }
                    }
                }
            },
            "browse" => self.open_home_item("myMusic").await,
            "globalsearch" => self.open_home_item("globalSearch").await,
            "menu" => match arg.unwrap_or("") {
                "home" => {
                    self.reset_home().await;
                    self.transition = Some(Transition::PushRight);
                }
                "now_playing" | "playlist" => {
                    self.go_now_playing().await;
                    self.transition = Some(Transition::PushLeft);
                }
                "browse_music" => self.open_home_item("myMusic").await,
                "browse_album" => self.open_home_item("myMusicAlbums").await,
                "browse_artist" => self.open_home_item("myMusicArtistsAllArtists").await,
                "browse_genre" => self.open_home_item("myMusicGenres").await,
                "browse_playlists" => self.open_home_item("myMusicPlaylists").await,
                "search_artist" | "search_album" | "search_song" => {
                    self.open_home_item("globalSearch").await
                }
                "settings" => self.open_home_item("settings").await,
                "synchronize" => self.open_home_item("settingsSync").await,
                "pop" => self.pop(),
                _ => {}
            },
            "home" => {
                // at the top of Home: go to Now Playing
                let at_home = self.stack.len() == 1;
                if at_home {
                    self.go_now_playing().await;
                    self.transition = Some(Transition::PushLeft);
                } else {
                    self.reset_home().await;
                    self.transition = Some(Transition::PushRight);
                }
            }
            "up" | "down" => {
                if !button.ends_with(".repeat") {
                    self.transition = Some(if name == "up" {
                        Transition::BumpUp
                    } else {
                        Transition::BumpDown
                    });
                }
            }
            "left" => self.pop(),
            "right" => self.transition = Some(Transition::BumpRight),
            _ => debug!("{}: function {} not implemented", p.id, name),
        }
    }

    /// Open an item of the home menu tree (from home).
    async fn open_home_item(&mut self, id: &str) {
        self.saver = None;
        self.reset_home().await;
        let item = if id == "settings" {
            self.nodes.get("home").and_then(|h| {
                h.iter()
                    .find(|i| i.get("id").and_then(Value::as_str) == Some("settings"))
                    .cloned()
            })
        } else {
            self.find_home_item(id)
        };
        let Some(item) = item else { return };
        // open it from a one-item helper list, then drop the helper
        let helper = self.stack.len();
        self.stack.push(Mode::List(List::fixed(
            "home",
            String::new(),
            vec![item],
            None,
        )));
        self.list_right().await;
        if self.stack.len() > helper + 1 {
            self.stack.remove(helper);
        } else {
            self.stack.truncate(helper);
        }
        self.transition = Some(Transition::PushLeft);
    }

    async fn play_preset(&mut self, num: &str) {
        let n: usize = num.parse().unwrap_or(1).max(1);
        let preset = self
            .p
            .prefs
            .get("presets")
            .and_then(|v| v.get(n - 1).cloned());
        let url = preset
            .as_ref()
            .and_then(|p| p.get("URL").or_else(|| p.get("url")))
            .and_then(Value::as_str)
            .map(str::to_string);
        match url {
            Some(url) => {
                let title = preset
                    .as_ref()
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.cmd(&["playlist", "play", &url, &title]).await;
                self.show_briefly(vec![s("PRESET").replace("%s", num), title], 1.5);
            }
            None => self.show_briefly(vec![s("PRESETS_NOT_DEFINED").replace("%s", num)], 1.5),
        }
    }

    /// `favorites_add` / `favorites_addN`: the browsed item or the browsed playlist song.
    async fn favorites_add(&mut self, preset: &str) {
        let (url, title, kind) = match self.stack.last() {
            Some(Mode::List(l)) => {
                let Some(it) = l.item() else { return };
                let pp = it.get("presetParams");
                let url = pp
                    .and_then(|p| p.get("favorites_url"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let title = pp
                    .and_then(|p| p.get("favorites_title"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| menu::text(it));
                let kind = pp
                    .and_then(|p| p.get("favorites_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("audio")
                    .to_string();
                (url, title, kind)
            }
            _ => {
                let idx = match self.stack.last() {
                    Some(Mode::Playlist(i)) => *i,
                    _ => self.playing_song_index(),
                };
                let url = self
                    .p
                    .controller()
                    .lock()
                    .playlist
                    .song(idx)
                    .map(|e| e.url.clone());
                (url, self.playlist_title(idx), "audio".to_string())
            }
        };
        let Some(url) = url else {
            self.transition = Some(Transition::BumpRight);
            return;
        };
        if preset.is_empty() {
            let t = format!("title:{title}");
            let u = format!("url:{url}");
            let k = format!("type:{kind}");
            self.cmd(&["favorites", "add", &u, &t, &k]).await;
            self.show_briefly(vec![s("FAVORITES_ADDING"), title], 1.5);
        } else {
            let key = format!("key:{preset}");
            let u = format!("favorites_url:{url}");
            let t = format!("favorites_title:{title}");
            let k = format!("favorites_type:{kind}");
            self.cmd(&["jivefavorites", "set_preset", &key, &u, &t, &k])
                .await;
            self.show_briefly(vec![s("PRESET").replace("%s", preset), title], 1.5);
        }
    }

    /// `sleep`: cycle 15/30/45/60/90 minutes (or to the end of the song) and off.
    async fn sleep(&mut self) {
        let (remaining, playing) = {
            let c = self.p.controller();
            let c = c.lock();
            let playing = c.mode() == "play";
            let rem = c
                .playing_song_duration()
                .map(|d| d - c.playing_song_elapsed())
                .unwrap_or(0.0);
            (if playing { rem / 60.0 } else { 0.0 }, playing)
        };
        let current = {
            let st = self.p.state.lock();
            if st.sleep_time > 0.0 {
                ((st.sleep_time - Self::now()) / 60.0).max(0.0)
            } else {
                0.0
            }
        };
        let mut choices = [0.0, 15.0, 30.0, 45.0, 60.0, 90.0];
        let mut i = 0;
        while i < choices.len() {
            if (remaining + 0.5).floor() == choices[i] {
                choices[i] = remaining;
            }
            if choices[i] > current {
                break;
            }
            i += 1;
        }
        let sleep = if i == choices.len() {
            if remaining > choices[5] && current <= choices[5] {
                remaining
            } else {
                0.0
            }
        } else if playing && remaining > current && remaining < choices[i] {
            remaining
        } else {
            choices[i]
        };
        let secs = (sleep * 60.0).round() as i64;
        self.cmd(&["sleep", &secs.to_string()]).await;
        let line = if secs == 0 {
            s("CANCEL_SLEEP")
        } else {
            format!(
                "{} {} {}",
                s("SLEEPING_IN"),
                (sleep + 0.5) as i64,
                s("MINUTES")
            )
        };
        self.show_briefly(vec![String::new(), line], 1.0);
    }

    // ---- periodic -----------------------------------------------------------------------------

    /// Power changes, the screensaver, volume mode timeout, and following the playing song.
    async fn periodic(&mut self) {
        let now = Self::now();
        let on = self.p.power();
        if on != self.was_on {
            self.was_on = on;
            self.saver = None;
            if on {
                self.go_now_playing().await;
            } else {
                self.reset_home().await;
            }
        }
        if !on {
            return;
        }
        let last_ir = self.p.state.lock().last_ir;
        // volume mode ends after its timeout
        if let Some(Mode::Volume(timeout)) = self.stack.last() {
            if now - last_ir > *timeout {
                self.stack.pop();
            }
        }
        // follow the playing song in Now Playing
        let playing_index = self.playing_song_index();
        if playing_index != self.playing_index {
            for m in self.stack.iter_mut() {
                if let Mode::Playlist(i) = m {
                    if *i == self.playing_index {
                        *i = playing_index;
                    }
                }
            }
            self.playing_index = playing_index;
        }
        let len = self.playlist_len();
        for m in self.stack.iter_mut() {
            if let Mode::Playlist(i) = m {
                *i = (*i).min(len.saturating_sub(1));
            }
        }
        // screensaver
        let playing = self.p.controller().lock().mode() == "play";
        let pref = self
            .p
            .prefs
            .get_str(if playing { "screensaver" } else { "idlesaver" });
        let wanted = match pref.as_str() {
            "nosaver" | "" => None,
            "SCREENSAVER.datetime" if !playing => Some(Saver::Idle { clock: true }),
            _ if playing => Some(Saver::Playing),
            _ => Some(Saver::Idle { clock: false }),
        };
        if self.saver.is_some() && self.saver != wanted {
            // the screensaver changed (e.g. playback stopped)
            self.saver = None;
        }
        let timeout = self.p.prefs.get_f64("screensavertimeout");
        let showing_np =
            matches!(self.stack.last(), Some(Mode::Playlist(i)) if *i == playing_index);
        let in_volume = matches!(self.stack.last(), Some(Mode::Volume(_)));
        if let Some(w) = wanted {
            if self.saver.is_none()
                && timeout > 0.0
                && last_ir < now - timeout
                && !showing_np
                && !in_volume
            {
                if pref == "playlist" {
                    self.go_now_playing().await;
                }
                self.saver = Some(w);
            }
        }
    }

    // ---- screens ------------------------------------------------------------------------------

    fn lines(&self) -> Option<Parts> {
        if !self.p.power() {
            return None;
        }
        match self.saver {
            Some(Saver::Idle { clock: true }) => {
                return Some(crate::display::datetime_parts(&self.server, &self.p))
            }
            Some(_) => return None,
            None => {}
        }
        let always_count = self.p.prefs.get_i64("alwaysShowCount") != 0;
        let now = Self::now();
        for m in self.stack.iter().rev() {
            match m {
                Mode::Volume(_) => continue,
                Mode::Scanner(sc) => return Some(self.scanner_lines(sc)),
                Mode::Time(t) => return Some(t.lines()),
                Mode::List(l) => {
                    let mut parts = Parts {
                        line: [l.header.clone(), String::new()],
                        ..Default::default()
                    };
                    if l.items.is_empty() {
                        parts.line[1] = s("EMPTY");
                    } else {
                        if always_count || l.count_until > now {
                            parts.overlay[0] =
                                format!(" {} {} {}", l.index + 1, s("OF"), l.items.len());
                        }
                        if let Some(it) = l.item() {
                            parts.line[1] = menu::text(it);
                            parts.overlay[1] = menu::overlay(it, l.base.as_ref());
                        }
                    }
                    return Some(parts);
                }
                Mode::Playlist(i) => {
                    if *i == self.playing_song_index() || self.playlist_len() == 0 {
                        return None;
                    }
                    let mut parts = Parts {
                        line: [s("CURRENT_PLAYLIST"), self.playlist_title(*i)],
                        overlay: [String::new(), menu::NOTESYMBOL.into()],
                        ..Default::default()
                    };
                    if always_count {
                        parts.overlay[0] =
                            format!(" {} {} {}", i + 1, s("OF"), self.playlist_len());
                    }
                    return Some(parts);
                }
                Mode::Text(t) => {
                    let cs = chars(t.upper_only);
                    let n = t.array.len();
                    let mut line2: String = t.array[..n - 1]
                        .iter()
                        .filter_map(|i| cs.get(*i).copied().flatten())
                        .collect();
                    // keep the end of long input visible
                    let skip = line2.chars().count().saturating_sub(28);
                    line2 = line2.chars().skip(skip).collect();
                    line2.push_str(menu::CURSORPOS);
                    match cs.get(t.array[n - 1]).copied().flatten() {
                        Some(c) => line2.push(c),
                        None => line2.push_str(menu::RIGHTARROW),
                    }
                    return Some(Parts {
                        line: [t.header.clone(), line2],
                        ..Default::default()
                    });
                }
                Mode::Bar(b) => {
                    let pct = if b.max > b.min {
                        (b.value - b.min) as f64 * 100.0 / (b.max - b.min) as f64
                    } else {
                        0.0
                    };
                    return Some(Parts {
                        line: [format!("{} ({})", b.header, b.value), String::new()],
                        slider: Some(pct),
                        ..Default::default()
                    });
                }
            }
        }
        None
    }

    fn publish(&mut self) {
        let parts = self.lines();
        let transition = self.transition.take();
        if parts == self.published && transition.is_none() {
            return;
        }
        let mut v = self.p.ui.lock();
        v.parts = parts.clone();
        if transition.is_some() {
            v.transition = transition;
        }
        v.generation += 1;
        drop(v);
        self.published = parts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_names_split_like_lyrion() {
        assert_eq!(split_function("up_repeat"), ("up", Some("repeat")));
        assert_eq!(
            split_function("menu_browse_album"),
            ("menu", Some("browse_album"))
        );
        assert_eq!(
            split_function("modefunction_off->play"),
            ("modefunction", Some("off->play"))
        );
        assert_eq!(split_function("right"), ("right", None));
    }

    #[test]
    fn text_input_character_sets() {
        let up = chars(true);
        assert_eq!(up[0], None);
        assert_eq!(up[1], Some('A'));
        assert!(chars(false).contains(&Some('a')));
        assert!(!up.contains(&Some('a')));
    }
}
