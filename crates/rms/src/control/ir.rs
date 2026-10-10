//! IR remote and hardware buttons (`Slim::Hardware::IR::enqueue`): codes and `button`
//! commands go to the player's UI ([`crate::ui`]), which applies Lyrion's IR timing and
//! button maps in the player's current mode.

use crate::player::Player;
use crate::server::Server;
use std::sync::Arc;

/// An `IR  ` frame (or `ir` command): code and player time (1 kHz ticks).  Time 0 (from
/// the CLI) uses the server clock.
pub fn handle_ir(server: &Arc<Server>, player: &Arc<Player>, code: u32, time: u32) {
    let time = if time == 0 {
        (crate::util::now() * 1000.0) as u64 as u32
    } else {
        time
    };
    crate::ui::send(server, player, crate::ui::Event::Ir { code, time });
}

/// Hardware buttons (`BUTN`, Transporter/Boom front panel), handled like IR codes.
pub fn handle_button(server: &Arc<Server>, player: &Arc<Player>, button: u32, time: u32) {
    handle_ir(server, player, button, time);
}

/// The `button` command: a button name, or a function name (`orFunction`).
pub fn execute_button(server: &Arc<Server>, player: &Arc<Player>, name: &str) {
    crate::ui::send(server, player, crate::ui::Event::Button(name.to_string()));
}
