//! Types of the native API.  Everything here is part of the wire format: field names are
//! `snake_case`, enums are lower-case strings, absent values are omitted.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScanProgress {
    /// Current step, e.g. `directory_new`, `post_processing`; empty while starting.
    pub step: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LibraryTotals {
    pub albums: u64,
    pub artists: u64,
    pub genres: u64,
    pub tracks: u64,
    /// Seconds.
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayMode {
    Play,
    Pause,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Queue {
    pub version: f64,
    pub length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    pub offset: usize,
    pub items: Vec<Track>,
}

/// Something that can be queued or used to seed a mix.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Item {
    Track { id: i64 },
    Album { id: i64 },
    Artist { id: i64 },
    Genre { id: i64 },
    Year { year: i64 },
    Playlist { id: i64 },
    Folder { id: i64 },
    Url { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Default)]
pub struct CommandResult {
    /// Tracks added by `enqueue` / `mix`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// Messages on `GET /api/v1/events`.  The SSE event name is the `type`.
/// Short-lived (serialized once per change), so variant sizes don't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub error: String,
}
