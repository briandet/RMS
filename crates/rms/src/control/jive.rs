//! Jive / SqueezePlay menu protocol (`Slim::Control::Jive`, `Slim::Menu::*`).
//! Placeholder registration — the full menu implementation lives in this module.

use super::dispatch::DispatchTable;
use super::request::Request;
use crate::server::Server;
use std::sync::Arc;

pub fn register(_t: &mut DispatchTable) {}

/// `contextmenu <start> <count> menu:<kind> …` → `<kind>info items …`.
pub async fn contextmenu(_server: Arc<Server>, mut r: Request) -> Request {
    r.set_done();
    r
}

/// `favorites items … menu:…` (Jive form).  Implemented with the menu system.
pub fn favorites_menu(_server: &Arc<Server>, r: &mut Request) {
    r.set_done();
}
