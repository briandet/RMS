//! Built-in plugins: Favorites, Alarm clock, Internet radio.

pub mod alarm;
pub mod favorites;

use crate::control::dispatch::DispatchTable;

pub fn register(t: &mut DispatchTable) {
    favorites::register(t);
}
