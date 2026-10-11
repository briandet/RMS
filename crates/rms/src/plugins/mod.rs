//! Built-in plugins: Favorites, Alarm clock, Internet radio, online artist pictures,
//! Spotify, Don't Stop The Music.

pub mod alarm;
pub mod artist_images;
pub mod dstm;
pub mod favorites;
pub mod spotify;

use crate::control::dispatch::DispatchTable;

pub fn register(t: &mut DispatchTable) {
    favorites::register(t);
    dstm::register(t);
    spotify::menu::register(t);
}
