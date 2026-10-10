//! The server: shared state and the glue between players, library and control layer.

use crate::config::Config;
use crate::control::dispatch::DispatchTable;
use crate::notify::Notifier;
use crate::player::controller::Controller;
use crate::player::playlist::Entry;
use crate::player::song::TrackInfo;
use crate::player::{Player, FORGET_DISCONNECTED_SECS};
use crate::prefs::Prefs;
use dashmap::DashMap;
use indexmap::IndexMap;
use parking_lot::{Mutex, RwLock};
use rms_library::Library;
use serde_json::json;
use slimproto::client::Helo;
use slimproto::server::ServerFrame;
use slimproto::MacAddress;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

static FFMPEG: OnceLock<Option<String>> = OnceLock::new();

/// Path to the ffmpeg binary (`RMS_FFMPEG` or `ffmpeg` on PATH).
pub fn ffmpeg_path() -> String {
    FFMPEG
        .get()
        .cloned()
        .flatten()
        .unwrap_or_else(|| "ffmpeg".to_string())
}

fn detect_ffmpeg() -> Option<String> {
    let candidate = std::env::var("RMS_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    match std::process::Command::new(&candidate)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Some(candidate),
        _ => None,
    }
}

/// Information obtained by probing a remote URL.
#[derive(Debug, Clone, Default)]
pub struct RemoteInfo {
    pub stream_url: String,
    pub content_type: String,
    pub bitrate: Option<u32>,
    pub title: Option<String>,
    pub is_hls: bool,
}

/// Per-connection handle so a newer connection can supersede an older one.
struct Conn {
    id: u64,
    cancel: CancellationToken,
}

pub struct Server {
    pub config: Config,
    pub prefs: Prefs,
    pub notifier: Notifier,
    pub dispatch: DispatchTable,
    pub library: Library,
    pub http: reqwest::Client,
    pub start_time: f64,
    players: RwLock<IndexMap<MacAddress, Arc<Player>>>,
    conns: Mutex<std::collections::HashMap<MacAddress, Conn>>,
    next_conn_id: AtomicU64,
    /// Metadata of remote URLs (titles from playlists / favorites / probing).
    pub remote_titles: DashMap<String, String>,
    pub remote_info: DashMap<String, RemoteInfo>,
    pub shutdown: CancellationToken,
    pub scanning: AtomicBool,
    pub favorites: crate::plugins::favorites::Favorites,
    pub cometd: crate::web::cometd::Manager,
    pub subscriptions: crate::control::subscribe::Subscriptions,
}

impl Server {
    pub fn new(config: Config) -> anyhow::Result<Arc<Server>> {
        std::fs::create_dir_all(&config.data_dir)?;
        let prefs = Prefs::open(&config.prefs_dir());
        if let Some(port) = config.http_port {
            prefs.server.set("httpport", json!(port));
        }
        if let Some(port) = config.cli_port {
            prefs.server.set("cliport", json!(port));
        }
        if !config.music_dirs.is_empty() {
            let dirs: Vec<String> = config
                .music_dirs
                .iter()
                .map(|d| {
                    std::fs::canonicalize(d)
                        .unwrap_or(d.clone())
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            prefs.server.set("mediadirs", json!(dirs));
        }
        if let Some(pd) = &config.playlist_dir {
            prefs.server.set("playlistdir", json!(pd.to_string_lossy()));
        }
        if let Some(name) = &config.name {
            prefs.server.set("libraryname", json!(name));
        }
        let ff = detect_ffmpeg();
        if ff.is_none() {
            warn!("ffmpeg not found: transcoding, seeking in some formats and cue sheets are unavailable");
        }
        let _ = FFMPEG.set(ff);
        let library = Library::open(&config.db_path(), &config.cache_dir())?;
        let http = reqwest::Client::builder()
            .user_agent(crate::stream::remote::USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(7))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        let favorites = crate::plugins::favorites::Favorites::load(&config.prefs_dir());
        let server = Arc::new(Server {
            prefs,
            notifier: Notifier::new(),
            dispatch: crate::control::build_dispatch_table(),
            library,
            http,
            start_time: crate::util::now(),
            players: RwLock::new(IndexMap::new()),
            conns: Mutex::new(Default::default()),
            next_conn_id: AtomicU64::new(1),
            remote_titles: DashMap::new(),
            remote_info: DashMap::new(),
            shutdown: CancellationToken::new(),
            scanning: AtomicBool::new(false),
            favorites,
            cometd: crate::web::cometd::Manager::default(),
            subscriptions: Default::default(),
            config,
        });
        server.apply_library_config();
        Ok(server)
    }

    pub fn has_ffmpeg(&self) -> bool {
        FFMPEG.get().map(|f| f.is_some()).unwrap_or(false)
    }

    pub fn apply_library_config(&self) {
        let p = &self.prefs.server;
        let opt = |k: &str| Some(p.get_str(k)).filter(|s| !s.is_empty());
        let mut cfg = rms_library::LibraryConfig {
            media_dirs: p
                .get_string_list("mediadirs")
                .into_iter()
                .map(Into::into)
                .collect(),
            playlist_dir: opt("playlistdir").map(Into::into),
            ..Default::default()
        };
        if let Some(v) = opt("ignoredarticles") {
            cfg.ignored_articles = v;
        }
        if let Some(v) = opt("splitList") {
            cfg.split_list = v;
        }
        if let Some(v) = opt("variousArtistsString") {
            cfg.various_artists_string = v;
        }
        if let Some(v) = opt("noGenreString") {
            cfg.no_genre_string = v;
        }
        cfg.ignore_dir_re = opt("ignoreDirRE");
        cfg.cover_art = p.get_str("coverArt");
        cfg.group_discs = p.get_bool("groupdiscs");
        cfg.composer_in_artists = p.get_bool("composerInArtists");
        cfg.conductor_in_artists = p.get_bool("conductorInArtists");
        cfg.band_in_artists = p.get_bool("bandInArtists");
        cfg.trackartist_in_artists = p.get_bool("trackartistInArtists");
        cfg.use_unified_artists_list = p.get_bool("useUnifiedArtistsList");
        cfg.various_artist_auto_identification = p.get_bool("variousArtistAutoIdentification");
        cfg.use_tpe2_as_album_artist = p.get_bool("useTPE2AsAlbumArtist");
        cfg.search_substring = p.get_bool("searchSubString");
        cfg.browse_age_limit = p.get_i64("browseagelimit").max(1) as usize;
        self.library.set_config(cfg);
    }

    // ---- identity --------------------------------------------------------------------

    pub fn library_name(&self) -> String {
        let n = self.prefs.server.get_str("libraryname");
        if !n.is_empty() {
            return n;
        }
        hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "RMS".to_string())
    }

    pub fn uuid(&self) -> String {
        self.prefs.server.get_str("server_uuid")
    }

    pub fn http_port(&self) -> u16 {
        self.prefs.server.get_i64("httpport").clamp(1, 65535) as u16
    }

    pub fn cli_port(&self) -> u16 {
        self.prefs.server.get_i64("cliport").clamp(1, 65535) as u16
    }

    /// Default name for a new player: model name, numbered when taken (`Client::name`).
    pub fn default_player_name(&self, model_name: &str) -> String {
        let base = if model_name.is_empty() {
            "Squeezebox".to_string()
        } else {
            model_name.to_string()
        };
        let taken: Vec<String> = self.players().iter().map(|p| p.name()).collect();
        if !taken.contains(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{} {}", base, i))
            .find(|n| !taken.contains(n))
            .unwrap()
    }

    // ---- players --------------------------------------------------------------------

    pub fn player(&self, id: &MacAddress) -> Option<Arc<Player>> {
        self.players.read().get(id).cloned()
    }

    /// Players in connection order (`Slim::Player::Client::clients`).
    pub fn players(&self) -> Vec<Arc<Player>> {
        self.players.read().values().cloned().collect()
    }

    pub fn connected_players(&self) -> Vec<Arc<Player>> {
        self.players()
            .into_iter()
            .filter(|p| p.is_connected())
            .collect()
    }

    /// `Slim::Player::Client::getClient`: MAC, `ip:port`, IP, name, or id.
    pub fn resolve_client(&self, token: &str) -> Option<Arc<Player>> {
        let token = token.trim();
        if token.is_empty() || token == "-" {
            return None;
        }
        if let Some(mac) = MacAddress::parse(token) {
            if let Some(p) = self.player(&mac) {
                return Some(p);
            }
        }
        let lower = token.to_ascii_lowercase();
        self.players().into_iter().find(|p| {
            let s = p.state.lock();
            let addr = s.addr.map(|a| a.to_string()).unwrap_or_default();
            let ip = s.addr.map(|a| a.ip().to_string()).unwrap_or_default();
            addr == token || ip == token || s.name == token || p.id.to_string() == lower
        })
    }

    /// Called by the slimproto connection after a HELO.  Returns the player and a token that
    /// is cancelled when a newer connection for the same player takes over.
    pub fn player_connected(
        self: &Arc<Server>,
        helo: &Helo,
        addr: SocketAddr,
        tx: mpsc::UnboundedSender<ServerFrame>,
    ) -> (Arc<Player>, u64, CancellationToken) {
        let existing = self.player(&helo.mac);
        let conn_id = self.next_conn_id.fetch_add(1, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        if let Some(old) = self.conns.lock().insert(
            helo.mac,
            Conn {
                id: conn_id,
                cancel: cancel.clone(),
            },
        ) {
            info!("{}: closing previous connection", helo.mac);
            old.cancel.cancel();
        }
        let player = match existing {
            Some(p) => {
                {
                    let mut s = p.state.lock();
                    s.caps = crate::player::Capabilities::from_helo(helo);
                    s.device_id = helo.device_id;
                    s.revision = helo.revision;
                    s.addr = Some(addr);
                    s.connected = true;
                    s.disconnected_at = None;
                    s.connected_at = crate::util::now();
                    s.last_heard = crate::util::now();
                    s.jiffies_epoch = None;
                    s.is_upgrading = false;
                }
                p.set_frame_sender(Some(tx));
                info!(
                    "{}: reconnect from {} ({})",
                    p.id,
                    addr,
                    p.state.lock().caps.model_name
                );
                self.notifier.notify(Some(p.id), &["client", "reconnect"]);
                p
            }
            None => {
                let p = Player::new(self, helo, addr);
                p.state.lock().connected = true;
                p.set_frame_sender(Some(tx));
                self.players.write().insert(p.id, p.clone());
                info!(
                    "{}: new player \"{}\" ({}) from {}",
                    p.id,
                    p.name(),
                    p.state.lock().caps.model_name,
                    addr
                );
                self.notifier.notify(Some(p.id), &["client", "new"]);
                p
            }
        };
        self.after_hello(&player, helo, existing_flag(&player));
        (player, conn_id, cancel)
    }

    /// `Squeezebox::reconnect` + `_hello_handler` tail: initial frames and state recovery.
    fn after_hello(self: &Arc<Server>, player: &Arc<Player>, helo: &Helo, _existing: bool) {
        crate::display::init_prefs(player);
        let (model, revision) = {
            let s = player.state.lock();
            (s.caps.model.clone(), s.revision)
        };
        if revision > 39 || model.starts_with("softsqueeze") {
            player.send(ServerFrame::Vers(crate::VERSION.to_string()));
        }
        // restore sync group
        let sg = helo
            .syncgroup_id()
            .or_else(|| Some(player.prefs.get_i64("syncgroupid") as u64).filter(|v| *v > 0));
        if let Some(sg) = sg {
            if let Some(buddy) = self.players().into_iter().find(|p| {
                p.id != player.id && p.prefs.get_i64("syncgroupid") as u64 == sg && p.is_connected()
            }) {
                info!("{}: restoring sync group with {}", player.id, buddy.id);
                self.sync(&buddy, player, true);
            }
        }
        let ctrl = player.controller();
        {
            let mut c = ctrl.lock();
            if player.power() {
                c.player_active(player);
            }
            if helo.bytes_received.is_some()
                && !helo.reconnect
                && c.is_active(player.id)
                && c.active_players().len() == 1
            {
                if !c.is_stopped() {
                    info!("{}: restarting play on pseudo-reconnect", player.id);
                    c.player_reconnect(helo.bytes_received);
                }
            }
            if c.is_stopped() {
                player.strm_stop();
            }
        }
        // SqueezePlay/squeezelite: query name; SB2 class also queries disableDac.
        player.query_player_name();
        if matches!(
            model.as_str(),
            "squeezebox2" | "squeezebox3" | "transporter" | "boom" | "receiver"
        ) {
            player.send(ServerFrame::Setd {
                id: slimproto::server::setd_id::DISABLE_DAC,
                value: None,
            });
        }
        player.audio_outputs_enable(player.power());
        player.send_volume();
        player.request_status();
    }

    /// The control connection closed.
    pub fn player_disconnected(self: &Arc<Server>, player: &Arc<Player>, conn_id: u64) {
        {
            let mut conns = self.conns.lock();
            match conns.get(&player.id) {
                Some(c) if c.id == conn_id => {
                    conns.remove(&player.id);
                }
                _ => return, // superseded by a newer connection
            }
        }
        player.set_frame_sender(None);
        {
            let mut s = player.state.lock();
            s.connected = false;
            s.disconnected_at = Some(crate::util::now());
        }
        info!("{}: disconnected", player.id);
        self.notifier
            .notify(Some(player.id), &["client", "disconnect"]);
        {
            let ctrl = player.controller();
            let mut c = ctrl.lock();
            let only = c.active_players().len() == 1 && c.is_active(player.id);
            if !only {
                c.player_inactive(player);
            }
            player
                .prefs
                .set("playingAtPowerOff", json!(c.is_playing(false) as i32));
        }
        let server = self.clone();
        let p = player.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(FORGET_DISCONNECTED_SECS)).await;
            if !p.is_connected() {
                server.forget_player(&p);
            }
        });
    }

    /// `client forget`.
    pub fn forget_player(self: &Arc<Server>, player: &Arc<Player>) {
        if player.is_connected() {
            return;
        }
        {
            let ctrl = player.controller();
            let mut c = ctrl.lock();
            c.player_inactive(player);
            if c.member_count() > 1 {
                c.remove_member(player);
            }
        }
        self.players.write().shift_remove(&player.id);
        info!("{}: forgotten", player.id);
        self.notifier.notify(Some(player.id), &["client", "forget"]);
    }

    /// Close the control connection of a player (used by the heartbeat checker).
    pub fn close_connection(&self, id: &MacAddress) {
        if let Some(c) = self.conns.lock().get(id) {
            c.cancel.cancel();
        }
    }

    // ---- power ----------------------------------------------------------------------

    pub fn set_power(self: &Arc<Server>, player: &Arc<Player>, on: bool, noplay: bool) {
        if player.power() == on {
            return;
        }
        player.state.lock().power = on;
        player.prefs.set("power", json!(on as i32));
        let ctrl = player.controller();
        let resume = player.prefs.get_str("powerOnResume");
        if on {
            player.audio_outputs_enable(true);
            let mut c = ctrl.lock();
            c.player_active(player);
            // `Player::resumeOnPower`: e.g. "PauseOff-PlayOn", "StopOff-ResetPlayOn"
            let resume_on = resume
                .split_once('-')
                .map(|(_, on)| on.trim_end_matches("On").to_string())
                .unwrap_or_default();
            if !noplay && !c.is_playing(false) {
                if resume_on.contains("Reset") && !c.playlist.is_empty() {
                    c.reset_song_queue(Some(0));
                }
                if resume_on.contains("Play")
                    && !c.playlist.is_empty()
                    && player.prefs.get_i64("playingAtPowerOff") != 0
                {
                    if c.is_paused() {
                        c.resume(None);
                    } else {
                        let idx = c.playing_song_index().min(c.playlist.len() - 1);
                        c.play(Some(idx), None, None);
                    }
                    player.prefs.set("playingAtPowerOff", json!(0));
                }
            }
        } else {
            {
                let mut c = ctrl.lock();
                // `Player::power`: only leave the group running if other members stay on
                let just_unsync = c
                    .all_players()
                    .iter()
                    .any(|o| o.id != player.id && o.power() && !o.prefs.get_bool("syncPower"));
                if just_unsync {
                    c.player_inactive(player);
                    player.prefs.set("playingAtPowerOff", json!(0));
                } else {
                    let playing = c.is_playing(true);
                    player.prefs.set("playingAtPowerOff", json!(playing as i32));
                    let resume_off = resume
                        .split_once("Off")
                        .map(|(a, _)| a.to_string())
                        .unwrap_or_default();
                    if playing && resume_off == "Pause" {
                        c.pause();
                    } else if !(c.is_paused() && resume_off == "Pause") {
                        c.stop();
                    }
                }
            }
            player.state.lock().sleep_time = 0.0;
            player.audio_outputs_enable(false);
        }
        // syncPower: power the rest of the group too
        if player.prefs.get_bool("syncPower") {
            let others: Vec<Arc<Player>> = ctrl
                .lock()
                .all_players()
                .into_iter()
                .filter(|p| p.id != player.id)
                .collect();
            for o in others {
                if o.prefs.get_bool("syncPower") {
                    self.set_power(&o, on, noplay);
                }
            }
        }
    }

    // ---- sync groups ----------------------------------------------------------------

    /// `$buddy->controller->sync($player)`: `player` joins `buddy`'s group.
    pub fn sync(self: &Arc<Server>, buddy: &Arc<Player>, player: &Arc<Player>, no_restart: bool) {
        let target = buddy.controller();
        let old = player.controller();
        if Arc::ptr_eq(&target, &old) {
            return;
        }
        let mut no_restart = no_restart;
        {
            let mut o = old.lock();
            if o.member_count() > 1 {
                o.remove_member(player);
                no_restart = false;
            } else if !no_restart && !o.is_stopped() {
                player.strm_stop();
            }
        }
        let gid = {
            let mut t = target.lock();
            let id = t.sync_group_id.unwrap_or_else(|| {
                let existing = buddy.prefs.get_i64("syncgroupid");
                if existing > 0 {
                    existing as u64
                } else {
                    rand::random::<u32>() as u64 % 999_999_999
                }
            });
            t.sync_group_id = Some(id);
            t.add_member(player);
            id
        };
        buddy.prefs.set("syncgroupid", json!(gid));
        player.prefs.set("syncgroupid", json!(gid));
        player.set_controller(target.clone());
        {
            let mut t = target.lock();
            t.after_join(player, no_restart);
            t.playlist.timestamp = crate::util::now();
        }
        let master = target.lock().master_id;
        self.notifier.notify(Some(master), &["playlist", "sync"]);
    }

    /// `controller->unsync($player)`.
    pub fn unsync(self: &Arc<Server>, player: &Arc<Player>) {
        let ctrl = player.controller();
        let (playlist, master) = {
            let mut c = ctrl.lock();
            if c.member_count() < 2 {
                return;
            }
            c.remove_member(player);
            (c.playlist.clone(), c.master_id)
        };
        let new = Controller::new_shared(self, player);
        {
            let mut n = new.lock();
            n.playlist = playlist;
            let cur = n.playlist.current;
            n.reset_song_queue(Some(cur));
            n.save_playlist();
        }
        player.set_controller(new);
        player.prefs.remove("syncgroupid");
        self.notifier.notify(Some(player.id), &["playlist", "stop"]);
        self.notifier.notify(Some(player.id), &["playlist", "sync"]);
        self.notifier.notify(Some(master), &["playlist", "sync"]);
    }

    /// Sync groups as lists of player ids (only groups with > 1 member).
    pub fn sync_groups(&self) -> Vec<Vec<Arc<Player>>> {
        let mut seen: Vec<*const Mutex<Controller>> = Vec::new();
        let mut out = Vec::new();
        for p in self.players() {
            let c = p.controller();
            let ptr = Arc::as_ptr(&c);
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            let members = c.lock().all_players();
            if members.len() > 1 {
                out.push(members);
            }
        }
        out
    }

    // ---- tracks ---------------------------------------------------------------------

    /// Build a playlist entry for a URL, filling library id / album key when known.
    pub fn entry_for_url(&self, url: &str) -> Entry {
        let mut e = Entry::new(url);
        if !crate::util::is_remote_url(url) {
            if let Some(t) = self.library.track_by_url(url) {
                e.track_id = Some(t.id);
                e.album_key = t.album_id.map(|a| a.to_string());
            }
        }
        e
    }

    pub fn entry_for_track(&self, t: &rms_library::Track) -> Entry {
        Entry {
            url: t.url.clone(),
            track_id: Some(t.id),
            album_key: t.album_id.map(|a| a.to_string()),
        }
    }

    /// Metadata for a playlist entry (`Slim::Schema::objectForUrl`).
    pub fn track_info_for(&self, e: &Entry) -> TrackInfo {
        if crate::util::is_remote_url(&e.url) {
            let info = self.remote_info.get(&e.url).map(|r| r.clone());
            return TrackInfo {
                url: e.url.clone(),
                title: self
                    .remote_titles
                    .get(&e.url)
                    .map(|t| t.clone())
                    .or_else(|| info.as_ref().and_then(|i| i.title.clone())),
                content_type: info
                    .as_ref()
                    .map(|i| i.content_type.clone())
                    .unwrap_or_default(),
                bitrate: info.as_ref().and_then(|i| i.bitrate),
                remote: true,
                ..Default::default()
            };
        }
        let t = e
            .track_id
            .and_then(|id| self.library.track(id))
            .or_else(|| self.library.track_by_url(&e.url));
        match t {
            Some(t) => track_info_from_library(&t),
            None => {
                // Not (yet) in the library: derive what we can from the file itself.
                let path = crate::util::path_from_file_url(&e.url);
                let ct = path
                    .as_ref()
                    .map(|p| content_type_for_ext(&crate::util::extension(p)))
                    .unwrap_or_default();
                let mut info = TrackInfo {
                    url: e.url.clone(),
                    content_type: ct.to_string(),
                    ..Default::default()
                };
                if let Some(p) = path {
                    probe_file(&p, &mut info);
                }
                info
            }
        }
    }

    /// Probe a remote URL (cached), expanding playlists to their first stream.
    pub async fn resolve_remote(&self, url: &str) -> Option<RemoteInfo> {
        let mut current = url.to_string();
        let mut title = self.remote_titles.get(url).map(|t| t.clone());
        for _depth in 0..4 {
            let probe = crate::stream::remote::probe(&self.http, &current).await;
            if title.is_none() {
                title = probe.title.clone();
            }
            if probe.is_hls {
                let info = RemoteInfo {
                    stream_url: probe.url.clone(),
                    content_type: "aac".into(),
                    bitrate: probe.bitrate,
                    title: title.clone(),
                    is_hls: true,
                };
                self.remote_info.insert(url.to_string(), info.clone());
                return Some(info);
            }
            if !probe.entries.is_empty() {
                if title.is_none() {
                    title = probe.entries[0].1.clone();
                }
                current = probe.entries[0].0.clone();
                continue;
            }
            let ct = probe.content_type.clone()?;
            let info = RemoteInfo {
                stream_url: probe.url.clone(),
                content_type: ct,
                bitrate: probe.bitrate,
                title: title.clone(),
                is_hls: false,
            };
            self.remote_info.insert(url.to_string(), info.clone());
            return Some(info);
        }
        None
    }

    // ---- volume fades ---------------------------------------------------------------

    /// `Player::fade_volume`: positive = fade in over `secs`, negative = fade out then call
    /// `done`.  Always asynchronous.
    pub fn fade_volume(
        &self,
        player: &Arc<Player>,
        secs: f64,
        done: Option<Box<dyn FnOnce() + Send>>,
    ) {
        let p = player.clone();
        tokio::spawn(async move {
            let target = p.volume();
            let steps = ((secs.abs() / 0.05).ceil() as i32).clamp(1, 200);
            let step_ms = ((secs.abs() * 1000.0) / steps as f64) as u64;
            for i in 1..=steps {
                let frac = i as f64 / steps as f64;
                let v = if secs < 0.0 {
                    (target as f64 * (1.0 - frac)).round() as i32
                } else {
                    (target as f64 * frac).round() as i32
                };
                p.set_volume_temp_silent(v);
                tokio::time::sleep(std::time::Duration::from_millis(step_ms)).await;
            }
            if let Some(cb) = done {
                cb();
            }
            if secs > 0.0 {
                p.clear_temp_volume();
            } else {
                // leave the real volume configured (player is paused now)
                p.clear_temp_volume();
            }
        });
    }
}

fn existing_flag(_p: &Arc<Player>) -> bool {
    true
}

pub fn content_type_for_ext(ext: &str) -> &'static str {
    match ext {
        "flac" | "flc" => "flc",
        "mp3" | "mp2" => "mp3",
        "m4a" | "m4b" | "mp4" | "aac" => "mp4",
        "alac" => "alc",
        "ogg" | "oga" => "ogg",
        "opus" => "ops",
        "wav" => "wav",
        "aif" | "aiff" | "aifc" => "aif",
        "wma" => "wma",
        "ape" => "ape",
        "wv" => "wvp",
        "mpc" => "mpc",
        "dsf" => "dsf",
        "dff" => "dff",
        _ => "",
    }
}

pub fn track_info_from_library(t: &rms_library::Track) -> TrackInfo {
    TrackInfo {
        url: t.url.clone(),
        id: Some(t.id),
        title: Some(t.title.clone()).filter(|t| !t.is_empty()),
        artist: t.artist.clone(),
        album: t.album.clone(),
        album_id: t.album_id,
        artist_id: t.artist_id,
        genre: t.genre.clone(),
        year: t.year,
        tracknum: t.tracknum,
        disc: t.disc,
        content_type: t.content_type.clone(),
        secs: t.secs,
        bitrate: t.bitrate,
        samplerate: t.samplerate,
        samplesize: t.samplesize,
        channels: t.channels,
        lossless: t.lossless,
        vbr: t.vbr,
        // tracks.endian = 1 means big-endian (LMS stream_s); AIFF defaults to big, WAV to little.
        little_endian: match t.endian {
            Some(1) => false,
            Some(_) => true,
            None => t.content_type != "aif",
        },
        audio_offset: t.audio_offset,
        audio_size: t.audio_size,
        filesize: t.filesize,
        replay_gain: t.replay_gain,
        replay_peak: t.replay_peak,
        album_replay_gain: t.album_replay_gain,
        coverid: t.coverid.clone(),
        remote: t.remote,
        cue_start: t.cue_start,
        cue_end: t.cue_end,
        artwork_url: None,
    }
}

/// Read audio properties and basic tags of a file that is not in the library.
fn probe_file(path: &std::path::Path, info: &mut TrackInfo) {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Accessor;
    let Ok(f) = lofty::read_from_path(path) else {
        return;
    };
    let props = f.properties();
    let secs = props.duration().as_secs_f64();
    if secs > 0.0 {
        info.secs = Some(secs);
    }
    info.bitrate = props.audio_bitrate().map(|k| k * 1000);
    info.samplerate = props.sample_rate();
    info.samplesize = props.bit_depth().map(u32::from);
    info.channels = props.channels().map(u32::from);
    info.lossless = matches!(
        info.content_type.as_str(),
        "flc" | "wav" | "aif" | "alc" | "ape" | "wvp"
    );
    info.filesize = std::fs::metadata(path).ok().map(|m| m.len());
    if let Some(tag) = f.primary_tag().or(f.first_tag()) {
        info.title = tag.title().map(|s| s.to_string());
        info.artist = tag.artist().map(|s| s.to_string());
        info.album = tag.album().map(|s| s.to_string());
    }
}
