//! Server and per-player preferences.
//!
//! Preferences are stored as YAML files in the prefs directory (`server.prefs` and
//! `<mac>.prefs`), compatible in spirit with the reference server.  Values are kept as
//! `serde_json::Value` so arbitrary keys set by clients (`pref`, `playerpref`) round-trip.

use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

pub type PrefMap = BTreeMap<String, Value>;

/// Default server preference values (subset of `Slim::Utils::Prefs::init`).
pub fn server_defaults() -> PrefMap {
    let mut m = PrefMap::new();
    let defaults = [
        ("httpport", json!(9000)),
        ("cliport", json!(9090)),
        ("language", json!("EN")),
        ("mediadirs", json!([])),
        ("playlistdir", json!("")),
        ("ignoredarticles", json!("The El La Los Las Le Les")),
        ("splitList", json!(";")),
        ("groupdiscs", json!(1)),
        ("playtrackalbum", json!(1)),
        ("persistPlaylists", json!(1)),
        ("reshuffleOnRepeat", json!(0)),
        ("composerInArtists", json!(0)),
        ("conductorInArtists", json!(0)),
        ("bandInArtists", json!(0)),
        ("trackartistInArtists", json!(0)),
        ("variousArtistAutoIdentification", json!(1)),
        ("useUnifiedArtistsList", json!(0)),
        ("useTPE2AsAlbumArtist", json!(0)),
        ("useBandAsAlbumArtist", json!(0)),
        ("variousArtistsString", json!("Various Artists")),
        ("noGenreString", json!("No Genre")),
        ("searchSubString", json!(0)),
        ("browseagelimit", json!(100)),
        ("bufferSecs", json!(3)),
        ("remotestreamtimeout", json!(15)),
        ("maxRedirects", json!(7)),
        ("maxPlaylistLength", json!(2500)),
        ("precacheArtwork", json!(1)),
        ("autorescan", json!(0)),
        ("disableStatistics", json!(0)),
        ("filterHosts", json!(0)),
        ("allowedHosts", json!("")),
        ("csrfProtectionLevel", json!(0)),
        ("authorize", json!(0)),
        ("username", json!("")),
        ("password", json!("")),
        ("insecureHTTPS", json!(0)),
        ("longdateFormat", json!("%A, %B |%d, %Y")),
        ("shortdateFormat", json!("%m/%d/%Y")),
        ("timeFormat", json!("|%I:%M %p")),
        ("showArtist", json!(0)),
        ("showYear", json!(0)),
        (
            "titleFormat",
            json!([
                "TITLE",
                "DISC-TRACKNUM. TITLE",
                "TRACKNUM. TITLE",
                "TRACKNUM. ARTIST - TITLE",
                "FILE.EXT",
                "TRACKNUM. TITLE (ARTIST)"
            ]),
        ),
        ("titleFormatWeb", json!(0)),
        ("thumbSize", json!(100)),
        ("syncStartDelay", json!(200)),
        ("syncStartDelayNotify", json!(0)),
        ("libraryname", json!("")),
        ("checkVersion", json!(0)),
        ("disabledextensionsaudio", json!("")),
        ("disabledextensionsplaylist", json!("")),
        ("prioritizeNative", json!(1)),
        ("disabledformats", json!([])),
        ("coverArt", json!("")),
        ("artfolder", json!("")),
        ("onlineArtistImages", json!(0)),
        ("sonicAnalysis", json!(1)),
        ("ignoreDirRE", json!("")),
        ("rank-PLUGIN_FAVORITES_MODULE_NAME", json!(5)),
        ("server_uuid", json!("")),
        ("securitySecret", json!("")),
        ("audiodir", json!("")),
        ("scannerPriority", json!(0)),
        ("transcoding", json!(1)),
        ("webproxy", json!("")),
        ("spotifyBitrate", json!(320)),
        ("spotifyNormalisation", json!(0)),
        ("spotifyClientId", json!("")),
    ];
    for (k, v) in defaults {
        m.insert(k.to_string(), v);
    }
    m
}

/// Default per-player preference values (`Slim::Player::Player::defaultPrefs` + client defaults).
pub fn player_defaults() -> PrefMap {
    let mut m = PrefMap::new();
    let defaults = [
        ("playername", json!("")),
        ("volume", json!(50)),
        ("mute", json!(0)),
        ("bass", json!(50)),
        ("treble", json!(50)),
        ("pitch", json!(100)),
        ("balance", json!(0)),
        ("power", json!(1)),
        ("repeat", json!(0)),
        ("shuffle", json!(0)),
        ("maxBitrate", json!(0)),
        ("transitionType", json!(0)),
        ("transitionDuration", json!(10)),
        ("transitionSmart", json!(1)),
        ("transitionSampleRestriction", json!(0)),
        ("replayGainMode", json!(0)),
        ("replayGainSmartGain", json!(1)),
        ("syncVolume", json!(0)),
        ("syncPower", json!(0)),
        ("syncBufferThreshold", json!(128)),
        ("bufferThreshold", json!(255)),
        ("digitalVolumeControl", json!(1)),
        ("preampVolumeControl", json!(0)),
        ("powerOffDac", json!(0)),
        ("disableDac", json!(0)),
        ("polarityInversion", json!(0)),
        ("outputChannels", json!(0)),
        ("fadeInDuration", json!(0)),
        ("playingDisplayMode", json!(0)),
        (
            "playingDisplayModes",
            json!([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]),
        ),
        (
            "menuItem",
            json!([
                "NOW_PLAYING",
                "BROWSE_MUSIC",
                "RADIO",
                "PLUGIN_MY_APPS_MODULE_NAME",
                "FAVORITES",
                "PLUGIN_APP_GALLERY_MODULE_NAME",
                "SETTINGS"
            ]),
        ),
        ("alarmsEnabled", json!(1)),
        ("alarmDefaultVolume", json!(50)),
        ("alarmSnoozeSeconds", json!(540)),
        ("alarmTimeoutSeconds", json!(3600)),
        ("alarmfadeseconds", json!(1)),
        ("alarms", json!({})),
        ("presets", json!([])),
        ("powerOnBrightness", json!(4)),
        ("powerOffBrightness", json!(1)),
        ("idleBrightness", json!(1)),
        ("screensaver", json!("screensaver")),
        ("idlesaver", json!("nosaver")),
        ("offsaver", json!("SCREENSAVER.datetime")),
        ("screensavertimeout", json!(30)),
        ("scrollMode", json!(0)),
        ("doublesize", json!(0)),
        ("powerOnResume", json!("PauseOff-PlayOn")),
        ("startDelay", json!(0)),
        ("lameQuality", json!(9)),
        ("mp3SilencePrelude", json!(0)),
        ("titleFormatCurr", json!(1)),
        ("titleFormat", json!([5, 1, 3, 6, 0])),
        ("playlist", json!([])),
        ("playlistShuffledList", json!([])),
        ("currentSong", json!(0)),
        ("playingAtPowerOff", json!(0)),
        ("playname", json!("")),
        ("irmap", json!("default.map")),
        ("disabledirsets", json!([])),
        ("remoteReplayGain", json!(-5)),
        ("alarmSoundEnabled", json!(1)),
    ];
    for (k, v) in defaults {
        m.insert(k.to_string(), v);
    }
    m
}

/// A single namespace of prefs backed by a YAML file.
pub struct PrefStore {
    path: PathBuf,
    defaults: PrefMap,
    values: RwLock<PrefMap>,
}

impl PrefStore {
    pub fn load(path: PathBuf, defaults: PrefMap) -> PrefStore {
        let mut values = PrefMap::new();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(text) => match serde_yaml::from_str::<serde_yaml::Value>(&text) {
                    Ok(y) => {
                        if let Ok(Value::Object(obj)) = serde_json::to_value(y) {
                            values.extend(obj);
                        }
                    }
                    Err(e) => warn!("could not parse {}: {}", path.display(), e),
                },
                Err(e) => warn!("could not read {}: {}", path.display(), e),
            }
        }
        PrefStore {
            path,
            defaults,
            values: RwLock::new(values),
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.values
            .read()
            .get(key)
            .cloned()
            .or_else(|| self.defaults.get(key).cloned())
    }

    pub fn get_str(&self, key: &str) -> String {
        match self.get(key) {
            Some(Value::String(s)) => s,
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        }
    }

    pub fn get_i64(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(Value::Number(n)) => n
                .as_i64()
                .unwrap_or_else(|| n.as_f64().map(|f| f as i64).unwrap_or(0)),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            Some(Value::Bool(b)) => b as i64,
            _ => 0,
        }
    }

    pub fn get_f64(&self, key: &str) -> f64 {
        match self.get(key) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    pub fn get_bool(&self, key: &str) -> bool {
        match self.get(key) {
            Some(Value::Bool(b)) => b,
            Some(Value::String(s)) => !(s.is_empty() || s == "0"),
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
            _ => false,
        }
    }

    pub fn get_string_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Array(a)) => a
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    Value::Null => None,
                    other => Some(other.to_string()),
                })
                .collect(),
            Some(Value::String(s)) if !s.is_empty() => vec![s],
            _ => Vec::new(),
        }
    }

    pub fn exists(&self, key: &str) -> bool {
        self.values.read().contains_key(key) || self.defaults.contains_key(key)
    }

    /// Set a value unless one is stored already (`Slim::Utils::Prefs::Namespace::init`).
    pub fn init(&self, key: &str, value: Value) {
        {
            let mut values = self.values.write();
            if values.contains_key(key) {
                return;
            }
            values.insert(key.to_string(), value);
        }
        self.save();
    }

    /// Set a value and persist.  Returns the previous value.
    pub fn set(&self, key: &str, value: Value) -> Option<Value> {
        let old = self.values.write().insert(key.to_string(), value);
        self.save();
        old
    }

    pub fn remove(&self, key: &str) {
        self.values.write().remove(key);
        self.save();
    }

    pub fn all(&self) -> PrefMap {
        let mut m = self.defaults.clone();
        m.extend(self.values.read().clone());
        m
    }

    pub fn save(&self) {
        let values = self.values.read().clone();
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_yaml::to_string(&values) {
            Ok(text) => {
                let tmp = self.path.with_extension("prefs.tmp");
                if let Err(e) = std::fs::write(&tmp, format!("---\n{}", text))
                    .and_then(|_| std::fs::rename(&tmp, &self.path))
                {
                    warn!("could not write {}: {}", self.path.display(), e);
                }
            }
            Err(e) => warn!("could not serialise prefs: {}", e),
        }
    }
}

/// All preference namespaces.
pub struct Prefs {
    dir: PathBuf,
    pub server: PrefStore,
    players: RwLock<BTreeMap<String, Arc<PrefStore>>>,
}

impl Prefs {
    pub fn open(dir: &Path) -> Prefs {
        std::fs::create_dir_all(dir).ok();
        let server = PrefStore::load(dir.join("server.prefs"), server_defaults());
        if server.get_str("server_uuid").is_empty() {
            server.set(
                "server_uuid",
                json!(uuid::Uuid::new_v4().simple().to_string()),
            );
        }
        if server.get_str("securitySecret").is_empty() {
            server.set(
                "securitySecret",
                json!(uuid::Uuid::new_v4().simple().to_string()),
            );
        }
        info!("prefs directory: {}", dir.display());
        Prefs {
            dir: dir.to_path_buf(),
            server,
            players: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Per-player prefs, created on first use.
    pub fn player(&self, id: &str) -> Arc<PrefStore> {
        if let Some(p) = self.players.read().get(id) {
            return p.clone();
        }
        let safe: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let store = Arc::new(PrefStore::load(
            self.dir.join(format!("{}.prefs", safe)),
            player_defaults(),
        ));
        self.players.write().insert(id.to_string(), store.clone());
        store
    }

    /// Ids of players that have a prefs file on disk (used to list "known" players).
    pub fn known_players(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = name.strip_suffix(".prefs") {
                    if stem.len() == 17 && stem.matches('_').count() == 5 {
                        ids.push(stem.replace('_', ":"));
                    }
                }
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = Prefs::open(dir.path());
        assert_eq!(prefs.server.get_i64("httpport"), 9000);
        prefs.server.set("httpport", json!(9001));
        prefs.player("00:04:20:aa:bb:cc").set("volume", json!(33));
        let again = Prefs::open(dir.path());
        assert_eq!(again.server.get_i64("httpport"), 9001);
        assert_eq!(again.player("00:04:20:aa:bb:cc").get_i64("volume"), 33);
        assert_eq!(again.known_players(), vec!["00:04:20:aa:bb:cc"]);
        assert!(!again.server.get_str("server_uuid").is_empty());
    }
}
