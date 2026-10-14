//! The end-to-end tests, one binary: each file was its own, and every test binary is
//! compiled and linked separately.

// tokio's spawn functions are fine here (crates/rms/clippy.toml is about the server's code size)
#![allow(clippy::disallowed_methods)]

#[macro_use]
mod common;

mod airplay_player;
mod analysis;
mod browser_player;
mod chromecast_player;
mod conformance;
mod cover_art;
mod effnet;
mod fake_player;
mod file_moving;
mod https;
mod listenbrainz;
mod loose_tracks;
mod mix_eval;
mod musicbrainz;
mod playlists;
mod release_art;
mod releases;
mod scrobble;
mod spectrum;
mod tag_editing;
mod upload;
mod upnp_player;
mod watch;
