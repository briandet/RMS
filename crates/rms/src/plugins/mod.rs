//! Built-in plugins: Favorites, Alarm clock, Internet radio, online artist pictures.

pub mod alarm;
pub mod artist_images;
pub mod favorites;

use crate::control::dispatch::DispatchTable;

pub fn register(t: &mut DispatchTable) {
    favorites::register(t);
}
