//! Types of the native API.  Everything here is part of the wire format: field names are
//! `snake_case`, enums are lower-case strings, absent values are omitted.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub uuid: String,
    /// Present while a library scan runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanProgress>,
    /// Unix time the last scan finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scan: Option<i64>,
    pub library: LibraryTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct ScanProgress {
    /// Current step, e.g. `directory_new`, `post_processing`; empty while starting.
    pub step: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct LibraryTotals {
    pub albums: u64,
    pub artists: u64,
    pub genres: u64,
    pub tracks: u64,
    /// Seconds.
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(optional_fields)]
#[serde(rename_all = "lowercase")]
pub enum PlayMode {
    Play,
    Pause,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
#[serde(rename_all = "lowercase")]
pub enum Shuffle {
    Off,
    Songs,
    Albums,
}

impl Shuffle {
    pub fn from_lms(v: u8) -> Shuffle {
        match v {
            1 => Shuffle::Songs,
            2 => Shuffle::Albums,
            _ => Shuffle::Off,
        }
    }

    pub fn to_lms(self) -> u8 {
        match self {
            Shuffle::Off => 0,
            Shuffle::Songs => 1,
            Shuffle::Albums => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(optional_fields)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    Off,
    One,
    All,
}

impl Repeat {
    pub fn from_lms(v: u8) -> Repeat {
        match v {
            1 => Repeat::One,
            2 => Repeat::All,
            _ => Repeat::Off,
        }
    }

    pub fn to_lms(self) -> u8 {
        match self {
            Repeat::Off => 0,
            Repeat::One => 1,
            Repeat::All => 2,
        }
    }
}

/// A player and everything a remote control shows about it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Player {
    /// MAC address, e.g. `00:04:20:12:34:56`.
    pub id: String,
    pub name: String,
    /// Model id (`squeezelite`, `baby`, `fab4`, …).
    pub model: String,
    pub model_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub firmware: String,
    pub connected: bool,
    pub power: bool,
    pub mode: PlayMode,
    /// Playing, but waiting for the buffer or for synced players: the position is not moving.
    pub buffering: bool,
    /// Seconds into the current track when this state was produced.  While `mode` is
    /// `play` and not `buffering`, clients advance it with their own clock.
    pub elapsed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    pub can_seek: bool,
    /// 0–100.
    pub volume: i32,
    pub muted: bool,
    /// The player outputs a fixed level (digital volume control off on a digital output).
    pub volume_fixed: bool,
    pub shuffle: Shuffle,
    pub repeat: Repeat,
    pub queue: QueueInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now_playing: Option<Track>,
    /// Ids of the other players in this player's sync group.
    pub synced_with: Vec<String>,
    /// Seconds until the sleep timer turns the player off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sleep: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct QueueInfo {
    /// Changes whenever the queue's contents or order change.
    pub version: f64,
    pub length: usize,
    /// Index of the current track; absent when the queue is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// Name of the saved playlist the queue was loaded from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Track {
    /// Library track id; absent for streams and services.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Artwork path relative to the server root (`music/<id>/cover`,
    /// `imageproxy/<url>/image`).  Append `_<w>x<h>_o` for a resized copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    /// Internet radio, a service track or another remote URL.
    pub remote: bool,
    /// Station name, for a radio stream that reports the current song as its title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    /// Content type (`flc`, `mp3`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Queue {
    pub version: f64,
    pub length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    pub offset: usize,
    pub items: Vec<Track>,
}

/// Something that can be queued or used to seed a mix.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[ts(optional_fields)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Item {
    Track {
        id: i64,
    },
    Album {
        id: i64,
    },
    Artist {
        id: i64,
    },
    Genre {
        id: i64,
    },
    Year {
        year: i64,
    },
    Playlist {
        id: i64,
    },
    Folder {
        id: i64,
    },
    Url {
        url: String,
    },
    /// A favorite or a folder of favorites (`BrowseItem` id from `/favorites`).
    Favorite {
        id: String,
    },
    /// A Spotify track, album, playlist, … (`BrowseItem` id from `/spotify/items`).
    Spotify {
        id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default, TS)]
#[ts(optional_fields)]
#[serde(rename_all = "lowercase")]
pub enum EnqueueMode {
    /// Replace the queue and start playing.
    #[default]
    Play,
    /// Append to the queue.
    Add,
    /// Insert after the current track.
    Next,
}

/// `POST /api/v1/players/{id}/command` body.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[ts(optional_fields)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Start playing, or resume when paused.
    Play,
    Pause,
    /// Pause when playing, play otherwise.
    TogglePause,
    Stop,
    Next,
    Previous,
    Seek {
        position: f64,
    },
    Volume {
        value: i32,
    },
    /// Without `muted`, toggles.
    Mute {
        #[serde(default)]
        muted: Option<bool>,
    },
    Power {
        on: bool,
    },
    Shuffle {
        mode: Shuffle,
    },
    Repeat {
        mode: Repeat,
    },
    /// Play the queue entry at `index`.
    Jump {
        index: usize,
    },
    Move {
        from: usize,
        to: usize,
    },
    Remove {
        index: usize,
    },
    Clear,
    Enqueue {
        item: Item,
        #[serde(default)]
        #[ts(as = "Option<EnqueueMode>", optional)]
        mode: EnqueueMode,
        /// With `mode: play`, the entry of the new queue to start at.
        #[serde(default)]
        start: Option<usize>,
    },
    /// Replace the queue with songs similar to `seed` (Create Mix) and play.
    Mix {
        seed: Item,
    },
    /// Join `with`'s sync group.
    Sync {
        with: String,
    },
    Unsync,
    Rename {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Default, TS)]
#[ts(optional_fields)]
pub struct CommandResult {
    /// Tracks added by `enqueue` / `mix`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// Messages on `GET /api/v1/events`.  The SSE event name is the `type`.
/// Short-lived (serialized once per change), so variant sizes don't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, TS)]
#[ts(optional_fields)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// First message of every connection (and after the client fell behind): full state.
    Snapshot {
        server: ServerInfo,
        players: Vec<Player>,
    },
    Server {
        server: ServerInfo,
    },
    /// A player appeared or its state changed.
    Player {
        player: Player,
    },
    PlayerRemoved {
        id: String,
    },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::Snapshot { .. } => "snapshot",
            Event::Server { .. } => "server",
            Event::Player { .. } => "player",
            Event::PlayerRemoved { .. } => "player_removed",
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(optional_fields)]
pub struct ApiError {
    pub error: String,
}

// ---------------------------------------------------------------- library

/// One page of a list.  Item `i` is entry `offset + i` of `total`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Page<T> {
    pub total: usize,
    pub offset: usize,
    pub items: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Artist {
    pub id: i64,
    pub name: String,
    /// Picture path relative to the server root (`contributor/<id>/image`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portrait: Option<String>,
    /// Index letter of the sort order (in lists).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub letter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Album {
    pub id: i64,
    pub title: String,
    /// The album artist; Various Artists for a compilation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    pub compilation: bool,
    /// Like `Track.artwork`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    /// Index letter of the sort order (in lists sorted by title or artist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub letter: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: Album,
    /// Seconds, all tracks together.
    pub duration: f64,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Genre {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Year {
    /// 0 for tracks without a year.
    pub year: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum FolderEntryType {
    Folder,
    Track,
    Playlist,
    /// A file that can't be played as it is (e.g. an audio file split by a cue sheet).
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct FolderEntry {
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: FolderEntryType,
    /// Folder id, playlist id or track id, depending on `type`.
    pub id: i64,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct SearchResults {
    pub artists: Page<Artist>,
    pub albums: Page<Album>,
    pub tracks: Page<Track>,
    pub genres: Page<Genre>,
}

// ---------------------------------------------------------------- favorites, Spotify

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum BrowseItemType {
    /// Has items: fetch them with its id.
    Folder,
    /// Something to play.
    Audio,
    /// A search box: fetch `<id>_<url-encoded term>` for the results.
    Search,
    /// Information only.
    Text,
}

/// An entry of favorites or of a service's menus.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct BrowseItem {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: BrowseItemType,
    /// Can be queued (as `{"type": "favorite"|"spotify", "id": …}`), a folder as a whole.
    pub playable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Like `Track.artwork`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
}

/// `POST /api/v1/favorites` body.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
pub struct NewFavorite {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct SpotifyStatus {
    pub signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `premium`, `free`, …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// Why browsing or playback doesn't work right now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/v1/spotify/login` body.  Without `code`, starts a sign-in; with it (the
/// address Spotify redirected the browser to), finishes one.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[ts(optional_fields)]
pub struct SpotifyLogin {
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default, TS)]
#[ts(optional_fields)]
pub struct SpotifyLoginResult {
    /// The page to open to sign in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Signed in as.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

// ---------------------------------------------------------------- settings

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ServerSettings {
    pub library_name: String,
    pub media_dirs: Vec<String>,
    /// Scan what changes in the music folders as it changes.
    pub watch_folders: bool,
    /// Folder with pictures named after artists; empty for none.
    pub art_folder: String,
    pub online_artist_images: bool,
    /// The user's own Spotify app; empty for the shared one.
    pub spotify_client_id: String,
    /// kbps: 96, 160 or 320.
    pub spotify_bitrate: i64,
    pub spotify_normalisation: bool,
}

/// `PATCH /api/v1/settings` body: the settings to change.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct ServerSettingsPatch {
    pub library_name: Option<String>,
    pub media_dirs: Option<Vec<String>>,
    pub watch_folders: Option<bool>,
    pub art_folder: Option<String>,
    pub online_artist_images: Option<bool>,
    pub spotify_client_id: Option<String>,
    pub spotify_bitrate: Option<i64>,
    pub spotify_normalisation: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default, TS)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    /// Look for new, changed and removed files.
    #[default]
    Changes,
    /// Saved playlists only.
    Playlists,
    /// Clear the library and scan everything again.
    Full,
}

/// `POST /api/v1/library/scan` body.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[ts(optional_fields)]
pub struct ScanRequest {
    #[serde(default)]
    pub mode: Option<ScanMode>,
}

/// One of a list of values.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Choice {
    pub value: String,
    pub options: Vec<ChoiceOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Range {
    pub value: i64,
    pub min: i64,
    pub max: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct PlayerSettings {
    /// 0 none, 1 crossfade, 2 fade in, 3 fade out, 4 fade in and out.
    pub transition_type: i64,
    /// Seconds.
    pub transition_duration: i64,
    /// 0 off, 1 track gain, 2 album gain, 3 smart.
    pub replay_gain_mode: i64,
    /// kbps; 0 for no limit.
    pub max_bitrate: i64,
    pub digital_volume_control: bool,
    /// What plays when the queue runs out ("0": nothing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dont_stop_the_music: Option<Choice>,
    /// For players with a screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplaySettings>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct DisplaySettings {
    /// What the screen shows while playing.
    pub playing_mode: Choice,
    pub text_size: Choice,
    pub text_size_off: Choice,
    pub brightness_on: Choice,
    pub brightness_off: Choice,
    pub brightness_idle: Choice,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_brightness_min: Option<Range>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_brightness_sensitivity: Option<Range>,
}

/// `PATCH /api/v1/players/{id}/settings` body: the settings to change.  Choices take one
/// of their options' values.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PlayerSettingsPatch {
    pub transition_type: Option<i64>,
    pub transition_duration: Option<i64>,
    pub replay_gain_mode: Option<i64>,
    pub max_bitrate: Option<i64>,
    pub digital_volume_control: Option<bool>,
    pub dont_stop_the_music: Option<String>,
    pub playing_mode: Option<String>,
    pub text_size: Option<String>,
    pub text_size_off: Option<String>,
    pub brightness_on: Option<String>,
    pub brightness_off: Option<String>,
    pub brightness_idle: Option<String>,
    pub auto_brightness_min: Option<i64>,
    pub auto_brightness_sensitivity: Option<i64>,
}
