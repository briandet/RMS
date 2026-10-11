//! `_songData`: render a playlist entry / song for the given tag letters.

use crate::player::playlist::Entry;
use crate::player::song::Song;
use crate::server::Server;
use serde_json::{json, Map, Value};

pub type Row = Map<String, Value>;

/// Stable negative id for a remote URL (LMS gives `RemoteTrack`s negative ids).
pub fn remote_id(url: &str) -> i64 {
    let d = md5::compute(url.as_bytes());
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) & 0x7fff_ffff;
    -(n as i64).max(1)
}

/// Render one track.  `song` (when given) supplies live metadata for remote streams.
pub fn song_row(server: &Server, entry: &Entry, song: Option<&Song>, tags: &str) -> Row {
    if !crate::util::is_remote_url(&entry.url) {
        if let Some(t) = server.library_track(entry) {
            return server.library.track_row(&t, tags);
        }
        let mut r = Row::new();
        r.insert("id".into(), json!(remote_id(&entry.url)));
        r.insert(
            "title".into(),
            json!(crate::player::controller::title_from_url(&entry.url)),
        );
        for c in tags.chars() {
            if c == 'u' {
                r.insert("url".into(), json!(entry.url));
            }
        }
        return r;
    }
    remote_row(server, &entry.url, song, tags)
}

fn remote_row(server: &Server, url: &str, song: Option<&Song>, tags: &str) -> Row {
    let info = server.track_info_for(&Entry::new(url));
    let station = song
        .and_then(|s| s.remote_meta.station.clone())
        .or(info.title.clone());
    let live_title = song.and_then(|s| s.remote_meta.title.clone());
    let title = live_title
        .clone()
        .or(station.clone())
        .unwrap_or_else(|| url.to_string());
    let mut r = Row::new();
    r.insert("id".into(), json!(remote_id(url)));
    r.insert("title".into(), json!(title));
    let mut seen = std::collections::HashSet::new();
    let (artist, track_title) = match &live_title {
        Some(t) => match t.split_once(" - ") {
            Some((a, b)) => (Some(a.trim().to_string()), Some(b.trim().to_string())),
            None => (None, None),
        },
        None => (None, None),
    };
    let _ = track_title;
    // tracks of a service (Spotify) carry their own artist and album
    let artist = song
        .and_then(|s| s.remote_meta.artist.clone())
        .or(info.artist.clone())
        .or(artist);
    let album = song
        .and_then(|s| s.remote_meta.album.clone())
        .or(info.album.clone());
    for c in tags.chars() {
        if !seen.insert(c) {
            continue;
        }
        match c {
            'u' => {
                r.insert("url".into(), json!(url));
            }
            'o' => {
                let ct = song
                    .map(|s| s.stream_format.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(info.content_type.clone());
                if !ct.is_empty() {
                    r.insert("type".into(), json!(ct));
                }
            }
            'a' => {
                if let Some(a) = &artist {
                    r.insert("artist".into(), json!(a));
                }
            }
            'A' => {
                if let Some(a) = &artist {
                    r.insert("artist".into(), json!(a));
                }
            }
            'l' => {
                if let Some(a) = &album {
                    r.insert("album".into(), json!(a));
                }
            }
            'x' => {
                r.insert("remote".into(), json!(1));
            }
            'd' => {
                let d = song.and_then(|s| s.duration()).or(info.secs).unwrap_or(0.0);
                r.insert("duration".into(), json!(d));
            }
            'r' => {
                if let Some(b) = song.and_then(|s| s.stream_bitrate).or(info.bitrate) {
                    r.insert("bitrate".into(), json!(format!("{}kbps", b / 1000)));
                }
            }
            'K' => {
                if let Some(c) = song
                    .and_then(|s| s.remote_meta.cover.clone())
                    .or(info.artwork_url.clone())
                {
                    r.insert("artwork_url".into(), json!(proxied_image(&c)));
                }
            }
            'N' => {
                if live_title.is_some() {
                    if let Some(s) = &station {
                        r.insert("remote_title".into(), json!(s));
                    }
                }
            }
            'V' => {
                r.insert(
                    "live_edge".into(),
                    json!(if song.map(|s| s.is_live()).unwrap_or(true) {
                        0
                    } else {
                        -1
                    }),
                );
            }
            _ => {}
        }
    }
    r
}

/// `Slim::Web::ImageProxy::proxiedImage`: route remote images through `/imageproxy`.
pub fn proxied_image(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        format!("/imageproxy/{}/image.png", crate::util::cli_escape(url))
    } else {
        url.to_string()
    }
}

/// `buildPrettyBitRate`.
pub fn pretty_bitrate(bitrate: u32, vbr: bool, format: &str) -> String {
    let kbps = bitrate / 1000;
    if crate::stream::is_lossless(format) {
        format!("{}kbps", kbps)
    } else {
        format!("{}kbps {}", kbps, if vbr { "VBR" } else { "CBR" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let a = remote_id("http://x");
        assert!(a < 0);
        assert_eq!(a, remote_id("http://x"));
        assert_ne!(a, remote_id("http://y"));
    }
}
