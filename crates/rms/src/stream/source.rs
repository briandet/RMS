//! Byte sources for `/stream.mp3` (`Slim::Player::Source::_readNextChunk`, `Pipeline`).

use super::{SourceKind, StreamSpec};
use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt};
use std::io::SeekFrom;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, warn};

/// `MAXCHUNKSIZE` in the reference server.
pub const CHUNK: usize = 32768;

pub type ByteStream = BoxStream<'static, std::io::Result<Bytes>>;

pub async fn open(spec: &StreamSpec, http: &reqwest::Client) -> std::io::Result<ByteStream> {
    if let Some(shared) = &spec.shared {
        return Ok(shared.subscribe(spec, http));
    }
    match &spec.source {
        SourceKind::File {
            path,
            offset,
            length,
        } => {
            let mut f = tokio::fs::File::open(path).await?;
            if *offset > 0 {
                f.seek(SeekFrom::Start(*offset)).await?;
            }
            let remaining = length.unwrap_or(u64::MAX);
            Ok(
                stream::unfold((f, remaining), |(mut f, remaining)| async move {
                    if remaining == 0 {
                        return None;
                    }
                    let want = (CHUNK as u64).min(remaining) as usize;
                    let mut buf = vec![0u8; want];
                    match f.read(&mut buf).await {
                        Ok(0) => None,
                        Ok(n) => {
                            buf.truncate(n);
                            Some((Ok(Bytes::from(buf)), (f, remaining - n as u64)))
                        }
                        Err(e) => Some((Err(e), (f, 0))),
                    }
                })
                .boxed(),
            )
        }
        SourceKind::Ffmpeg { args } => {
            let mut child = tokio::process::Command::new(crate::server::ffmpeg_path())
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let stdout = child.stdout.take().expect("piped stdout");
            if let Some(mut stderr) = child.stderr.take() {
                crate::util::spawn(async move {
                    let mut s = String::new();
                    let _ = stderr.read_to_string(&mut s).await;
                    let s = s.trim();
                    if !s.is_empty() {
                        warn!("ffmpeg: {}", s);
                    }
                });
            }
            debug!("spawned ffmpeg pid {:?}", child.id());
            Ok(
                stream::unfold((stdout, child), |(mut out, child)| async move {
                    let mut buf = vec![0u8; CHUNK];
                    match out.read(&mut buf).await {
                        Ok(0) => None,
                        Ok(n) => {
                            buf.truncate(n);
                            Some((Ok(Bytes::from(buf)), (out, child)))
                        }
                        Err(e) => Some((Err(e), (out, child))),
                    }
                })
                .boxed(),
            )
        }
        SourceKind::Spotify {
            url,
            start_ms,
            bitrate,
            normalise,
        } => {
            let sp = crate::plugins::spotify::get()
                .ok_or_else(|| std::io::Error::other("Spotify is not available"))?;
            let config = crate::plugins::spotify::stream::player_config(*bitrate, *normalise);
            let flac = spec.format == "flc";
            crate::plugins::spotify::stream::open(sp, url, *start_ms, config, flac).await
        }
        SourceKind::Remote { url } => {
            let resp = http
                .get(url)
                .header("User-Agent", super::remote::USER_AGENT)
                .send()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            if !resp.status().is_success() {
                return Err(std::io::Error::other(format!("HTTP {}", resp.status())));
            }
            Ok(resp
                .bytes_stream()
                .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())))
                .boxed())
        }
    }
}
