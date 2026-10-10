//! Spotify audio for `/stream.mp3`: a librespot player decodes the track (from `start_ms`)
//! into 16-bit stereo at 44.1 kHz, which is sent to the player as FLAC (about half the size
//! of PCM, still lossless) or, for players without FLAC, as little-endian PCM.  Its output
//! goes through a bounded channel, so decoding runs only as fast as the player takes it.

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
use std::sync::Arc;
use tracing::{debug, warn};

/// Format of the PCM produced (what the `strm` header announces).
pub const SAMPLE_RATE: u32 = 44100;
pub const SAMPLE_SIZE: u32 = 16;
pub const CHANNELS: u32 = 2;

/// Chunks of PCM between the player thread and the HTTP stream.
const QUEUE: usize = 8;

/// FLAC block size (samples per channel), the reference encoder's default.
const FLAC_BLOCK: usize = 4096;

/// librespot output: interleaved samples into a channel (blocking when the player is ahead).
struct ChannelSink {
    tx: std::sync::mpsc::SyncSender<Vec<i16>>,
}

impl Sink for ChannelSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        self.tx
            .send(converter.f64_to_s16(&samples))
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
    if kind != "track" {
        return Err(std::io::Error::other("only Spotify tracks can be streamed"));
    }
    let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}"))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let session = spotify.session().await.map_err(std::io::Error::other)?;

    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(QUEUE);
    let player = Player::new(config, session, Box::new(NoOpVolume), move || {
        Box::new(ChannelSink { tx: sync_tx })
    });
    let mut events = player.get_player_event_channel();
    player.load(uri, true, start_ms);

    // Encode on a thread of its own, bridging the player thread's blocking channel to async.
    // The channel closes when the player is released after the end of the track; the last
    // FLAC block goes out then.
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUEUE);
    std::thread::Builder::new()
        .name("spotify-audio".into())
        .spawn(move || {
            let mut flac = flac.then(FlacEncoder::new);
            if let Some(enc) = &flac {
                if tx.blocking_send(enc.header()).is_err() {
                    return;
                }
            }
            while let Ok(samples) = sync_rx.recv() {
                let bytes = match &mut flac {
                    Some(enc) => {
                        let mut out = Vec::new();
                        enc.push(&samples, &mut out);
                        out
                    }
                    None => pcm_bytes(&samples),
                };
                if !bytes.is_empty() && tx.blocking_send(bytes).is_err() {
                    return;
                }
            }
            if let Some(enc) = &mut flac {
                let mut out = Vec::new();
                enc.finish(&mut out);
                let _ = tx.blocking_send(out);
            }
        })
        .map_err(std::io::Error::other)?;

    // Finish when librespot reports the end of the track (all audio is queued by then).
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    let url_owned = url.to_string();
    tokio::spawn(async move {
        let mut done = Some(done_tx);
        while let Some(ev) = events.recv().await {
            let result = match ev {
                PlayerEvent::EndOfTrack { .. } | PlayerEvent::Stopped { .. } => Ok(()),
                PlayerEvent::Unavailable { .. } => Err(format!("{url_owned} is unavailable")),
                _ => continue,
            };
            if let Some(d) = done.take() {
                let _ = d.send(result);
            }
            break;
        }
    });

    struct State {
        rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        done: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
        player: Option<Arc<Player>>,
    }
    impl State {
        /// Shut the player down: its output closes, the bridge drains and ends `rx`.
        fn release(&mut self) {
            if let Some(p) = self.player.take() {
                p.stop();
                // joining the decoder thread may block briefly
                std::thread::spawn(move || drop(p));
            }
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            self.rx.close();
            self.release();
        }
    }
    let state = State {
        rx,
        done: Some(done_rx),
        player: Some(player),
    };
    Ok(stream::unfold(state, |mut st| async move {
        loop {
            tokio::select! {
                chunk = st.rx.recv() => match chunk {
                    Some(c) => return Some((Ok(Bytes::from(c)), st)),
                    // everything decoded has been sent
                    None => return None,
                },
                res = async { st.done.as_mut().expect("pending").await }, if st.done.is_some() => {
                    st.done = None;
                    st.release();
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
