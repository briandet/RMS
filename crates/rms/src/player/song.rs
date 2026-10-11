//! A `Song`: one playlist entry prepared for streaming (`Slim::Player::Song`).

use crate::stream::StreamSpec;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SONG_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongStatus {
    /// Prepared, not yet streamed.
    Ready,
    /// Being sent to the player (and/or buffered there).
    Streaming,
    /// Audible on the player.
    Playing,
    Finished,
    Failed,
}

/// Metadata about the underlying track, from the library or from probing a remote URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackInfo {
    pub url: String,
    pub id: Option<i64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<i64>,
    pub artist_id: Option<i64>,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub tracknum: Option<i64>,
    pub disc: Option<i64>,
    /// Library content type code (`flc`, `mp3`, `pcm`, `aac`, …).
    pub content_type: String,
    pub secs: Option<f64>,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub samplesize: Option<u32>,
    pub channels: Option<u32>,
    pub lossless: bool,
    pub vbr: bool,
    /// PCM data is little endian (WAV) rather than big (AIFF).
    pub little_endian: bool,
    pub audio_offset: Option<u64>,
    pub audio_size: Option<u64>,
    pub filesize: Option<u64>,
    pub replay_gain: Option<f64>,
    pub replay_peak: Option<f64>,
    pub album_replay_gain: Option<f64>,
    pub coverid: Option<String>,
    pub remote: bool,
    /// Cue-sheet virtual track: start/end seconds inside the file.
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
    pub artwork_url: Option<String>,
}

impl TrackInfo {
    pub fn duration(&self) -> Option<f64> {
        match (self.cue_start, self.cue_end) {
            (Some(s), Some(e)) if e > s => Some(e - s),
            _ => self.secs,
        }
    }
}

/// Live metadata for a remote stream (ICY title etc.).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover: Option<String>,
    pub bitrate: Option<String>,
    pub content_type: Option<String>,
    /// Station name (icy-name).
    pub station: Option<String>,
}

/// `Song::retryData`: retries done so far and when retrying started.
#[derive(Debug, Clone, Copy)]
pub struct RetryData {
    pub count: usize,
    pub start: f64,
}

#[derive(Debug, Clone)]
pub struct Song {
    pub id: u64,
    /// Playlist position this song came from.
    pub index: usize,
    pub track: TrackInfo,
    pub status: SongStatus,
    /// Output format after transcoding decision (`flc`, `mp3`, `pcm`, …).
    pub stream_format: String,
    /// Where the player gets its audio: `None` for the server (`/stream.mp3`), or a remote URL
    /// the player should fetch directly.
    pub direct_url: Option<String>,
    /// Seek position in seconds within the track at which streaming started.
    pub start_offset: f64,
    /// Bitrate of the *stream* (after transcoding / as probed), bits/s.
    pub stream_bitrate: Option<u32>,
    pub remote_meta: RemoteMeta,
    pub spec: Option<StreamSpec>,
    /// Set while a remote stream that already played is being retried (`retryData`).
    pub retry: Option<RetryData>,
    pub can_seek: bool,
    /// Replay gain applied when streaming (dB); 0 = none.
    pub replay_gain_db: f64,
    pub started_at: Option<f64>,
    /// Resolved audio URL for remote entries (after playlist expansion / redirects).
    pub stream_url: Option<String>,
    /// The player reported this song's start (STMs).  Songs promoted to playing by the
    /// server (short tracks fully streamed before STMs) absorb the late STMs instead of
    /// being retired by it.
    pub start_reported: bool,
}

impl Song {
    pub fn new(index: usize, track: TrackInfo) -> Song {
        Song {
            id: NEXT_SONG_ID.fetch_add(1, Ordering::Relaxed),
            index,
            track,
            status: SongStatus::Ready,
            stream_format: String::new(),
            direct_url: None,
            start_offset: 0.0,
            stream_bitrate: None,
            remote_meta: RemoteMeta::default(),
            spec: None,
            retry: None,
            can_seek: true,
            replay_gain_db: 0.0,
            started_at: None,
            stream_url: None,
            start_reported: false,
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.track.duration().filter(|d| *d > 0.0)
    }

    pub fn is_remote(&self) -> bool {
        self.track.remote
    }

    pub fn is_live(&self) -> bool {
        self.track.remote && self.duration().is_none()
    }

    pub fn url(&self) -> &str {
        &self.track.url
    }
}
