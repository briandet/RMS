//! Built-in plugins: Favorites, Alarm clock, Internet radio, online artist pictures.

pub mod alarm;
pub mod artist_images;
pub mod favorites;
pub mod spotify;

use crate::control::dispatch::DispatchTable;

pub fn register(t: &mut DispatchTable) {
    favorites::register(t);
    spotify::menu::register(t);
}
