//! Server and player settings.  Changes run as `pref` / `playerpref` commands, so the
//! same things happen as when a Jive client or the CLI changes them (library settings
//! reconfigure the library, `prefset` notifications go out).

use super::commands::{exec_for, CommandError};
use super::model::{
    Choice, ChoiceOption, DisplaySettings, ListenBrainzAccount, PlayerSettings,
    PlayerSettingsPatch, Range, ScanMode, ServerSettings, ServerSettingsPatch,
};
use crate::display::Kind;
use crate::player::Player;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn server_settings(server: &Server) -> ServerSettings {
    let prefs = &server.prefs.server;
    let bitrate = prefs.get_i64("spotifyBitrate");
    let mb = server.library.musicbrainz_totals();
    let albums = server.library.release_totals();
    let d = server.library.discovery_totals();
    let (pending, scrobble_error) = crate::plugins::scrobble::pending(server);
    let effnet = crate::plugins::effnet::status();
    let (effnet_analyzed, effnet_pending) = server.library.effnet_totals();
    let viz = visualizer_settings(server);
    ServerSettings {
        library_name: prefs.get_str("libraryname"),
        media_dirs: prefs.get_string_list("mediadirs"),
        watch_folders: prefs.get_bool("autorescan"),
        upnp_players: prefs.get_bool("bridgeUpnp"),
        chromecast_players: prefs.get_bool("bridgeChromecast"),
        airplay_players: prefs.get_bool("bridgeAirplay"),
        radio_direct: prefs.get_bool("radioDirect"),
        art_folder: prefs.get_str("artfolder"),
        online_artist_images: prefs.get_bool("onlineArtistImages"),
        spotify_client_id: prefs.get_str("spotifyClientId"),
        spotify_bitrate: if bitrate > 0 { bitrate } else { 320 },
        spotify_normalisation: prefs.get_bool("spotifyNormalisation"),
        musicbrainz: prefs.get_bool(crate::plugins::musicbrainz::PREF),
        acoustid_key: prefs.get_str(crate::plugins::musicbrainz::KEY_PREF),
        musicbrainz_identified: mb.identified,
        musicbrainz_pending: mb.pending,
        albums_matched: albums.matched,
        albums_uncertain: albums.uncertain,
        albums_pending: albums.pending,
        edit_files: prefs.get_bool(crate::tagedit::PREF),
        edit_files_blocked: crate::tagedit::blocked(server),
        file_name_template: crate::tagedit::template(server),
        listenbrainz: prefs.get_bool(crate::plugins::listenbrainz::PREF),
        discovered: d.proposed,
        discovered_played: d.played,
        discovered_rejected: d.rejected,
        listenbrainz_accounts: crate::plugins::scrobble::accounts(server)
            .into_iter()
            .map(|a| ListenBrainzAccount {
                user: a.user,
                default: a.default,
            })
            .collect(),
        scrobble_spotify: prefs.get_bool(crate::plugins::scrobble::SPOTIFY),
        scrobble_pending: pending,
        scrobble_error,
        effnet: prefs.get_bool(crate::plugins::effnet::PREF),
        effnet_status: match &effnet {
            crate::plugins::effnet::Status::Off => "off",
            crate::plugins::effnet::Status::Downloading(..) => "downloading",
            crate::plugins::effnet::Status::Ready => "ready",
            crate::plugins::effnet::Status::Failed(_) => "failed",
        }
        .into(),
        effnet_error: match effnet {
            crate::plugins::effnet::Status::Failed(e) => Some(e),
            _ => None,
        },
        effnet_analyzed,
        effnet_pending,
        visualizer_available: viz.available,
        visualizer_error: viz.error,
        visualizer_encoder: viz.encoder,
        visualizer_size: viz.size,
        visualizer_render_size: viz.render_size,
        visualizer_fps: viz.fps,
        visualizer_quality: viz.quality,
        visualizer_preset_seconds: viz.preset_seconds,
        visualizer_receiver_app: prefs.get_str("visualizerReceiverApp").trim().to_string(),
    }
}

/// Video sizes, draw sizes and frame rates the settings offer.
const VIZ_SIZES: &[(&str, &str)] = &[
    ("1920x1080", "1080p (1920×1080)"),
    ("1600x900", "900p (1600×900)"),
    ("1280x720", "720p (1280×720)"),
];
const VIZ_RENDER_SIZES: &[(&str, &str)] = &[
    ("", "The video size"),
    ("1600x900", "1600×900"),
    ("1280x720", "1280×720"),
    ("960x540", "960×540"),
];
const VIZ_FPS: &[(&str, &str)] = &[("24", "24"), ("30", "30"), ("60", "60")];

/// The visualiser part of the server settings.
struct VizSettings {
    available: bool,
    error: Option<String>,
    encoder: String,
    size: Choice,
    render_size: Choice,
    fps: Choice,
    quality: Range,
    preset_seconds: Range,
}

fn visualizer_settings(server: &Server) -> VizSettings {
    let prefs = &server.prefs.server;
    let (cfg, error) = match crate::viz::config(server) {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    };
    let size = cfg
        .as_ref()
        .map(|c| c.size.clone())
        .unwrap_or_else(|| crate::viz::DEFAULT_SIZE.into());
    let render_size = Some(prefs.get_str("visualizerRenderSize"))
        .filter(|s| crate::viz::valid_size(s))
        .unwrap_or_default();
    let fps = cfg
        .as_ref()
        .map(|c| c.fps)
        .unwrap_or(crate::viz::DEFAULT_FPS);
    let opts = |list: &[(&str, &str)]| -> Vec<(String, String)> {
        list.iter()
            .map(|(v, l)| (v.to_string(), l.to_string()))
            .collect()
    };
    VizSettings {
        available: cfg.is_some(),
        error,
        encoder: cfg.as_ref().map(|c| c.encoder.clone()).unwrap_or_default(),
        size: choice(size, opts(VIZ_SIZES)),
        render_size: choice(render_size, opts(VIZ_RENDER_SIZES)),
        fps: choice(fps.to_string(), opts(VIZ_FPS)),
        quality: Range {
            value: cfg
                .as_ref()
                .map(|c| c.quality as i64)
                .unwrap_or(crate::viz::DEFAULT_QUALITY as i64),
            min: 18,
            max: 45,
        },
        preset_seconds: Range {
            value: cfg
                .as_ref()
                .map(|c| c.preset_seconds as i64)
                .unwrap_or(crate::viz::DEFAULT_PRESET_SECONDS as i64),
            min: 5,
            max: 600,
        },
    }
}

async fn set_pref(
    server: &Arc<Server>,
    p: Option<&Arc<Player>>,
    name: &str,
    value: String,
) -> Result<(), CommandError> {
    let verb = if p.is_some() { "playerpref" } else { "pref" };
    exec_for(server, p, vec![verb.into(), name.into(), value]).await?;
    Ok(())
}

fn flag(b: bool) -> String {
    (b as i32).to_string()
}

pub async fn update_server_settings(
    server: &Arc<Server>,
    patch: ServerSettingsPatch,
) -> Result<ServerSettings, CommandError> {
    if let Some(b) = patch.spotify_bitrate {
        if ![96, 160, 320].contains(&b) {
            return Err(CommandError::Invalid(
                "spotify_bitrate must be 96, 160 or 320".into(),
            ));
        }
    }
    if let Some(id) = patch.spotify_client_id.as_deref().map(str::trim) {
        if !id.is_empty() && (id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(CommandError::Invalid(
                "spotify_client_id must be 32 hexadecimal digits".into(),
            ));
        }
    }
    let mut changes: Vec<(&str, String)> = Vec::new();
    if let Some(v) = patch.library_name {
        changes.push(("libraryname", v.trim().to_string()));
    }
    if let Some(dirs) = patch.media_dirs {
        let mut unique: Vec<String> = Vec::new();
        for d in dirs.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
            if !unique.iter().any(|u| u == d) {
                unique.push(d.to_string());
            }
        }
        changes.push(("mediadirs", json!(unique).to_string()));
    }
    if let Some(v) = patch.watch_folders {
        changes.push(("autorescan", flag(v)));
    }
    for (v, pref) in [
        (patch.upnp_players, "bridgeUpnp"),
        (patch.chromecast_players, "bridgeChromecast"),
        (patch.radio_direct, "radioDirect"),
        (patch.airplay_players, "bridgeAirplay"),
    ] {
        if let Some(v) = v {
            changes.push((pref, flag(v)));
        }
    }
    if let Some(v) = patch.art_folder {
        changes.push(("artfolder", v.trim().to_string()));
    }
    if let Some(v) = patch.online_artist_images {
        changes.push(("onlineArtistImages", flag(v)));
    }
    if let Some(v) = patch.spotify_client_id {
        changes.push(("spotifyClientId", v.trim().to_string()));
    }
    if let Some(v) = patch.spotify_bitrate {
        changes.push(("spotifyBitrate", v.to_string()));
    }
    if let Some(v) = patch.spotify_normalisation {
        changes.push(("spotifyNormalisation", flag(v)));
    }
    if let Some(v) = patch.musicbrainz {
        changes.push((crate::plugins::musicbrainz::PREF, flag(v)));
    }
    if let Some(v) = patch.edit_files {
        changes.push((crate::tagedit::PREF, flag(v)));
    }
    if let Some(v) = patch.file_name_template {
        let v = v.trim().to_string();
        if !v.is_empty() {
            rms_library::organize::Template::parse(&v).map_err(CommandError::Invalid)?;
        }
        changes.push((crate::tagedit::TEMPLATE_PREF, v));
    }
    if let Some(v) = patch.scrobble_spotify {
        changes.push((crate::plugins::scrobble::SPOTIFY, flag(v)));
    }
    if let Some(v) = patch.effnet {
        changes.push((crate::plugins::effnet::PREF, flag(v)));
    }
    if let Some(v) = patch.listenbrainz {
        changes.push((crate::plugins::listenbrainz::PREF, flag(v)));
    }
    if let Some(v) = patch.acoustid_key {
        changes.push((crate::plugins::musicbrainz::KEY_PREF, v.trim().to_string()));
    }
    let one_of = |list: &[(&str, &str)], v: String, field: &str| {
        if list.iter().any(|(o, _)| *o == v) {
            Ok(v)
        } else {
            Err(CommandError::Invalid(format!("{v} is not a valid {field}")))
        }
    };
    if let Some(v) = patch.visualizer_size {
        changes.push(("visualizerSize", one_of(VIZ_SIZES, v, "visualizer_size")?));
    }
    if let Some(v) = patch.visualizer_render_size {
        changes.push((
            "visualizerRenderSize",
            one_of(VIZ_RENDER_SIZES, v, "visualizer_render_size")?,
        ));
    }
    if let Some(v) = patch.visualizer_fps {
        changes.push(("visualizerFps", one_of(VIZ_FPS, v, "visualizer_fps")?));
    }
    if let Some(v) = patch.visualizer_quality {
        changes.push((
            "visualizerQuality",
            within(v, 18, 45, "visualizer_quality")?,
        ));
    }
    if let Some(v) = patch.visualizer_receiver_app {
        let v = v.trim().to_ascii_uppercase();
        if !v.is_empty() && !crate::viz::valid_app_id(&v) {
            return Err(CommandError::Invalid(
                "visualizer_receiver_app: a Cast app id is 8 hex digits, like 1A2B3C4D".into(),
            ));
        }
        changes.push(("visualizerReceiverApp", v));
    }
    if let Some(v) = patch.visualizer_preset_seconds {
        changes.push((
            "visualizerPresetSeconds",
            within(v, 5, 600, "visualizer_preset_seconds")?,
        ));
    }
    for (name, value) in changes {
        set_pref(server, None, name, value).await?;
    }
    Ok(server_settings(server))
}

pub async fn scan(server: &Arc<Server>, mode: ScanMode) -> Result<(), CommandError> {
    let words: &[&str] = match mode {
        ScanMode::Changes => &["rescan"],
        ScanMode::Playlists => &["rescan", "playlists"],
        ScanMode::Full => &["wipecache"],
    };
    exec_for(server, None, words.iter().map(|w| w.to_string()).collect()).await?;
    Ok(())
}

/// A pref value as a string: `"3"` for 3, `"abc"` for "abc", "" when unset.
fn pref_string(p: &Player, name: &str) -> String {
    match p.prefs.get(name) {
        Some(Value::String(s)) => s,
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

fn choice(value: String, options: impl IntoIterator<Item = (String, String)>) -> Choice {
    Choice {
        value,
        options: options
            .into_iter()
            .map(|(value, label)| ChoiceOption { value, label })
            .collect(),
    }
}

fn kind(p: &Player) -> Option<Kind> {
    Kind::for_model(&p.state.lock().caps.model)
}

pub fn player_settings(server: &Arc<Server>, p: &Arc<Player>) -> PlayerSettings {
    let prefs = &p.prefs;
    let master = p.controller().lock().master().unwrap_or_else(|| p.clone());
    let dstm = crate::plugins::dstm::provider(&master).unwrap_or_else(|| "0".into());
    let mut providers = vec![("0".to_string(), s("DISABLED"))];
    providers.extend(crate::plugins::dstm::providers(server));
    PlayerSettings {
        transition_type: prefs.get_i64("transitionType"),
        transition_duration: prefs.get_i64("transitionDuration"),
        play_delay: prefs.get_i64("playDelay"),
        replay_gain_mode: prefs.get_i64("replayGainMode"),
        max_bitrate: prefs.get_i64("maxBitrate"),
        digital_volume_control: prefs.get_bool("digitalVolumeControl"),
        dont_stop_the_music: Some(choice(dstm, providers)),
        adventurous: crate::plugins::listenbrainz::adventurous(p),
        scrobble_to: crate::plugins::scrobble::player_accounts(server, p)
            .into_iter()
            .map(|a| a.user)
            .collect(),
        display: display_settings(p),
        visualizer: visualizer_choice(p, crate::viz::receiver_app(server).is_some()),
    }
}

/// A Chromecast's visualiser setting, with what automatic means for it.
fn visualizer_choice(p: &Player, receiver: bool) -> Option<Choice> {
    let screen = crate::viz::screen(&p.id)?;
    let value = match p.prefs.get("visualizer") {
        None | Some(Value::Null) => "auto".to_string(),
        Some(Value::String(s)) if s.is_empty() || s == "auto" => "auto".to_string(),
        Some(Value::String(s)) if s == "tv" => "tv".to_string(),
        Some(_) => flag(p.prefs.get_bool("visualizer")),
    };
    let auto = match screen {
        Some(true) => "Automatic (video: it has a screen)",
        Some(false) => "Automatic (off: no screen)",
        None => "Automatic (off: added by address, screen unknown)",
    };
    let tv = if receiver {
        "Drawn on the TV"
    } else {
        "Drawn on the TV (set up the receiver app first)"
    };
    Some(choice(
        value,
        [
            ("auto".to_string(), auto.to_string()),
            ("1".to_string(), "Video from the server".to_string()),
            ("tv".to_string(), tv.to_string()),
            ("0".to_string(), "Off".to_string()),
        ],
    ))
}

fn display_settings(p: &Player) -> Option<DisplaySettings> {
    let kind = kind(p)?;
    let modes: Vec<(String, String)> = p
        .prefs
        .get_string_list("playingDisplayModes")
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let label = m
                .parse::<usize>()
                .map(|m| crate::display::mode_description(kind, m))
                .unwrap_or_default();
            (i.to_string(), label)
        })
        .collect();
    let fonts = |list: &str| {
        let fonts = p.prefs.get_string_list(list);
        choice(
            pref_string(p, &format!("{list}_curr")),
            fonts
                .iter()
                .enumerate()
                .map(|(i, f)| (i.to_string(), s(&f.to_uppercase()))),
        )
    };
    let levels = crate::display::brightness_options(p);
    let brightness = |pref: &str| {
        choice(
            p.prefs.get_i64(pref).to_string(),
            levels.iter().map(|(v, l)| (v.to_string(), l.clone())),
        )
    };
    let boom = p.state.lock().caps.model == "boom";
    let range = |pref: &str, min, max| {
        boom.then(|| Range {
            value: p.prefs.get_i64(pref),
            min,
            max,
        })
    };
    Some(DisplaySettings {
        playing_mode: choice(pref_string(p, "playingDisplayMode"), modes),
        text_size: fonts("activeFont"),
        text_size_off: fonts("idleFont"),
        brightness_on: brightness("powerOnBrightness"),
        brightness_off: brightness("powerOffBrightness"),
        brightness_idle: brightness("idleBrightness"),
        auto_brightness_min: range("minAutoBrightness", 1, 5),
        auto_brightness_sensitivity: range("sensAutoBrightness", 1, 20),
    })
}

/// `value` must be one of `c`'s options.
fn pick(c: Option<&Choice>, field: &str, value: String) -> Result<String, CommandError> {
    match c {
        Some(c) if c.options.iter().any(|o| o.value == value) => Ok(value),
        Some(_) => Err(CommandError::Invalid(format!(
            "{} is not a valid {}",
            value, field
        ))),
        None => Err(CommandError::Invalid(format!(
            "this player has no {}",
            field
        ))),
    }
}

fn within(v: i64, min: i64, max: i64, field: &str) -> Result<String, CommandError> {
    if (min..=max).contains(&v) {
        Ok(v.to_string())
    } else {
        Err(CommandError::Invalid(format!(
            "{} must be {}–{}",
            field, min, max
        )))
    }
}

pub async fn update_player_settings(
    server: &Arc<Server>,
    p: &Arc<Player>,
    patch: PlayerSettingsPatch,
) -> Result<PlayerSettings, CommandError> {
    let cur = player_settings(server, p);
    let d = cur.display.as_ref();
    let range = |r: Option<&Range>, v: i64, field: &str| match r {
        Some(r) => within(v, r.min, r.max, field),
        None => Err(CommandError::Invalid(format!(
            "this player has no {}",
            field
        ))),
    };
    // validate everything before changing anything
    let mut changes: Vec<(&str, String)> = Vec::new();
    if let Some(v) = patch.transition_type {
        changes.push(("transitionType", within(v, 0, 4, "transition_type")?));
    }
    if let Some(v) = patch.transition_duration {
        changes.push((
            "transitionDuration",
            within(v, 0, 10, "transition_duration")?,
        ));
    }
    if let Some(v) = patch.replay_gain_mode {
        changes.push(("replayGainMode", within(v, 0, 3, "replay_gain_mode")?));
    }
    if let Some(v) = patch.play_delay {
        changes.push(("playDelay", within(v, 0, 1000, "play_delay")?));
    }
    if let Some(v) = patch.max_bitrate {
        changes.push(("maxBitrate", within(v, 0, 10_000, "max_bitrate")?));
    }
    if let Some(v) = patch.digital_volume_control {
        changes.push(("digitalVolumeControl", flag(v)));
    }
    if let Some(v) = patch.adventurous {
        changes.push((
            crate::plugins::listenbrainz::ADVENTUROUS,
            within(
                v,
                0,
                crate::plugins::listenbrainz::MAX_ADVENTUROUS,
                "adventurous",
            )?,
        ));
    }
    if let Some(v) = patch.dont_stop_the_music {
        let v = pick(cur.dont_stop_the_music.as_ref(), "dont_stop_the_music", v)?;
        changes.push((crate::plugins::dstm::PREF, v));
    }
    if let Some(v) = patch.playing_mode {
        changes.push((
            "playingDisplayMode",
            pick(d.map(|d| &d.playing_mode), "playing_mode", v)?,
        ));
    }
    if let Some(v) = patch.text_size {
        changes.push((
            "activeFont_curr",
            pick(d.map(|d| &d.text_size), "text_size", v)?,
        ));
    }
    if let Some(v) = patch.text_size_off {
        changes.push((
            "idleFont_curr",
            pick(d.map(|d| &d.text_size_off), "text_size_off", v)?,
        ));
    }
    for (pref, v, c, field) in [
        (
            "powerOnBrightness",
            patch.brightness_on,
            d.map(|d| &d.brightness_on),
            "brightness_on",
        ),
        (
            "powerOffBrightness",
            patch.brightness_off,
            d.map(|d| &d.brightness_off),
            "brightness_off",
        ),
        (
            "idleBrightness",
            patch.brightness_idle,
            d.map(|d| &d.brightness_idle),
            "brightness_idle",
        ),
    ] {
        if let Some(v) = v {
            changes.push((pref, pick(c, field, v)?));
        }
    }
    if let Some(v) = patch.auto_brightness_min {
        let r = d.and_then(|d| d.auto_brightness_min.as_ref());
        changes.push(("minAutoBrightness", range(r, v, "auto_brightness_min")?));
    }
    if let Some(v) = patch.visualizer {
        let v = pick(cur.visualizer.as_ref(), "visualizer", v)?;
        changes.push(("visualizer", if v == "auto" { String::new() } else { v }));
    }
    if let Some(v) = patch.auto_brightness_sensitivity {
        let r = d.and_then(|d| d.auto_brightness_sensitivity.as_ref());
        changes.push((
            "sensAutoBrightness",
            range(r, v, "auto_brightness_sensitivity")?,
        ));
    }
    let scrobble_to = match patch.scrobble_to {
        Some(to) => {
            let known = crate::plugins::scrobble::accounts(server);
            if let Some(u) = to.iter().find(|u| !known.iter().any(|a| &a.user == *u)) {
                return Err(CommandError::Invalid(format!(
                    "scrobble_to: no ListenBrainz account {u}"
                )));
            }
            Some(to)
        }
        None => None,
    };
    for (name, value) in changes {
        set_pref(server, Some(p), name, value).await?;
    }
    if let Some(to) = scrobble_to {
        p.prefs
            .set(crate::plugins::scrobble::SCROBBLE_TO, serde_json::json!(to));
    }
    Ok(player_settings(server, p))
}
