//! Spotify audio for `/stream.mp3`: a librespot player decodes the track (from `start_ms`)
//! into 16-bit little-endian stereo PCM at 44.1 kHz.  Its output goes through a bounded
//! channel, so decoding runs only as fast as the Squeezebox player takes the stream.

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

/// librespot output: PCM bytes into a channel (blocking when the player is ahead).
struct ChannelSink {
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
}

impl Sink for ChannelSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        let pcm = converter.f64_to_s16(&samples);
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.tx
            .send(bytes)
            .map_err(|_| SinkError::OnWrite("stream closed".into()))
    }
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

/// Open the PCM stream of a track.
pub async fn open(
    spotify: &Arc<Spotify>,
    url: &str,
    start_ms: u32,
    config: PlayerConfig,
) -> std::io::Result<crate::stream::source::ByteStream> {
    let (kind, id) =
        super::parse_url(url).ok_or_else(|| std::io::Error::other("bad Spotify URL"))?;
    if kind != "track" {
        return Err(std::io::Error::other("only Spotify tracks can be streamed"));
    }
    let uri = SpotifyUri::from_uri(&format!("spotify:track:{id}"))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let session = spotify.session().await.map_err(std::io::Error::other)?;

    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(QUEUE);
    let player = Player::new(config, session, Box::new(NoOpVolume), move || {
        Box::new(ChannelSink { tx: sync_tx })
    });
    let mut events = player.get_player_event_channel();
    player.load(uri, true, start_ms);

    // Bridge the player thread's blocking channel to async (outside any runtime context).
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(QUEUE);
    std::thread::Builder::new()
        .name("spotify-pcm".into())
        .spawn(move || {
            while let Ok(chunk) = sync_rx.recv() {
                if tx.blocking_send(chunk).is_err() {
                    break;
                }
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
