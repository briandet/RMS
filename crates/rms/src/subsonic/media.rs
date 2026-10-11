//! Media: streaming (as stored, or transcoded with ffmpeg), downloads and cover art.

use super::browse::{cover_art_of_track, parse_id, song_reply};
use super::{Error, Params, Reply, Result};
use crate::server::Server;
use crate::web::artwork;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::Arc;

/// Bit rates `maxBitRate` is rounded down to.
const RATES: &[u32] = &[32, 48, 64, 80, 96, 112, 128, 160, 192, 256, 320];

/// `stream`: the file as it is when the client can take it (no `format`, or `raw`, and
/// within `maxBitRate`), otherwise transcoded by ffmpeg to MP3 (or Opus with
/// `format=opus`).  Cue sheet tracks are cut out of their file; `timeOffset` starts later
/// (OpenSubsonic `transcodeOffset`).
pub async fn stream(server: &Arc<Server>, p: &Params, headers: &HeaderMap) -> Result<Reply> {
    let t = song_reply(server, p)?;
    let format = p.get("format").filter(|f| !f.is_empty());
    let max = p.int("maxBitRate")?.unwrap_or(0).max(0) as u32;
    let offset = p.int("timeOffset")?.unwrap_or(0).max(0) as f64;
    let kbps = t.bitrate.unwrap_or(0) / 1000;
    let segment = t.cue_start.is_some() || t.cue_end.is_some();
    let as_is =
        format == Some("raw") || (format.is_none() && (max == 0 || (kbps > 0 && kbps <= max)));
    if as_is && !segment && offset == 0.0 {
        let mut r = artwork::download(server, &t.id.to_string(), headers).await;
        r.headers_mut().remove(header::CONTENT_DISPOSITION);
        return Ok(Reply::Raw(r));
    }
    if !server.has_ffmpeg() {
        return Err(Error::new(
            0,
            "Transcoding needs ffmpeg, which this server does not have",
        ));
    }
    let path = t.path().ok_or_else(|| Error::not_found("Song"))?;
    let opus = format == Some("opus");
    // a segment asked for as it is: lossless FLAC of just that part
    let target = if as_is && segment { "flc" } else { "mp3" };
    let rate = if max == 0 {
        if opus {
            128
        } else {
            320
        }
    } else {
        RATES
            .iter()
            .copied()
            .filter(|r| *r <= max)
            .max()
            .unwrap_or(32)
    };
    let info = crate::server::track_info_from_library(&t);
    let start = t.cue_start.unwrap_or(0.0) + offset;
    let mut args = crate::stream::ffmpeg_args(
        &path.to_string_lossy(),
        target,
        start,
        t.cue_end,
        rate,
        None,
        &info,
    );
    if opus && target == "mp3" {
        for a in args.iter_mut() {
            match a.as_str() {
                "libmp3lame" => *a = "libopus".into(),
                "mp3" => *a = "ogg".into(),
                _ => {}
            }
        }
    }
    let mime = match (target, opus) {
        ("flc", _) => "audio/flac",
        (_, true) => "audio/ogg",
        _ => "audio/mpeg",
    };
    let spec = crate::stream::StreamSpec {
        source: crate::stream::SourceKind::Ffmpeg { args },
        format: target.into(),
        song_id: 0,
        shared: None,
    };
    let body = crate::stream::source::open(&spec, &server.http)
        .await
        .map_err(|e| Error::new(0, format!("Cannot transcode: {e}")))?;
    let mut r = axum::body::Body::from_stream(body).into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if p.flag("estimateContentLength", false) {
        let secs = t
            .cue_end
            .or(t.secs.map(|s| s + t.cue_start.unwrap_or(0.0)))
            .unwrap_or(0.0)
            - start;
        let len = (secs.max(0.0) * rate as f64 * 125.0) as u64;
        r.headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    Ok(Reply::Raw(r))
}

pub async fn download(server: &Arc<Server>, p: &Params, headers: &HeaderMap) -> Result<Reply> {
    let t = song_reply(server, p)?;
    Ok(Reply::Raw(
        artwork::download(server, &t.id.to_string(), headers).await,
    ))
}

/// `getCoverArt`: an id from a `coverArt` field (an album's or track's cover, or `pt-…` for
/// an artist's picture), or an album, song or artist id.
pub async fn cover_art(server: &Arc<Server>, p: &Params) -> Result<Reply> {
    let id = p.require("id")?.to_string();
    let spec = match p.int("size")? {
        Some(s) if s > 0 => format!("cover_{s}x{s}_o"),
        _ => "cover".to_string(),
    };
    let portrait = |pid: String| async {
        let r = artwork::contributor(State(server.clone()), Path((pid, spec.clone()))).await;
        Ok(Reply::Raw(r))
    };
    if let Some(pid) = id.strip_prefix("pt-") {
        return portrait(pid.to_string()).await;
    }
    let cover = if let Some(n) = id.strip_prefix("al-") {
        let a = n.parse().ok().and_then(|n| server.library.album_info(n));
        a.ok_or_else(|| Error::not_found("Album"))?.cover_id
    } else if id.starts_with("tr-") {
        let t = parse_id(&id, "tr").and_then(|n| server.library.track(n));
        cover_art_of_track(&t.ok_or_else(|| Error::not_found("Song"))?)
    } else if id.starts_with("ar-") {
        let a = parse_id(&id, "ar").and_then(|n| server.library.artist_info(n));
        match a.ok_or_else(|| Error::not_found("Artist"))?.portrait_id {
            Some(pid) => return portrait(pid).await,
            None => return Err(Error::not_found("Cover art")),
        }
    } else {
        Some(id.clone())
    };
    let cover = cover.ok_or_else(|| Error::not_found("Cover art"))?;
    let r: Response = artwork::music(
        State(server.clone()),
        Path((cover, spec)),
        Query(HashMap::new()),
        HeaderMap::new(),
    )
    .await;
    Ok(Reply::Raw(r))
}
