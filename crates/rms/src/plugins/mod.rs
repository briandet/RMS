//! Built-in plugins: Favorites, Alarm clock, Internet radio (with Radio Browser), online artist pictures,
//! Spotify, Don't Stop The Music, sonic analysis.

pub mod alarm;
pub mod artist_images;
pub mod coverart;
pub mod dstm;
pub mod effnet;
pub mod favorites;
pub mod listenbrainz;
pub mod musicbrainz;
pub mod radiobrowser;
pub mod scrobble;
pub mod sonic;
pub mod spotify;

use crate::control::dispatch::DispatchTable;

pub fn register(t: &mut DispatchTable) {
    favorites::register(t);
    dstm::register(t);
    spotify::menu::register(t);
    radiobrowser::register(t);
}
