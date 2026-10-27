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
    /// The port of the server's HTTPS address, when it has one (the same routes as HTTP).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub https_port: Option<u16>,
    /// HTTPS uses a certificate from the server's own authority, which devices trust once
    /// `/tls/rms-ca.crt` is installed on them.
    pub local_ca: bool,
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

/// The player's shuffle and repeat modes are the command core's.
pub use crate::player::command::{Repeat, Shuffle};

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

/// What a player is playing as spectrum bars (`GET /api/v1/players/{id}/spectrum`).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Spectrum {
    pub kind: SpectrumKind,
    /// What the curve belongs to; a different key is a different curve.
    pub key: String,
    /// Readings per second.
    pub fps: u32,
    /// Position in the track (seconds) of the first reading.
    pub start: f64,
    /// The readings, base64: two bytes each, four levels 0–15 (bass, low mids, high mids,
    /// treble), the first of each pair in the high four bits.
    pub curve: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum SpectrumKind {
    /// The whole track's curve: fetch it once per track.
    Track,
    /// A stream's readings from a second before `from` on, as far as they go: ask again
    /// for more.
    Live,
    /// Nothing to show (not playing, or the audio can't be analysed).
    None,
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
    /// Spotify menu ids of a Spotify track's album and first artist (`album:<id>`,
    /// `artist:<id>`), to browse to them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spotify_album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spotify_artist: Option<String>,
    /// Internet radio, a service track or another remote URL.
    pub remote: bool,
    /// Station name, for a radio stream that reports the current song as its title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub station: Option<String>,
    /// Content type (`flc`, `mp3`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// What the library knows about a library track beyond its tags; only when asked for
    /// (`detail=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<TrackDetail>,
}

/// A library track's genres, tempo, key, listening history and identifiers: what
/// `GET /api/v1/tracks` filters on.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TrackDetail {
    pub genres: Vec<String>,
    /// Beats per minute, from the rhythm analysis, else the `BPM` tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// Musical key as a Camelot code (`8A`), from the rhythm analysis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// 0–100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<i64>,
    pub play_count: i64,
    /// Times it was left within 30 seconds.
    pub skip_count: i64,
    /// Unix time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_played: Option<i64>,
    /// Unix time it was added to the library.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<i64>,
    /// MusicBrainz recording id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// Integrated loudness, LUFS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loudness: Option<f64>,
    /// The sonic analysis is done, so similar songs can go by how it sounds.
    pub analyzed: bool,
    /// What kind of music it is, by EffNet (when that analysis is on and has reached it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<TrackKind>,
}

/// EffNet-Discogs' view of a track: the likeliest Discogs styles, moods and qualities (0–1;
/// approachability and engagement as the model gives them), and the likeliest MTG-Jamendo
/// themes and instruments.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct TrackKind {
    pub styles: Vec<Label>,
    pub moods: Vec<Label>,
    pub themes: Vec<Label>,
    pub instruments: Vec<Label>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Label {
    pub name: String,
    pub score: f32,
}

impl From<rms_library::effnet::Labels> for TrackKind {
    fn from(l: rms_library::effnet::Labels) -> Self {
        let v = |x: Vec<(String, f32)>| {
            x.into_iter()
                .map(|(name, score)| Label {
                    name,
                    score: (score * 1000.0).round() / 1000.0,
                })
                .collect()
        };
        TrackKind {
            styles: v(l.styles),
            moods: v(l.moods),
            themes: v(l.themes),
            instruments: v(l.instruments),
        }
    }
}

impl From<rms_library::find::TrackDetail> for TrackDetail {
    fn from(d: rms_library::find::TrackDetail) -> Self {
        TrackDetail {
            genres: d.genres,
            bpm: d.bpm,
            key: d.key,
            rating: d.rating,
            play_count: d.play_count,
            skip_count: d.skip_count,
            last_played: d.last_played,
            added: d.added,
            mbid: d.mbid,
            isrc: d.isrc,
            loudness: d.loudness,
            analyzed: d.analyzed,
            kind: None,
        }
    }
}

/// What the analyses made of a library track (`GET /api/v1/tracks/{id}/analysis`): the file,
/// then each analysis with where it stands and its results.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TrackAnalysis {
    pub track_id: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Its album, with the album's artist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<Album>,
    pub file: AnalysisFile,
    /// bliss: what it sounds like, which drives similar songs and mixes; loudness.
    pub bliss: AnalysisStage<BlissAnalysis>,
    /// Tempo, beats, key and energy.
    pub rhythm: AnalysisStage<RhythmAnalysis>,
    /// The audio fingerprint, to recognise the same recording.
    pub chromaprint: AnalysisStage<ChromaprintAnalysis>,
    /// EffNet-Discogs: what kind of music it is.
    pub effnet: AnalysisStage<EffnetAnalysis>,
    /// The MusicBrainz lookup.
    pub musicbrainz: AnalysisStage<MusicBrainzAnalysis>,
}

/// Where one analysis stands for a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisStatus {
    /// Current results.
    Done,
    /// Analysed by the current version, but nothing came of it (unreadable audio, too short,
    /// not found); tried again when the file changes.
    Failed,
    /// Results of an older version, waiting to be redone.
    Stale,
    /// Not analysed yet.
    Pending,
    /// Never analysed: longer than 20 minutes, or not a local file.
    Skipped,
    /// The analysis is turned off on this server.
    Off,
}

impl From<rms_library::report::Status> for AnalysisStatus {
    fn from(s: rms_library::report::Status) -> Self {
        use rms_library::report::Status as S;
        match s {
            S::Done => AnalysisStatus::Done,
            S::Failed => AnalysisStatus::Failed,
            S::Stale => AnalysisStatus::Stale,
            S::Pending => AnalysisStatus::Pending,
            S::Skipped => AnalysisStatus::Skipped,
            S::Off => AnalysisStatus::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct AnalysisStage<T> {
    pub status: AnalysisStatus,
    /// Version of the analysis that made the stored results, and the current one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    pub current_version: i64,
    /// Unix time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzed_at: Option<i64>,
    /// Present when `done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
}

impl<T> AnalysisStage<T> {
    /// A library analysis stage, its status adjusted by `status` and its results by `result`.
    pub fn map<A>(
        s: rms_library::report::Stage<A>,
        status: impl FnOnce(rms_library::report::Status) -> rms_library::report::Status,
        result: impl FnOnce(A) -> T,
    ) -> Self {
        AnalysisStage {
            status: status(s.status).into(),
            version: s.version,
            current_version: s.current_version,
            analyzed_at: s.analyzed_at,
            result: s.data.map(result),
        }
    }
}

/// The file and what the scanner read from it.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct AnalysisFile {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Identifies the audio across moves and renames; the analyses are stored under it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub format: String,
    /// Bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Bits per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    pub vbr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<i64>,
    pub lossless: bool,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// A cue-sheet track: where it starts and ends in the file, seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_start: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_end: Option<f64>,
    /// ReplayGain, dB, and peak, from the tags or the measured loudness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_gain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_peak: Option<f64>,
    /// The `BPM` tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm_tag: Option<i64>,
}

impl From<rms_library::report::FileInfo> for AnalysisFile {
    fn from(f: rms_library::report::FileInfo) -> Self {
        AnalysisFile {
            path: rms_library::url::path_from_file_url(&f.url)
                .map(|p| p.to_string_lossy().into_owned()),
            url: f.url,
            fingerprint: f.fingerprint,
            format: f.format,
            size: f.size,
            bitrate: f.bitrate,
            vbr: f.vbr,
            sample_rate: f.sample_rate,
            sample_size: f.sample_size,
            channels: f.channels,
            lossless: f.lossless,
            duration: f.duration,
            segment_start: f.segment.map(|s| s.0),
            segment_end: f.segment.map(|s| s.1).filter(|e| *e > 0.0),
            replay_gain: f.replay_gain,
            replay_peak: f.replay_peak,
            bpm_tag: f.bpm_tag,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct Feature {
    pub name: String,
    pub value: f32,
}

/// Another library track and how close it is.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Neighbour {
    pub id: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// bliss: the distance (smaller is closer); EffNet: the cosine similarity (larger is
    /// closer, at most 1).
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct BlissAnalysis {
    /// bliss's features, normalised to about -1..1, in its order: tempo, zero-crossing rate,
    /// spectral centroid, rolloff and flatness (mean, deviation), loudness (mean, deviation),
    /// then chroma.
    pub features: Vec<Feature>,
    /// Integrated loudness (EBU R128), LUFS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loudness: Option<f64>,
    /// True peak, linear (1.0 is full scale).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak: Option<f64>,
    /// The library tracks that sound closest.
    pub nearest: Vec<Neighbour>,
    /// Tracks closer than this are taken for the same recording.
    pub duplicate_distance: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct RhythmAnalysis {
    /// Beats per minute; 0 when there is no steady beat.
    pub bpm: f32,
    /// How steady and strong the beat is, 0–1 (below about 0.3: not beat-matched in mixes).
    pub beat_confidence: f32,
    /// Beats tracked, and 4/4 bars from the first downbeat.
    pub beats: usize,
    pub bars: usize,
    /// Seconds into the track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_downbeat: Option<f32>,
    /// Camelot code (`8A`) and name (`A minor`).
    pub key: String,
    pub key_name: String,
    /// 0–1: how clearly the key beats the runner-up.
    pub key_confidence: f32,
    /// Loudness per bar (per 2 seconds without a beat), dB.
    pub energy: Vec<f32>,
    /// Seconds per `energy` value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_step: Option<f32>,
}

impl From<rms_library::report::RhythmInfo> for RhythmAnalysis {
    fn from(r: rms_library::report::RhythmInfo) -> Self {
        RhythmAnalysis {
            bpm: r.bpm,
            beat_confidence: r.beat_confidence,
            beats: r.beats,
            bars: r.bars,
            first_downbeat: r.first_downbeat,
            key: r.key,
            key_name: r.key_name,
            key_confidence: r.key_confidence,
            energy: r.energy,
            energy_step: r.energy_step,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct ChromaprintAnalysis {
    /// 32-bit items, about 8 a second.
    pub items: usize,
    /// Seconds of audio it covers (the first two minutes at most).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Library tracks that sound like the same recording (by bliss), and whether their
    /// fingerprints confirm it.
    pub duplicates: Vec<Duplicate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct Duplicate {
    pub id: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    pub same_recording: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct EffnetAnalysis {
    pub labels: TrackKind,
    /// The library tracks closest in kind.
    pub nearest: Vec<Neighbour>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct MusicBrainzAnalysis {
    /// Recording id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    pub genres: Vec<String>,
    /// How it was found: `tag`, `isrc`, `acoustid` or `search`; `none` when it wasn't.
    pub how: String,
}

impl From<rms_library::report::MusicBrainz> for MusicBrainzAnalysis {
    fn from(m: rms_library::report::MusicBrainz) -> Self {
        MusicBrainzAnalysis {
            mbid: m.mbid,
            isrc: m.isrc,
            artist_mbid: m.artist_mbid,
            genres: m.genres,
            how: m.how,
        }
    }
}

/// How far the analyses have got over the library (`GET /api/v1/library/analysis`).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct AnalysisProgress {
    /// The background analysis (bliss, rhythm, Chromaprint, EffNet and spectrum bars, in one
    /// pass over the audio) is running.
    pub running: bool,
    pub bliss: AnalysisCount,
    pub rhythm: AnalysisCount,
    pub chromaprint: AnalysisCount,
    pub effnet: AnalysisCount,
    pub spectrum: AnalysisCount,
    pub musicbrainz: AnalysisCount,
}

/// One analysis over the library's distinct local audio.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, TS)]
pub struct AnalysisCount {
    /// Turned on in the server settings.
    pub enabled: bool,
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    /// Not analysed yet, or by an older version.
    pub pending: i64,
}

impl AnalysisCount {
    pub fn new(enabled: bool, c: rms_library::report::Count) -> Self {
        AnalysisCount {
            enabled,
            total: c.total,
            done: c.done,
            failed: c.failed,
            pending: c.pending,
        }
    }
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
    /// Library tracks, in this order.
    Tracks {
        ids: Vec<i64>,
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
    /// Recommendations, a playlist ListenBrainz made, or one of their recordings
    /// (`BrowseItem` id from `/listenbrainz`), from the library or else Spotify.
    #[serde(rename = "listenbrainz")]
    #[ts(rename = "listenbrainz")]
    ListenBrainz {
        id: String,
    },
    /// An internet radio station (`BrowseItem` id from `/radio`).
    Radio {
        id: String,
    },
    /// The library tracks a [Table](TableRequest) query finds, in its order.
    Query {
        q: String,
        #[serde(default)]
        sort: Vec<TableSort>,
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
    /// `album`, `ep`, `single`, `compilation`, `live`, … (as tagged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_type: Option<String>,
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

/// `GET /api/v1/albums/{id}/release`: the MusicBrainz release the album was matched to.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct AlbumRelease {
    pub status: ReleaseStatus,
    /// How the release was found: `tag` (the tags name it), `auto` (best fit), `user` (chosen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    /// How well the album fits the release, 0–1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseInfo>,
    /// The album's tracks, and the release track each one is.
    pub tracks: Vec<ReleaseTrackMatch>,
    /// The releases considered, best first.
    pub candidates: Vec<ReleaseCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseStatus {
    /// Matched confidently (or chosen, or named by the tags): fills in what tags leave out.
    Matched,
    /// Matched, but not surely: worth checking.
    Uncertain,
    /// No release found.
    None,
    /// Not matched yet (MusicBrainz identification is off, or still busy).
    Pending,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ReleaseInfo {
    pub mbid: String,
    pub title: String,
    pub artist: String,
    /// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`; empty when unknown.
    pub date: String,
    /// The first release of this album (the original year).
    pub original_date: String,
    pub country: String,
    /// `Official`, `Promotion`, `Bootleg`, …
    pub status: String,
    pub label: String,
    pub catalog_number: String,
    pub barcode: String,
    /// The media: `CD`, `2×CD`, `Digital Media`, …
    pub format: String,
    /// `Album`, `Single`, `EP`, … and secondary types (`Compilation`, `Live`, …).
    pub release_type: Vec<String>,
    pub track_count: i64,
    pub disc_count: i64,
    /// The release's page on MusicBrainz.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct ReleaseTrackMatch {
    pub track_id: i64,
    /// Where the track is on the release; absent when it is none of its tracks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    /// The track's number as printed ("A1", "3").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<f64>,
    /// MusicBrainz recording id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct ReleaseCandidate {
    pub mbid: String,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseInfo>,
}

/// `GET /api/v1/albums/{id}/tags`: the album's tracks' tags as they are in the files, next
/// to what its release says.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct AlbumTags {
    /// The release the proposals come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseInfo>,
    /// Why the files can't be changed now (see `edit_files_blocked` in the settings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// Not an album: the tracks without an album tag, each on its own.
    pub loose: bool,
    pub tracks: Vec<TrackTags>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TrackTags {
    pub track_id: i64,
    /// The file.
    pub path: String,
    /// Why this file can't be changed, if it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub tags: Vec<TagValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TagValue {
    /// `title`, `artist`, `album`, `album_artist`, `track_number`, `track_total`,
    /// `disc_number`, `disc_total`, `date`, `original_date`, `label`, `catalog_number`,
    /// `barcode`, `musicbrainz_recording_id`, `musicbrainz_track_id`,
    /// `musicbrainz_release_id`, `musicbrainz_release_group_id`, `musicbrainz_artist_id`,
    /// `musicbrainz_album_artist_id`.
    pub field: String,
    /// In the file (several values joined with "; "); absent when the file has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// What the release says; absent when it says nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    /// `proposed` differs from `current`.
    pub changed: bool,
}

/// `POST /api/v1/tags` body: tags to write, by track (`null` removes a tag).
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TagChanges {
    pub changes: Vec<TrackTagChange>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TrackTagChange {
    pub track_id: i64,
    pub tags: std::collections::BTreeMap<String, Option<String>>,
}

/// What writing (or undoing) tags did.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TagEditResult {
    /// The change's entry in `GET /api/v1/tags/edits`, to undo it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<i64>,
    /// Files changed.
    pub changed: i64,
    pub failed: Vec<TagEditFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct TagEditFailure {
    pub track_id: i64,
    pub error: String,
}

/// `GET /api/v1/albums/{id}/files`: where the album's files are, and where they go.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct AlbumFiles {
    /// The template used (the setting's, or the one asked for).
    pub template: String,
    /// Why files can't be moved now, if they can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    pub files: Vec<FileMove>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct FileMove {
    /// Absent for a file that is not a track (cover, booklet) going with the album.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<i64>,
    /// Relative to its music folder.
    pub from: String,
    pub to: String,
    /// `to` differs from `from`.
    pub changed: bool,
    /// Why it can't move, if it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/v1/albums/{id}/files` body.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct MoveRequest {
    /// Instead of the setting's.
    #[serde(default)]
    pub template: Option<String>,
}

/// What moving an album's files did.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct MoveResult {
    /// The change's entry in `GET /api/v1/tags/edits`, to undo it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<i64>,
    /// Tracks moved.
    pub changed: i64,
    pub failed: Vec<TagEditFailure>,
    /// The album after the library caught up (its id can change).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<i64>,
}

/// `GET /api/v1/albums/{id}/cover-art`: the album's cover now, and the Cover Art Archive's
/// images of its release.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct CoverArt {
    /// The MusicBrainz release the images are of; absent while the album isn't matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_mbid: Option<String>,
    /// Front cover first.
    pub images: Vec<CoverImage>,
    /// The album's files, and how many have an embedded front cover (it wins over a picture
    /// in the folder).
    pub files: i64,
    pub embedded: i64,
    /// The picture file in the album's folder that stands for it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_image: Option<String>,
    /// The album's folder holds other music too: a picture there would be theirs as well, so
    /// the cover can only be embedded.
    pub folder_shared: bool,
    /// The album shows the archive's front cover of its release: it has no art of its own
    /// (the files are not changed; saving it in the folder or the files keeps it).
    pub archive: bool,
    /// Why covers can't be written now, if they can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CoverImage {
    pub id: String,
    /// `Front`, `Back`, `Booklet`, …
    pub types: Vec<String>,
    pub front: bool,
    /// A small version, on coverartarchive.org.
    pub thumbnail: String,
}

/// `POST /api/v1/albums/{id}/cover-art` body.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CoverChoice {
    /// One of `images`.
    pub image: String,
    /// Save it as `cover.jpg` (or `.png`) in the album's folder.
    #[serde(default)]
    pub folder: bool,
    /// Embed it as the front cover of each of the album's files.
    #[serde(default)]
    pub embed: bool,
}

/// A change of tags (or of files' places) made through RMS.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TagEdit {
    pub batch: i64,
    /// `tags`, `move` or `cover`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Files changed.
    pub files: i64,
    /// Unix time.
    pub applied_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undone_at: Option<i64>,
}

/// `PUT /api/v1/albums/{id}/release` body: the release the album is.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ReleaseChoice {
    /// A MusicBrainz release id (from `candidates`, or from musicbrainz.org).
    pub mbid: String,
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
    /// Saved in the playlist folder, so it can be changed (a playlist file among the music or
    /// a cue sheet can't).
    pub editable: bool,
}

/// `POST /api/v1/playlists` body: a new playlist with what `item` stands for, or the queue
/// of `from_player`, or nothing.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct NewPlaylist {
    pub name: String,
    pub item: Option<Item>,
    pub from_player: Option<String>,
}

/// `PATCH /api/v1/playlists/{id}` body.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlaylistPatch {
    pub name: String,
}

/// `PUT /api/v1/playlists/{id}/tracks` body: the playlist's tracks (their indexes) in their
/// new order; those left out are removed.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlaylistOrder {
    pub order: Vec<usize>,
}

/// `POST /api/v1/playlists/{id}/tracks` body: add what `item` stands for, before track
/// `position` or at the end.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct PlaylistAdd {
    pub item: Item,
    pub position: Option<usize>,
}

/// `POST /api/v1/query` body: a page of the Table, rows of tracks or of groups of them.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct TableRequest {
    /// Words and `field:value` filters.
    #[serde(default)]
    pub q: String,
    /// A groupable field: one row per album, artist, year, …; absent or `track`: one row per
    /// track.
    pub group: Option<String>,
    /// Field names; `count` is a grouped row's number of tracks.
    pub columns: Vec<String>,
    #[serde(default)]
    pub sort: Vec<TableSort>,
    #[serde(default)]
    pub offset: usize,
    /// Rows (default 200, at most 1000).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct TableSort {
    pub field: String,
    #[serde(default)]
    pub desc: bool,
}

/// A page of the Table.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TablePage {
    /// Rows in all.
    pub total: usize,
    pub offset: usize,
    pub rows: Vec<TableRow>,
    /// Tracks the query finds, and their length in all (seconds).
    pub tracks: i64,
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TableRow {
    /// The track, on rows of tracks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// What a grouped row stands for (an album id, a year, a folder, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "string | number | boolean | null")]
    pub key: Option<serde_json::Value>,
    /// Its name (`null`: the tracks without one).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "string | number | boolean | null")]
    pub label: Option<serde_json::Value>,
    /// Query words that find a grouped row's tracks (`album_id:12`), to add to the query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drill: Option<String>,
    /// The grouping to show a grouped row's tracks in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// One per column: a value, `null`, `{"varies": n}` (a grouped row's tracks differ) or
    /// `[min, max]`.
    #[ts(type = "Array<string | number | boolean | null | { varies: number } | [number, number]>")]
    pub cells: Vec<serde_json::Value>,
}

/// A field of the Table (`GET /api/v1/query/fields`).
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct TableField {
    pub name: String,
    pub label: String,
    /// `text`, `int`, `real`, `duration` (seconds), `time` (Unix), `bool`, `size` (bytes),
    /// `bitrate` (bit/s), `percent` (0–1).
    pub kind: String,
    /// Where it belongs: `Tags`, `Audio`, `Listening`, `Analysis`, `MusicBrainz`, `Kind (EffNet)`,
    /// `Mood (EffNet)`, `File`.
    pub section: String,
    /// What a grouped row shows: `same` (the value its tracks share, else varies), `sum`,
    /// `avg`, `max`, `range`.
    pub agg: String,
    /// Rows can be grouped by it; `next` is the grouping a row's tracks are shown in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A saved view of the Table (`GET`, `PUT /api/v1/query/views`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct TableView {
    pub name: String,
    #[serde(default)]
    pub q: String,
    pub group: Option<String>,
    pub columns: Vec<String>,
    #[serde(default)]
    pub sort: Vec<TableSort>,
    /// Column widths in pixels, by field name.
    #[serde(default)]
    #[ts(type = "Record<string, number>")]
    pub widths: std::collections::BTreeMap<String, f64>,
}

/// `POST /api/v1/similar` body: songs similar to the seeds.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct SimilarRequest {
    /// Tracks, albums, artists, genres or years to start from.
    pub seeds: Vec<Item>,
    /// How many (default 20, at most 200).
    pub limit: Option<usize>,
    /// Track ids to leave out (what is already chosen).
    #[serde(default)]
    pub exclude: Vec<i64>,
    /// Include each track's `detail`.
    #[serde(default)]
    pub detail: bool,
}

/// An artist ListenBrainz finds similar, and the library's artist of that name if any.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct SimilarArtist {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<Artist>,
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
    /// Why a player isn't available as a Spotify Connect device (tried again later).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_error: Option<String>,
    /// Learn what tracks played from Spotify sound like, for mixes.
    pub learn: bool,
    /// Tracks learned so far.
    pub learned: u64,
}

/// The tracks learned from Spotify (`GET /api/v1/spotify/learned`), the latest first.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct LearnedTracks {
    pub items: Vec<LearnedTrack>,
}

/// A track played from a streaming service whose sound was analysed, for mixes.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct LearnedTrack {
    /// What plays it (`spotify://track:<id>`).
    pub url: String,
    pub title: String,
    /// The artists' names, joined.
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Image URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// Camelot key (`8B`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Integrated loudness, LUFS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loudness: Option<f64>,
    /// MusicBrainz genres.
    pub genres: Vec<String>,
    /// The most likely Discogs style, by its sound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// The library has a copy of it.
    pub in_library: bool,
    /// When it was learned, Unix seconds.
    pub learned_at: i64,
}

/// `GET /api/v1/players/{id}/spotify-queue`: what Spotify plays next on the player's
/// Spotify Connect device.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(optional_fields)]
pub struct SpotifyQueue {
    /// The player plays its Spotify Connect device (else `items` is empty).
    pub connect: bool,
    pub items: Vec<Track>,
    /// Why Spotify's queue can't be shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `PATCH /api/v1/spotify` body.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[ts(optional_fields)]
pub struct SpotifyPatch {
    #[serde(default)]
    pub learn: Option<bool>,
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
    /// UPnP / DLNA renderers on the network play as players.
    pub upnp_players: bool,
    /// Chromecasts on the network play as players.
    pub chromecast_players: bool,
    /// AirPlay speakers on the network play as players.
    pub airplay_players: bool,
    /// Players fetch internet radio from the station themselves rather than through the
    /// server (which then can't show radio's spectrum bars).
    pub radio_direct: bool,
    /// Folder with pictures named after artists; empty for none.
    pub art_folder: String,
    pub online_artist_images: bool,
    /// kbps: 96, 160 or 320.
    pub spotify_bitrate: i64,
    pub spotify_normalisation: bool,
    /// Identify recordings with MusicBrainz (and AcoustID) to fill in what tags leave out.
    pub musicbrainz: bool,
    /// AcoustID application key, for recognising recordings by their sound; empty for none.
    pub acoustid_key: String,
    /// Recordings identified on MusicBrainz so far.
    pub musicbrainz_identified: i64,
    /// Recordings waiting to be looked up.
    pub musicbrainz_pending: i64,
    /// Albums matched to their MusicBrainz release; matched only possibly (worth checking);
    /// waiting to be matched.
    pub albums_matched: i64,
    pub albums_uncertain: i64,
    pub albums_pending: i64,
    /// Tags can be changed in the music files (the web UI's album tag editor).
    pub edit_files: bool,
    /// Why music files can't be changed now (off, or no login), if they can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_files_blocked: Option<String>,
    /// Where an album's files go when organised, relative to their music folder
    /// (`{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}`).
    pub file_name_template: String,
    /// Mixes take some songs from outside what was played, found with ListenBrainz.
    pub listenbrainz: bool,
    /// Songs discovered so far, and of those played and rejected (skipped early).
    pub discovered: i64,
    pub discovered_played: i64,
    pub discovered_rejected: i64,
    /// ListenBrainz accounts, for scrobbling and recommendations.
    pub listenbrainz_accounts: Vec<ListenBrainzAccount>,
    /// Scrobble what plays from Spotify too.
    pub scrobble_spotify: bool,
    /// Listens waiting to be sent, and the last error sending them.
    pub scrobble_pending: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrobble_error: Option<String>,
    /// Analyse what kind of music each track is with EffNet-Discogs (models downloaded from
    /// MTG, non-commercial licence).
    pub effnet: bool,
    /// `off`, `downloading`, `ready` or `failed` (with `effnet_error`).
    pub effnet_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effnet_error: Option<String>,
    /// Tracks analysed with EffNet, and still to analyse.
    pub effnet_analyzed: i64,
    pub effnet_pending: i64,
    /// The TV visualiser can run (pmviz and presets are installed).
    pub visualizer_available: bool,
    /// Why it can't, when it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visualizer_error: Option<String>,
    /// The video encoder it uses (`h264_vaapi`: the GPU's, `libx264`: the CPU's).
    pub visualizer_encoder: String,
    /// Video size (`1920x1080`).
    pub visualizer_size: Choice,
    /// Size the visuals are drawn at, scaled up to the video size ("": the same).
    pub visualizer_render_size: Choice,
    pub visualizer_fps: Choice,
    /// The encoder's constant quality: higher is a smaller stream (each +6 about halves it).
    pub visualizer_quality: Range,
    /// Seconds each preset shows.
    pub visualizer_preset_seconds: Range,
    /// The Cast receiver app Chromecasts play in (8 hex digits), or empty for RMS's own:
    /// registered in the Google Cast developer console for
    /// `https://<owner>.github.io/<repo>/receiver/`.
    pub visualizer_receiver_app: String,
    // ---- playback, the same for every player
    /// 0 none, 1 crossfade, 2 fade in, 3 fade out, 4 fade in and out.
    pub transition_type: i64,
    /// Seconds.
    pub transition_duration: i64,
    /// 0 off, 1 track gain, 2 album gain, 3 smart.
    pub replay_gain_mode: i64,
    /// What plays when the queue runs out ("0": nothing).
    pub dont_stop_the_music: Choice,
    /// Percent of a mix discovered from outside what was played (0–40; with ListenBrainz on).
    pub adventurous: i64,
    /// The ListenBrainz accounts (user names) players scrobble to.
    pub scrobble_to: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct ListenBrainzAccount {
    pub user: String,
    /// Used by players without their own choice, and by Subsonic apps.
    pub default: bool,
}

/// `POST /api/v1/listenbrainz/accounts` body: a user token from
/// <https://listenbrainz.org/settings/>.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
pub struct NewListenBrainzAccount {
    pub token: String,
}

/// `PATCH /api/v1/listenbrainz/accounts/{user}` body.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ListenBrainzAccountPatch {
    pub default: bool,
}

/// `PATCH /api/v1/settings` body: the settings to change.
#[derive(Debug, Clone, PartialEq, Deserialize, Default, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields)]
pub struct ServerSettingsPatch {
    pub library_name: Option<String>,
    pub media_dirs: Option<Vec<String>>,
    pub watch_folders: Option<bool>,
    pub upnp_players: Option<bool>,
    pub chromecast_players: Option<bool>,
    pub airplay_players: Option<bool>,
    pub radio_direct: Option<bool>,
    pub art_folder: Option<String>,
    pub online_artist_images: Option<bool>,
    pub spotify_bitrate: Option<i64>,
    pub spotify_normalisation: Option<bool>,
    pub musicbrainz: Option<bool>,
    pub acoustid_key: Option<String>,
    pub edit_files: Option<bool>,
    pub file_name_template: Option<String>,
    pub listenbrainz: Option<bool>,
    pub scrobble_spotify: Option<bool>,
    pub effnet: Option<bool>,
    pub visualizer_size: Option<String>,
    pub visualizer_render_size: Option<String>,
    pub visualizer_fps: Option<String>,
    pub visualizer_quality: Option<i64>,
    pub visualizer_preset_seconds: Option<i64>,
    pub visualizer_receiver_app: Option<String>,
    pub transition_type: Option<i64>,
    pub transition_duration: Option<i64>,
    pub replay_gain_mode: Option<i64>,
    pub dont_stop_the_music: Option<String>,
    pub adventurous: Option<i64>,
    pub scrobble_to: Option<Vec<String>>,
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
    /// kbps; 0 for no limit.
    pub max_bitrate: i64,
    pub digital_volume_control: bool,
    /// For players with a screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DisplaySettings>,
    /// For Chromecasts: the TV visualiser (`auto`: the video for devices with a screen, `1`
    /// the video from the server, `tv` drawn on the TV by RMS's receiver app, `0` off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visualizer: Option<Choice>,
    /// Milliseconds the player's sound comes out later than it says (`playDelay`: TV
    /// speakers, a soundbar), so synced players line up; 0–1000.
    pub play_delay: i64,
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
    pub play_delay: Option<i64>,
    pub max_bitrate: Option<i64>,
    pub digital_volume_control: Option<bool>,
    pub playing_mode: Option<String>,
    pub text_size: Option<String>,
    pub text_size_off: Option<String>,
    pub brightness_on: Option<String>,
    pub brightness_off: Option<String>,
    pub brightness_idle: Option<String>,
    pub auto_brightness_min: Option<i64>,
    pub auto_brightness_sensitivity: Option<i64>,
    pub visualizer: Option<String>,
}
