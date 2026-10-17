//! Spotify audio for `/stream.mp3`: a librespot player decodes the track (from `start_ms`)
//! into 16-bit stereo at 44.1 kHz, which is sent to the player as FLAC (about half the size
//! of PCM, still lossless) or, for players without FLAC, as little-endian PCM.  Its output
//! goes through a bounded channel, so decoding runs only as fast as the player takes it.
//!
//! When Spotify refuses a track's decryption key, librespot plays the file undecrypted, and
//! its decoder can take the scrambled bytes for MP3 and make noise of them.  librespot only
//! logs it, so [`key_refusals`] watches its log: the stream loading the track drops what
//! that attempt decodes and loads the track again on a new librespot player ([`TRIES`] in all,
//! through the same encoder, so a FLAC stream stays one stream), and then gives up with an
//! error rather than noise.

use super::Spotify;
use bytes::Bytes;
use futures::stream::{self, StreamExt};
use librespot_core::SpotifyUri;
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::NoOpVolume;
use librespot_playback::player::{Player, PlayerEvent};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::{debug, warn};

/// Format of the PCM produced (what the `strm` header announces).
pub const SAMPLE_RATE: u32 = 44100;
pub const SAMPLE_SIZE: u32 = 16;
pub const CHANNELS: u32 = 2;

/// Chunks of PCM between the player thread and the HTTP stream.
pub const QUEUE: usize = 8;

/// FLAC block size (samples per channel), the reference encoder's default.
const FLAC_BLOCK: usize = 4096;

/// Times a stream loads its track before giving up when Spotify refuses its key.
pub const TRIES: u32 = 3;
/// Waits before loading again: the key server refuses requests that come too fast.
const RETRY_AFTER: [Duration; 2] = [Duration::from_millis(1500), Duration::from_secs(4)];

/// What librespot logs (`librespot_playback::player`, a warning) when the key is refused.
const KEY_REFUSED: &str = "Unable to load key";

/// One load of a track: whether Spotify refused its key.
#[derive(Default)]
pub struct Attempt {
    refused: AtomicBool,
    notify: tokio::sync::Notify,
}

impl Attempt {
    fn refused(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }
}

/// Loads under way (no audio yet), oldest first.  librespot's message doesn't say which track
/// it was loading, so a refusal goes to the oldest (loads at once are rare).
static LOADING: LazyLock<Mutex<VecDeque<Arc<Attempt>>>> = LazyLock::new(Default::default);

fn loaded(a: &Arc<Attempt>) {
    LOADING.lock().retain(|x| !Arc::ptr_eq(x, a));
}

/// A refusal: the oldest load under way was refused (`false`: none was under way).
pub fn key_refused() -> bool {
    let Some(a) = LOADING.lock().pop_front() else {
        return false;
    };
    a.refused.store(true, Ordering::Release);
    a.notify.notify_one();
    true
}

/// The log layer that sees librespot's key refusals (warnings and worse only).
pub fn key_refusals<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer;
    KeyRefusals.with_filter(tracing_subscriber::filter::LevelFilter::WARN)
}

struct KeyRefusals;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for KeyRefusals {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        use tracing_log::NormalizeEvent;
        // librespot logs through `log`: its target is in the normalized metadata
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());
        if meta.target() != "librespot_playback::player" {
            return;
        }
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        if m.0.starts_with(KEY_REFUSED) && !key_refused() {
            debug!("Spotify: a key was refused, but no stream was loading");
        }
    }
}

/// librespot output: interleaved samples into a channel (blocking when the player is ahead).
/// What a load whose key was refused decodes goes nowhere.
struct ChannelSink {
    tx: std::sync::mpsc::SyncSender<Vec<i16>>,
    learner: super::learn::Learner,
    attempt: Arc<Attempt>,
    started: bool,
}

impl Sink for ChannelSink {
    fn stop(&mut self) -> SinkResult<()> {
        self.learner.stopped();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        // the key is asked for before decoding starts: refused or not, it's known by now
        if self.attempt.refused() {
            return Ok(());
        }
        if !self.started {
            self.started = true;
            loaded(&self.attempt);
        }
        let samples = converter.f64_to_s16(&samples);
        self.learner.samples(&samples);
        self.tx
            .send(samples)
            .map_err(|_| SinkError::OnWrite("stream closed".into()))
    }
}

/// Streaming FLAC encoder: the stream header, then one frame per `FLAC_BLOCK` samples.
/// The header leaves the length and checksum unknown, as any streamed FLAC does.
pub struct FlacEncoder {
    config: flacenc::error::Verified<flacenc::config::Encoder>,
    info: flacenc::component::StreamInfo,
    /// Interleaved samples not yet encoded.
    pending: Vec<i32>,
    frame: usize,
}

impl FlacEncoder {
    pub fn new() -> FlacEncoder {
        use flacenc::error::Verify;
        let config = flacenc::config::Encoder::default()
            .into_verified()
            .unwrap_or_else(|_| unreachable!("the default encoder config is valid"));
        let mut stream = flacenc::component::Stream::new(
            SAMPLE_RATE as usize,
            CHANNELS as usize,
            SAMPLE_SIZE as usize,
        )
        .expect("CD audio is valid FLAC");
        let _ = stream
            .stream_info_mut()
            .set_block_sizes(FLAC_BLOCK, FLAC_BLOCK);
        FlacEncoder {
            config,
            info: stream.stream_info().clone(),
            pending: Vec::with_capacity(FLAC_BLOCK * CHANNELS as usize * 2),
            frame: 0,
        }
    }

    /// `fLaC` and the STREAMINFO block.
    pub fn header(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_bits(
            &flacenc::component::Stream::with_stream_info(self.info.clone()),
            &mut out,
        );
        out
    }

    fn encode(&mut self, samples: &[i32], out: &mut Vec<u8>) {
        use flacenc::source::Fill;
        let len = samples.len() / CHANNELS as usize;
        let Ok(mut fb) = flacenc::source::FrameBuf::with_size(CHANNELS as usize, len) else {
            return;
        };
        if fb.fill_interleaved(samples).is_err() {
            return;
        }
        match flacenc::encode_fixed_size_frame(&self.config, &fb, self.frame, &self.info) {
            Ok(frame) => {
                write_bits(&frame, out);
                self.frame += 1;
            }
            Err(e) => warn!("Spotify: FLAC encoding failed: {e:?}"),
        }
    }

    /// Encode every complete block of `samples` (interleaved); keep the rest for later.
    pub fn push(&mut self, samples: &[i16], out: &mut Vec<u8>) {
        self.pending.extend(samples.iter().map(|&s| s as i32));
        let block = FLAC_BLOCK * CHANNELS as usize;
        let mut done = 0;
        while self.pending.len() - done >= block {
            let chunk: Vec<i32> = self.pending[done..done + block].to_vec();
            self.encode(&chunk, out);
            done += block;
        }
        self.pending.drain(..done);
    }

    /// The last, shorter block (padded with silence to the encoder's minimum block size).
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.pending.is_empty() {
            return;
        }
        let mut last = std::mem::take(&mut self.pending);
        let min = flacenc::constant::MIN_BLOCK_SIZE * CHANNELS as usize;
        if last.len() < min {
            last.resize(min, 0);
        }
        self.encode(&last, out);
    }
}

/// Serialise a FLAC component (always whole bytes: headers and frames are byte-aligned).
fn write_bits<T: flacenc::component::BitRepr>(c: &T, out: &mut Vec<u8>) {
    let mut sink = flacenc::bitsink::ByteSink::new();
    match c.write(&mut sink) {
        Ok(()) => out.extend_from_slice(sink.as_slice()),
        Err(e) => warn!("Spotify: FLAC encoding failed: {e:?}"),
    }
}

impl Default for FlacEncoder {
    fn default() -> Self {
        FlacEncoder::new()
    }
}

fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}

/// Decoder settings from the server prefs.
pub fn player_config(bitrate: u32, normalise: bool) -> PlayerConfig {
    PlayerConfig {
        bitrate: match bitrate {
            0..=96 => Bitrate::Bitrate96,
            97..=160 => Bitrate::Bitrate160,
            _ => Bitrate::Bitrate320,
        },
        // one track per stream: our controller does gapless between streams
        gapless: false,
        normalisation: normalise,
        ..Default::default()
    }
}

/// Encode PCM from a player thread's blocking channel on a thread of its own, bridging it
/// to async: FLAC when `flac`, else PCM.  The output ends when the input channel closes
/// (after the last FLAC block).
pub fn encode(
    sync_rx: std::sync::mpsc::Receiver<Vec<i16>>,
    flac: bool,
) -> std::io::Result<tokio::sync::mpsc::Receiver<Vec<u8>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUEUE);
    std::thread::Builder::new()
        .name("spotify-audio".into())
        .spawn(move || {
            let mut flac = flac.then(FlacEncoder::new);
            // The header goes out with the first audio: a track that fails before any (it
            // cannot be loaded) sends nothing at all, which is what tells a failed stream from
            // a short track.
            let mut header = flac.as_ref().map(FlacEncoder::header);
            let with_header = |header: &mut Option<Vec<u8>>, bytes: Vec<u8>| match header.take() {
                Some(mut h) => {
                    h.extend(bytes);
                    h
                }
                None => bytes,
            };
            while let Ok(samples) = sync_rx.recv() {
                let bytes = match &mut flac {
                    Some(enc) => {
                        let mut out = Vec::new();
                        enc.push(&samples, &mut out);
                        out
                    }
                    None => pcm_bytes(&samples),
                };
                if !bytes.is_empty() && tx.blocking_send(with_header(&mut header, bytes)).is_err() {
                    return;
                }
            }
            if let Some(enc) = &mut flac {
                let mut out = Vec::new();
                enc.finish(&mut out);
                if !out.is_empty() {
                    let _ = tx.blocking_send(with_header(&mut header, out));
                }
            }
        })
        .map_err(std::io::Error::other)?;
    Ok(rx)
}

/// Open the audio stream of a track: FLAC when `flac`, else PCM.
pub async fn open(
    spotify: &Arc<Spotify>,
    url: &str,
    start_ms: u32,
    config: PlayerConfig,
    flac: bool,
) -> std::io::Result<crate::stream::source::ByteStream> {
    let (kind, id) =
        super::parse_url(url).ok_or_else(|| std::io::Error::other("bad Spotify URL"))?;
    if kind == "connect" {
        return super::connect::open_stream(&id, flac);
    }
    if kind != "track" {
        return Err(std::io::Error::other("only Spotify tracks can be streamed"));
    }
    let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}"))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let session = spotify.session().await.map_err(std::io::Error::other)?;

    // one encoder for every attempt: its output is the stream
    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(QUEUE);
    let rx = encode(sync_rx, flac)?;
    let load = Load {
        spotify: spotify.clone(),
        session,
        config,
        uri,
        start_ms,
        url: url.to_string(),
    };
    let running = load.start(sync_tx.clone());
    let state = State {
        rx,
        tx: Some(sync_tx),
        load,
        running: Some(running),
        tries: 1,
    };
    Ok(stream::unfold(state, |mut st| async move {
        loop {
            let refused = st.running.as_ref().map(|r| r.attempt.clone());
            tokio::select! {
                chunk = st.rx.recv() => match chunk {
                    Some(c) => return Some((Ok(Bytes::from(c)), st)),
                    // everything decoded has been sent
                    None => return None,
                },
                _ = async { refused.as_ref().expect("running").notify.notified().await }, if refused.is_some() => {
                    if let Err(e) = st.again().await {
                        return Some((Err(e), st));
                    }
                }
                res = async { st.running.as_mut().expect("running").done.as_mut().expect("pending").await },
                    if st.running.as_ref().is_some_and(|r| r.done.is_some()) => {
                    let refused = st.running.as_ref().is_some_and(|r| r.attempt.refused());
                    if let Some(r) = st.running.as_mut() {
                        r.done = None;
                    }
                    if refused {
                        // the refusal's own notice may come after the player gave up
                        if let Err(e) = st.again().await {
                            return Some((Err(e), st));
                        }
                        continue;
                    }
                    st.finish();
                    if let Ok(Err(e)) = res {
                        warn!("Spotify: {e}");
                        return Some((Err(std::io::Error::other(e)), st));
                    }
                    debug!("Spotify: end of track");
                }
            }
        }
    })
    .boxed())
}

/// What a stream loads, again if needed.
struct Load {
    spotify: Arc<Spotify>,
    session: librespot_core::Session,
    config: PlayerConfig,
    uri: SpotifyUri,
    start_ms: u32,
    url: String,
}

/// A librespot player loading or playing the track into the stream's encoder.
struct Running {
    player: Option<Arc<Player>>,
    attempt: Arc<Attempt>,
    /// Ends with the track (`Err`: it can't be played).
    done: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
}

impl Load {
    fn start(&self, tx: std::sync::mpsc::SyncSender<Vec<i16>>) -> Running {
        let attempt = Arc::new(Attempt::default());
        LOADING.lock().push_back(attempt.clone());
        let learn_events = super::learn::EventSlot::default();
        let learner = super::learn::Learner::new(self.spotify.server(), learn_events.clone());
        let sink_attempt = attempt.clone();
        let player = Player::new(
            self.config.clone(),
            self.session.clone(),
            Box::new(NoOpVolume),
            move || {
                Box::new(ChannelSink {
                    tx,
                    learner,
                    attempt: sink_attempt,
                    started: false,
                })
            },
        );
        *learn_events.lock() = Some(player.get_player_event_channel());
        let mut events = player.get_player_event_channel();
        player.load(self.uri.clone(), true, self.start_ms);

        // Finish when librespot reports the end of the track (all audio is queued by then).
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
        let url = self.url.clone();
        crate::util::spawn(async move {
            while let Some(ev) = events.recv().await {
                let result = match ev {
                    PlayerEvent::EndOfTrack { .. } | PlayerEvent::Stopped { .. } => Ok(()),
                    PlayerEvent::Unavailable { .. } => Err(format!("{url} is unavailable")),
                    _ => continue,
                };
                let _ = done_tx.send(result);
                break;
            }
        });
        Running {
            player: Some(player),
            attempt,
            done: Some(done_rx),
        }
    }
}

impl Running {
    /// Shut the player down: its output closes.
    fn release(&mut self) {
        loaded(&self.attempt);
        if let Some(p) = self.player.take() {
            p.stop();
            // joining the decoder thread may block briefly
            std::thread::spawn(move || drop(p));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.release();
    }
}

struct State {
    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    /// The encoder's input, for loading again; dropped at the end so the stream ends.
    tx: Option<std::sync::mpsc::SyncSender<Vec<i16>>>,
    load: Load,
    running: Option<Running>,
    tries: u32,
}

impl State {
    /// Spotify refused the key: load the track again after a while, or give up.
    async fn again(&mut self) -> std::io::Result<()> {
        if let Some(mut r) = self.running.take() {
            r.release();
        }
        let url = self.load.url.clone();
        if self.tries >= TRIES {
            self.finish();
            warn!(
                "Spotify: the key of {url} was refused {} times; giving up",
                self.tries
            );
            return Err(std::io::Error::other(format!(
                "Spotify refused the key of {url}"
            )));
        }
        let wait = RETRY_AFTER[(self.tries as usize - 1).min(RETRY_AFTER.len() - 1)];
        warn!("Spotify: the key of {url} was refused; loading it again in {wait:?}");
        tokio::time::sleep(wait).await;
        let Some(tx) = self.tx.clone() else {
            return Err(std::io::Error::other("stream closed"));
        };
        self.tries += 1;
        self.running = Some(self.load.start(tx));
        Ok(())
    }

    /// The track is over: let the encoder finish and the stream end.
    fn finish(&mut self) {
        if let Some(mut r) = self.running.take() {
            r.release();
        }
        self.tx = None;
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.rx.close();
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second of a 440 Hz tone (left) and 660 Hz (right), plus a short tail.
    fn tone() -> Vec<i16> {
        let n = SAMPLE_RATE as usize + 1234;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / SAMPLE_RATE as f64;
            v.push(((t * 440.0 * std::f64::consts::TAU).sin() * 12000.0) as i16);
            v.push(((t * 660.0 * std::f64::consts::TAU).sin() * 9000.0) as i16);
        }
        v
    }

    fn collect(mut rx: tokio::sync::mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
        let mut all = Vec::new();
        while let Some(b) = rx.blocking_recv() {
            all.extend(b);
        }
        all
    }

    #[test]
    fn a_track_without_audio_sends_nothing() {
        for flac in [true, false] {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(1);
            drop(tx);
            assert!(
                collect(encode(rx, flac).unwrap()).is_empty(),
                "flac: {flac}"
            );
        }
    }

    #[test]
    fn the_flac_header_comes_with_the_first_audio() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(1);
        let out = encode(rx, true).unwrap();
        tx.send(tone()).unwrap();
        drop(tx);
        let all = collect(out);
        assert_eq!(&all[..4], b"fLaC");
        assert!(all.len() > 42, "header and audio");
    }

    #[test]
    fn flac_stream_decodes_to_the_same_samples() {
        let pcm = tone();
        let mut enc = FlacEncoder::new();
        let mut out = enc.header();
        // in packet-sized pieces, as librespot delivers them
        for piece in pcm.chunks(2 * 1000 + 6) {
            enc.push(piece, &mut out);
        }
        enc.finish(&mut out);
        assert!(out.len() < pcm.len() * 2 * 3 / 4, "{} bytes", out.len());

        let mut reader = claxon::FlacReader::new(std::io::Cursor::new(out)).unwrap();
        let info = reader.streaminfo();
        assert_eq!(
            (info.sample_rate, info.channels, info.bits_per_sample),
            (44100, 2, 16)
        );
        assert_eq!(info.samples, None);
        let decoded: Vec<i16> = reader.samples().map(|s| s.unwrap() as i16).collect();
        assert_eq!(decoded, pcm);
    }

    /// librespot's warning that Spotify refused a key marks the oldest load under way, whose
    /// audio then goes nowhere; other messages and loads are left alone.
    #[test]
    fn a_refused_key_is_noticed_and_its_audio_dropped() {
        use tracing_subscriber::prelude::*;
        let subscriber = tracing_subscriber::registry().with(key_refusals());
        // as librespot logs: through `log`, bridged to tracing
        let warn = |target: &str, msg: &str| {
            tracing_log::format_trace(
                &log::Record::builder()
                    .target(target)
                    .level(log::Level::Warn)
                    .args(format_args!("{msg}"))
                    .build(),
            )
            .unwrap();
        };
        let (older, newer) = (Arc::new(Attempt::default()), Arc::new(Attempt::default()));
        LOADING.lock().extend([older.clone(), newer.clone()]);
        tracing::subscriber::with_default(subscriber, || {
            warn(
                "librespot_playback::player",
                "Unable to get normalisation data, continuing with defaults.",
            );
            warn(
                "librespot_core::audio_key",
                "Unable to load key, but from elsewhere",
            );
            assert!(!older.refused() && !newer.refused());
            warn("librespot_playback::player", "Unable to load key, continuing without decryption: Service unavailable { audio key error }");
        });
        assert!(older.refused());
        assert!(!newer.refused());
        assert!(LOADING.lock().iter().any(|a| Arc::ptr_eq(a, &newer)));
        assert!(!LOADING.lock().iter().any(|a| Arc::ptr_eq(a, &older)));

        // the refused load's sink passes nothing on; the other's does, and is loaded then
        let sink = |attempt: &Arc<Attempt>| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(4);
            let learner =
                super::super::learn::Learner::new(std::sync::Weak::new(), Default::default());
            let sink = ChannelSink {
                tx,
                learner,
                attempt: attempt.clone(),
                started: false,
            };
            (sink, rx)
        };
        let mut converter = Converter::new(None);
        let (mut refused, refused_rx) = sink(&older);
        refused
            .write(AudioPacket::Samples(vec![0.5; 8]), &mut converter)
            .unwrap();
        assert!(refused_rx.try_recv().is_err());
        let (mut fine, fine_rx) = sink(&newer);
        fine.write(AudioPacket::Samples(vec![0.5; 8]), &mut converter)
            .unwrap();
        assert_eq!(fine_rx.try_recv().unwrap().len(), 8);
        assert!(!LOADING.lock().iter().any(|a| Arc::ptr_eq(a, &newer)));
        // a refusal with no load under way is nobody's
        assert!(!key_refused());
    }

    #[test]
    fn a_tiny_last_block_is_padded() {
        let mut enc = FlacEncoder::new();
        let mut out = enc.header();
        enc.push(&[100, -100, 200, -200], &mut out);
        enc.finish(&mut out);
        let mut reader = claxon::FlacReader::new(std::io::Cursor::new(out)).unwrap();
        let decoded: Vec<i32> = reader.samples().map(|s| s.unwrap()).collect();
        assert_eq!(decoded.len(), flacenc::constant::MIN_BLOCK_SIZE * 2);
        assert_eq!(&decoded[..4], &[100, -100, 200, -200]);
        assert!(decoded[4..].iter().all(|&s| s == 0));
    }
}
