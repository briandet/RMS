//! Learning what Spotify tracks sound like while they play, so mixes can pick them
//! ([`rms_library::remote`]).  A [`Learner`] sits in a librespot sink: it keeps a mono copy of
//! the samples of the track being played (at bliss's 22050 Hz) and, when the track has played
//! through to its end, has the library analyse it.  Only the analysis and the track's
//! metadata are kept; tracks that were seeked in, cut short or are already known are not
//! analysed, and no track is ever decoded just to be analysed.
//!
//! The sink learns where tracks start and end from the player's events, read on the player's
//! own thread before each write: librespot sends them from that thread in order with the
//! audio, so the boundaries are exact even when one track runs into the next.

use super::Spotify;
use crate::server::Server;
use librespot_core::SpotifyUri;
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use parking_lot::Mutex;
use rms_library::remote::RemoteTrack;
use std::sync::{Arc, Weak};
use tracing::{debug, warn};

/// Server pref: learn Spotify tracks as they play.
pub const PREF: &str = "spotifyLearn";
/// Longest track kept for analysis (as for local tracks: DJ mixes say little as one vector).
const MAX_SECS: f64 = 20.0 * 60.0;
/// How far the samples kept may fall short of the track's length (the start and end of a
/// stream are not always whole packets).
const SLACK_SECS: f64 = 2.0;

/// The player's events, for the sink: set once the player exists.
pub type EventSlot = Arc<Mutex<Option<PlayerEventChannel>>>;

/// Whether learning is on.
pub fn enabled(server: &Server) -> bool {
    !server.prefs.server.exists(PREF) || server.prefs.server.get_bool(PREF)
}

/// Whether mixes may pick learned Spotify tracks: learning is on and Spotify can play them.
pub fn in_mixes(server: &Server) -> bool {
    enabled(server) && super::get().is_some_and(|s| s.signed_in())
}

/// Whether to keep a track's samples.
type Want = Box<dyn Fn(&SpotifyUri) -> bool + Send>;
/// A track kept whole: its metadata as announced, and its samples.
type Done = Box<dyn FnMut(SpotifyUri, Option<Announced>, Vec<f32>) + Send>;

/// Follows a librespot player's output (see the module doc).
pub struct Learner {
    want: Want,
    done: Done,
    events: EventSlot,
    channel: Option<PlayerEventChannel>,
    /// The track whose samples are being kept.
    capture: Option<Capture>,
    /// Name, artists, album, length and cover of tracks, as the player announced them.
    announced: Option<(SpotifyUri, Announced)>,
}

#[derive(Clone)]
struct Announced {
    title: String,
    artists: Vec<String>,
    album: String,
    duration_ms: u32,
    cover: Option<String>,
}

struct Capture {
    uri: SpotifyUri,
    mono: Vec<f32>,
    down: Downsampler,
}

impl Learner {
    /// A learner for `server`'s library (it learns nothing outside a Tokio runtime).
    pub fn new(server: Weak<Server>, events: EventSlot) -> Learner {
        let runtime = tokio::runtime::Handle::try_current().ok();
        let s = server.clone();
        // learning is on and the track isn't known yet
        let want = move |uri: &SpotifyUri| {
            let Some(server) = s.upgrade() else {
                return false;
            };
            if !enabled(&server) || !matches!(uri, SpotifyUri::Track { .. }) {
                return false;
            }
            let Ok(id) = uri.to_id() else { return false };
            !server.library.remote_analyzed("spotify", &id)
        };
        let done = move |uri: SpotifyUri, announced: Option<Announced>, mono: Vec<f32>| {
            let (Some(runtime), Some(server)) = (&runtime, server.upgrade()) else {
                return;
            };
            runtime.spawn(learn(server, uri, announced, mono));
        };
        Learner::with(events, Box::new(want), Box::new(done))
    }

    fn with(events: EventSlot, want: Want, done: Done) -> Learner {
        Learner {
            want,
            done,
            events,
            channel: None,
            capture: None,
            announced: None,
        }
    }

    /// Samples the sink is about to output (interleaved stereo, 44.1 kHz).
    pub fn samples(&mut self, s: &[i16]) {
        self.events();
        if let Some(c) = &mut self.capture {
            c.down.push(s, &mut c.mono);
            if c.mono.len() as f64 > MAX_SECS * rms_library::sonic::SAMPLE_RATE as f64 {
                self.capture = None;
            }
        }
    }

    /// The sink stops (end of the last track, pause, stop): catch up with the events.
    pub fn stopped(&mut self) {
        self.events();
    }
}

impl Drop for Learner {
    /// The player shuts down: a track that just ended is still learned.
    fn drop(&mut self) {
        self.events();
    }
}

impl Learner {
    fn events(&mut self) {
        if self.channel.is_none() {
            self.channel = self.events.lock().take();
        }
        let mut evs = Vec::new();
        if let Some(ch) = &mut self.channel {
            while let Ok(ev) = ch.try_recv() {
                evs.push(ev);
            }
        }
        for ev in evs {
            self.event(ev);
        }
    }

    fn event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::TrackChanged { audio_item } => {
                let artists = match &audio_item.unique_fields {
                    librespot_metadata::audio::UniqueFields::Track { artists, .. } => {
                        artists.0.iter().map(|a| a.name.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                let album = match &audio_item.unique_fields {
                    librespot_metadata::audio::UniqueFields::Track { album, .. } => album.clone(),
                    _ => String::new(),
                };
                let cover = audio_item
                    .covers
                    .iter()
                    .max_by_key(|c| c.width)
                    .map(|c| c.url.clone());
                self.announced = Some((
                    audio_item.track_id.clone(),
                    Announced {
                        title: audio_item.name.clone(),
                        artists,
                        album,
                        duration_ms: audio_item.duration_ms,
                        cover,
                    },
                ));
            }
            // a track starts (or resumes, or plays on after an underrun)
            PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            } => {
                if self.capture.as_ref().is_some_and(|c| c.uri == track_id) {
                    return;
                }
                self.capture = None;
                if position_ms < 100 && (self.want)(&track_id) {
                    self.capture = Some(Capture {
                        uri: track_id,
                        mono: Vec::with_capacity(rms_library::sonic::SAMPLE_RATE as usize * 240),
                        down: Downsampler::default(),
                    });
                }
            }
            // not the whole track in order
            PlayerEvent::Seeked { .. }
            | PlayerEvent::PositionCorrection { .. }
            | PlayerEvent::Stopped { .. }
            | PlayerEvent::Unavailable { .. }
            | PlayerEvent::Loading { .. } => self.capture = None,
            PlayerEvent::EndOfTrack { track_id, .. } => {
                if let Some(c) = self.capture.take().filter(|c| c.uri == track_id) {
                    self.finish(c);
                }
            }
            _ => {}
        }
    }

    /// A track played to its end: have it analysed, unless samples are missing.
    fn finish(&mut self, c: Capture) {
        let announced = self
            .announced
            .as_ref()
            .filter(|(u, _)| *u == c.uri)
            .map(|(_, a)| a.clone());
        let secs = c.mono.len() as f64 / rms_library::sonic::SAMPLE_RATE as f64;
        if let Some(a) = &announced {
            let want = a.duration_ms as f64 / 1000.0;
            if secs + SLACK_SECS < want {
                debug!(
                    "Spotify: not learning {}: {secs:.1} s of {want:.1} s kept",
                    c.uri
                );
                return;
            }
        }
        (self.done)(c.uri, announced, c.mono);
    }
}

/// Analyse a track that played whole, with its metadata, into the library.
async fn learn(server: Arc<Server>, uri: SpotifyUri, announced: Option<Announced>, mono: Vec<f32>) {
    let Ok(id) = uri.to_id() else { return };
    let Some(spotify) = super::get().cloned() else {
        return;
    };
    let Some(track) = remote_track(&spotify, &uri, &id, announced).await else {
        return;
    };
    let lib = server.library.clone();
    let r = tokio::task::spawn_blocking(move || lib.learn_remote(&track, &mono)).await;
    match r {
        Ok(Err(e)) => debug!("Spotify: not learning {id}: {e}"),
        Err(e) => warn!("Spotify: learning {id}: {e}"),
        Ok(Ok(())) => {}
    }
}

/// The track's metadata: from the Spotify catalogue (with its ISRC and year), else as the
/// player announced it.
async fn remote_track(
    spotify: &Arc<Spotify>,
    uri: &SpotifyUri,
    id: &str,
    announced: Option<Announced>,
) -> Option<RemoteTrack> {
    let mut t = RemoteTrack {
        source: "spotify".into(),
        source_id: id.to_string(),
        url: super::url_for("track", id),
        ..Default::default()
    };
    if let Some(a) = announced {
        t.title = a.title;
        t.artists = a.artists;
        t.album = Some(a.album).filter(|s| !s.is_empty());
        t.duration = Some(a.duration_ms as f64 / 1000.0);
        t.cover = a.cover;
    }
    if let Ok(session) = spotify.session().await {
        use librespot_metadata::Metadata;
        if let Ok(m) = librespot_metadata::Track::get(&session, uri).await {
            t.title = m.name.clone();
            t.artists = m.artists.0.iter().map(|a| a.name.clone()).collect();
            t.album = Some(m.album.name.clone()).filter(|s| !s.is_empty());
            t.duration = Some(m.duration as f64 / 1000.0);
            t.isrc = m
                .external_ids
                .0
                .iter()
                .find(|x| x.external_type.eq_ignore_ascii_case("isrc"))
                .map(|x| x.id.clone());
            use chrono::Datelike;
            t.year = chrono::DateTime::from_timestamp_millis(m.album.date.as_timestamp_ms())
                .map(|d| d.year() as i64)
                .filter(|y| *y > 1000);
        }
    }
    (!t.title.is_empty()).then_some(t)
}

/// 44.1 kHz interleaved stereo → 22.05 kHz mono: the channels averaged, low-passed below the
/// new Nyquist frequency and every other sample kept.
struct Downsampler {
    taps: Vec<f32>,
    /// The last input samples (mono), newest last.
    history: Vec<f32>,
    /// Whether the next input sample is one to output at.
    odd: bool,
}

/// Low-pass filter length (odd).
const TAPS: usize = 47;

impl Default for Downsampler {
    fn default() -> Downsampler {
        // windowed sinc (Blackman), cutoff 10 kHz at 44.1 kHz
        let fc = 10_000.0 / 44_100.0;
        let m = (TAPS - 1) as f64;
        let mut taps: Vec<f64> = (0..TAPS)
            .map(|n| {
                let x = n as f64 - m / 2.0;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
                };
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / m).cos()
                    + 0.08 * (4.0 * std::f64::consts::PI * n as f64 / m).cos();
                sinc * w
            })
            .collect();
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Downsampler {
            taps: taps.into_iter().map(|t| t as f32).collect(),
            history: vec![0.0; TAPS],
            odd: false,
        }
    }
}

impl Downsampler {
    fn push(&mut self, stereo: &[i16], out: &mut Vec<f32>) {
        for f in stereo.chunks_exact(2) {
            let x = (f[0] as f32 + f[1] as f32) / (2.0 * 32768.0);
            self.history.rotate_left(1);
            if let Some(last) = self.history.last_mut() {
                *last = x;
            }
            if !self.odd {
                out.push(
                    self.history
                        .iter()
                        .zip(&self.taps)
                        .map(|(a, b)| a * b)
                        .sum(),
                );
            }
            self.odd = !self.odd;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(id: &str) -> SpotifyUri {
        SpotifyUri::from_uri(&format!("spotify:track:{id}")).unwrap()
    }

    /// A second of 44.1 kHz stereo.
    fn second() -> Vec<i16> {
        vec![1000; 2 * 44100]
    }

    #[test]
    fn whole_tracks_are_kept_with_exact_boundaries() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let slot = EventSlot::default();
        *slot.lock() = Some(rx);
        let kept = Arc::new(Mutex::new(Vec::new()));
        let k = kept.clone();
        let mut l = Learner::with(
            slot,
            Box::new(|_| true),
            Box::new(move |uri, _, mono| k.lock().push((uri, mono.len()))),
        );
        let (a, b, c) = (
            uri("4uLU6hMCjMI75M1A2tKUQC"),
            uri("1301WleyT98MSxVHPZCA6M"),
            uri("6rqhFgbbKwnb9MLmUQDhG6"),
        );
        let playing = |id: &SpotifyUri, pos| PlayerEvent::Playing {
            play_request_id: 1,
            track_id: id.clone(),
            position_ms: pos,
        };
        let end = |id: &SpotifyUri| PlayerEvent::EndOfTrack {
            play_request_id: 1,
            track_id: id.clone(),
        };
        // A plays whole, and B follows on gaplessly: the player announces B's start
        // before writing its first samples
        tx.send(playing(&a, 0)).unwrap();
        l.samples(&second());
        l.samples(&second());
        tx.send(end(&a)).unwrap();
        tx.send(playing(&b, 0)).unwrap();
        l.samples(&second());
        // a pause and resume of B changes nothing
        tx.send(playing(&b, 1000)).unwrap();
        l.samples(&second());
        // ...but a seek in it does
        tx.send(PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: b.clone(),
            position_ms: 30_000,
        })
        .unwrap();
        l.samples(&second());
        tx.send(end(&b)).unwrap();
        // C starts in the middle: not kept either
        tx.send(playing(&c, 5000)).unwrap();
        l.samples(&second());
        tx.send(end(&c)).unwrap();
        l.stopped();
        assert_eq!(*kept.lock(), vec![(a, 2 * 22050)]);
    }

    /// What the stream's samples give sounds like what the file gives: the downsampling
    /// here is as good as bliss's own decoding for the analysis.
    #[test]
    fn streamed_samples_analyse_like_the_file() {
        let Ok(out) = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
        else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        if !out.status.success() {
            return;
        }
        let decode = |fmt: &[&str]| -> Vec<u8> {
            let out = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                // something like music: a melody over noise and a pulse
                .arg("aevalsrc='0.3*sin(2*PI*(220+110*floor(mod(t,4)))*t)+0.2*sin(2*PI*660*t)*exp(-8*mod(t,0.5))+0.05*(random(0)-0.5)':s=44100:d=40")
                .args(fmt)
                .arg("-")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        };
        let stereo: Vec<i16> = decode(&["-ac", "2", "-ar", "44100", "-f", "s16le"])
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let reference: Vec<f32> = decode(&["-ac", "1", "-ar", "22050", "-f", "f32le"])
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let mut mono = Vec::new();
        let mut d = Downsampler::default();
        for chunk in stereo.chunks(4096) {
            d.push(chunk, &mut mono);
        }
        assert!((mono.len() as i64 - reference.len() as i64).abs() <= 1);
        let ours = rms_library::sonic::features(&mono).unwrap();
        let theirs = rms_library::sonic::features(&reference).unwrap();
        let mut ix = rms_library::sonic::Index::empty();
        ix.add(1, &ours);
        ix.add(2, &theirs);
        assert!(ix.near_duplicate(1, 2), "{ours:?}\n{theirs:?}");
    }

    #[test]
    fn downsampling_keeps_low_tones_and_drops_high_ones() {
        let tone = |hz: f64| -> Vec<i16> {
            (0..44100)
                .flat_map(|i| {
                    let v = ((2.0 * std::f64::consts::PI * hz * i as f64 / 44100.0).sin() * 16000.0)
                        as i16;
                    [v, v]
                })
                .collect()
        };
        let rms = |s: &[f32]| {
            (s[1000..].iter().map(|x| x * x).sum::<f32>() / (s.len() - 1000) as f32).sqrt()
        };
        let mut low = Vec::new();
        Downsampler::default().push(&tone(1000.0), &mut low);
        assert_eq!(low.len(), 22050);
        let mut high = Vec::new();
        Downsampler::default().push(&tone(15000.0), &mut high);
        // 16000/32768 / √2 ≈ 0.345 for the kept tone; the one above 11 kHz nearly gone
        assert!((rms(&low) - 0.345).abs() < 0.02, "{}", rms(&low));
        assert!(rms(&high) < 0.005, "{}", rms(&high));
    }
}
