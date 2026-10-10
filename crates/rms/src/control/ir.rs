//! IR remote and hardware buttons (`Slim::Hardware::IR`, default IR map subset).
//!
//! Only the playback-relevant buttons of the standard Slim Devices remote are mapped; menu
//! navigation needs a player display, which the supported players (squeezelite, WiiM,
//! SqueezePlay) do not have.

use crate::player::Player;
use crate::server::Server;
use std::sync::Arc;
use tracing::debug;

/// IR codes of the Slim Devices remote (`IR/Slim_Devices_Remote.ir`, NEC 0x7689xxxx).
fn button_for_code(code: u32) -> Option<&'static str> {
    Some(match code {
        0x768910ef => "play",
        0x768920df => "pause",
        0x7689c03f => "rew",
        0x7689a05f => "fwd",
        0x7689807f => "volup",
        0x768900ff => "voldown",
        0x768940bf => "power",
        0x7689c43b => "muting",
        0x7689d827 => "shuffle",
        0x768938c7 => "repeat",
        0x7689b847 => "sleep",
        _ => return None,
    })
}

pub fn handle_ir(server: &Arc<Server>, player: &Arc<Player>, code: u32, time: u32) {
    match button_for_code(code) {
        Some(b) => execute_button(server, player, b),
        None => {
            debug!("{}: unknown IR code {:08x}", player.id, code);
            server.notifier.notify_owned(Some(player.id), vec!["unknownir".into(), format!("{:08x}", code), time.to_string()]);
        }
    }
}

/// Hardware buttons (`BUTN`, Transporter/Boom front panel).
pub fn handle_button(server: &Arc<Server>, player: &Arc<Player>, button: u32, _time: u32) {
    // `IR/Front_Panel.ir` (`.down` codes)
    let name = match button {
        0x00010012 => "play",
        0x00010017 => "pause",
        0x00010010 => "rew",
        0x00010011 => "fwd",
        0x0001000a => "power",
        0x00010019 => "volup",
        0x0001001a => "voldown",
        _ => return debug!("{}: unknown button {:08x}", player.id, button),
    };
    execute_button(server, player, name);
}

/// Execute a named button as the default IR map would.
pub fn execute_button(server: &Arc<Server>, player: &Arc<Player>, name: &str) {
    let base = name.split('.').next().unwrap_or(name);
    let cmd: Option<Vec<&str>> = match base {
        "play" => Some(vec!["play"]),
        "pause" => Some(vec!["pause"]),
        "stop" => Some(vec!["stop"]),
        "fwd" | "jump_fwd" => Some(vec!["playlist", "index", "+1"]),
        "rew" | "jump_rew" => {
            // within the first seconds of a track: previous track, else restart
            let t = player.controller().lock().playing_song_elapsed();
            if t < 5.0 {
                Some(vec!["playlist", "index", "-1"])
            } else {
                Some(vec!["time", "0"])
            }
        }
        "volup" | "volume_up" => Some(vec!["mixer", "volume", "+2"]),
        "voldown" | "volume_down" => Some(vec!["mixer", "volume", "-2"]),
        "muting" | "mute" => Some(vec!["mixer", "muting"]),
        "power" => Some(vec!["power"]),
        "power_on" => Some(vec!["power", "1"]),
        "power_off" => Some(vec!["power", "0"]),
        "shuffle" | "shuffle_toggle" => Some(vec!["playlist", "shuffle"]),
        "repeat" | "repeat_toggle" => Some(vec!["playlist", "repeat"]),
        _ => None,
    };
    let Some(cmd) = cmd else {
        debug!("{}: button {} not mapped", player.id, name);
        return;
    };
    let server = server.clone();
    let id = player.id;
    let tokens: Vec<String> = cmd.into_iter().map(str::to_string).collect();
    tokio::spawn(async move {
        super::dispatch::execute_tokens(&server, Some(id), tokens, super::request::Source::Internal).await;
    });
}
