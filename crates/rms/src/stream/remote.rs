//! Remote (internet radio / HTTP) stream helpers: probing a URL, parsing playlist formats,
//! ICY metadata and building the request string players use for direct streaming
//! (`Slim::Utils::Scanner::Remote`, `Slim::Formats::Playlists::*`,
//! `Slim::Player::Protocols::HTTP`).

use super::type_for_mime;
use std::time::Duration;
use tracing::{debug, warn};

pub const USER_AGENT: &str = concat!(
    "Lyrion Music Server/9.2.0 (RMS ",
    env!("CARGO_PKG_VERSION"),
    ")"
);

/// Result of probing a URL.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    /// URL after redirects.
    pub url: String,
    /// LMS content type of the audio (`mp3`, `aac`, `ogg`, `flc`, …).
    pub content_type: Option<String>,
    /// For playlists: the contained entries `(url, title)`.
    pub entries: Vec<(String, Option<String>)>,
    pub title: Option<String>,
    pub bitrate: Option<u32>,
    pub is_hls: bool,
}

/// Guess a type from the URL's extension.
pub fn type_from_extension(url: &str) -> Option<&'static str> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    let ext = path.rsplit('.').next().unwrap_or("");
    Some(match ext {
        "mp3" => "mp3",
        "aac" | "aacp" => "aac",
        "m4a" | "mp4" => "mp4",
        "ogg" | "oga" => "ogg",
        "opus" => "ops",
        "flac" | "flc" => "flc",
        "wav" => "wav",
        "aif" | "aiff" => "aif",
        "wma" => "wma",
        "pls" => "pls",
        "m3u" => "m3u",
        "m3u8" => "m3u8",
        "asx" => "asx",
        "xspf" => "xspf",
        _ => return None,
    })
}

pub fn is_playlist_type(t: &str) -> bool {
    matches!(t, "pls" | "m3u" | "m3u8" | "asx" | "xspf")
}

/// Fetch headers (and for playlists the body) of `url`, following redirects.
pub async fn probe(client: &reqwest::Client, url: &str) -> Probe {
    let mut probe = Probe {
        url: url.to_string(),
        ..Default::default()
    };
    let ext_type = type_from_extension(url);
    let resp = client
        .get(url)
        .header("Icy-MetaData", "1")
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            // Shoutcast v1 answers "ICY 200 OK" which is not valid HTTP; treat as mp3 radio.
            warn!("probe {} failed: {} — assuming direct audio", url, e);
            probe.content_type = Some(
                ext_type
                    .filter(|t| !is_playlist_type(t))
                    .unwrap_or("mp3")
                    .to_string(),
            );
            return probe;
        }
    };
    probe.url = resp.url().to_string();
    let headers = resp.headers().clone();
    let mime = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    probe.title = headers
        .get("icy-name")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    probe.bitrate = headers
        .get("icy-br")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(',')
                .next()
                .and_then(|x| x.trim().parse::<u32>().ok())
        })
        .map(|kbps| kbps * 1000);
    let mut t = type_for_mime(&mime).map(str::to_string);
    if t.is_none()
        || t.as_deref() == Some("html")
        || mime.starts_with("text/plain")
        || mime.starts_with("application/octet-stream")
    {
        if let Some(e) = ext_type {
            t = Some(e.to_string());
        }
    }
    let lms_type = t.unwrap_or_else(|| "mp3".to_string());
    debug!("probe {} -> mime {} type {}", url, mime, lms_type);
    if is_playlist_type(&lms_type) || lms_type == "m3u" {
        let body = read_limited(resp, 256 * 1024).await.unwrap_or_default();
        let text = decode_text(&body);
        if text.trim_start().starts_with("#EXTM3U")
            && (text.contains("#EXT-X-") || lms_type == "m3u8")
            && (text.contains("#EXT-X-TARGETDURATION")
                || text.contains("#EXT-X-STREAM-INF")
                || text.contains("#EXT-X-MEDIA-SEQUENCE"))
        {
            probe.is_hls = true;
            probe.content_type = Some("aac".into());
            return probe;
        }
        probe.entries = parse_playlist(&text, &lms_type, &probe.url);
        return probe;
    }
    probe.content_type = Some(lms_type);
    probe
}

async fn read_limited(resp: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    use futures::StreamExt;
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        out.extend_from_slice(&chunk);
        if out.len() >= limit {
            break;
        }
    }
    Some(out)
}

pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.contains("://") {
        return href.to_string();
    }
    url::Url::parse(base)
        .and_then(|b| b.join(href))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| href.to_string())
}

/// Parse a playlist body by type (`pls`, `m3u`, `asx`, `xspf`).  Falls back to sniffing.
pub fn parse_playlist(text: &str, kind: &str, base: &str) -> Vec<(String, Option<String>)> {
    let trimmed = text.trim_start();
    let kind = if trimmed.starts_with("[playlist]")
        || trimmed.to_ascii_lowercase().starts_with("[playlist]")
    {
        "pls"
    } else if trimmed.starts_with("<?xml") && trimmed.contains("<playlist") {
        "xspf"
    } else if trimmed.to_ascii_lowercase().starts_with("<asx") {
        "asx"
    } else {
        kind
    };
    match kind {
        "pls" => parse_pls(text, base),
        "asx" => parse_asx(text, base),
        "xspf" => parse_xspf(text, base),
        _ => parse_m3u(text, base),
    }
}

pub fn parse_pls(text: &str, base: &str) -> Vec<(String, Option<String>)> {
    let mut files: std::collections::BTreeMap<u32, (Option<String>, Option<String>)> =
        Default::default();
    for line in text.lines() {
        let line = line.trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let kl = k.trim().to_ascii_lowercase();
        if let Some(n) = kl.strip_prefix("file") {
            if let Ok(n) = n.parse() {
                files.entry(n).or_default().0 = Some(resolve(base, v));
            }
        } else if let Some(n) = kl.strip_prefix("title") {
            if let Ok(n) = n.parse() {
                files.entry(n).or_default().1 = Some(v.trim().to_string());
            }
        }
    }
    files
        .into_values()
        .filter_map(|(u, t)| u.map(|u| (u, t)))
        .collect()
}

pub fn parse_m3u(text: &str, base: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut title: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            title = info
                .split_once(',')
                .map(|(_, t)| t.trim().to_string())
                .filter(|t| !t.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        out.push((resolve(base, line), title.take()));
    }
    out
}

pub fn parse_asx(text: &str, base: &str) -> Vec<(String, Option<String>)> {
    // ASX is frequently not well-formed XML; scan for <ref href="..."> case-insensitively.
    let re = regex::Regex::new(r#"(?is)<entry\b.*?</entry>|<ref\s+href\s*=\s*["']([^"']+)["']"#)
        .unwrap();
    let href_re = regex::Regex::new(r#"(?i)<ref\s+href\s*=\s*["']([^"']+)["']"#).unwrap();
    let title_re = regex::Regex::new(r"(?is)<title>\s*(.*?)\s*</title>").unwrap();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        let s = m.as_str();
        let title = title_re.captures(s).map(|c| c[1].to_string());
        for c in href_re.captures_iter(s) {
            out.push((resolve(base, &c[1].replace("&amp;", "&")), title.clone()));
        }
    }
    out
}

pub fn parse_xspf(text: &str, base: &str) -> Vec<(String, Option<String>)> {
    let Ok(doc) = roxmltree::Document::parse(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for track in doc.descendants().filter(|n| n.has_tag_name("track")) {
        let loc = track
            .children()
            .find(|n| n.has_tag_name("location"))
            .and_then(|n| n.text());
        let title = track
            .children()
            .find(|n| n.has_tag_name("title"))
            .and_then(|n| n.text())
            .map(str::to_string);
        if let Some(l) = loc {
            out.push((resolve(base, l), title));
        }
    }
    out
}

/// Parse ICY metadata (`StreamTitle='Artist - Title';StreamUrl='...';`).
pub fn parse_icy_metadata(meta: &[u8]) -> (Option<String>, Option<String>) {
    let text = decode_text(meta);
    let text = text.trim_end_matches('\0');
    let mut title = None;
    let mut stream_url = None;
    let re = regex::Regex::new(r"(?s)(\w+)='(.*?)';").unwrap();
    for c in re.captures_iter(text) {
        match &c[1] {
            "StreamTitle" => title = Some(c[2].trim().to_string()).filter(|s| !s.is_empty()),
            "StreamUrl" => stream_url = Some(c[2].trim().to_string()).filter(|s| !s.is_empty()),
            _ => {}
        }
    }
    (title, stream_url)
}

/// HTTP request text sent inside `strm s` when the player fetches a remote URL itself
/// (`Slim::Player::Protocols::HTTP::requestString`).
pub fn direct_request_string(u: &url::Url) -> String {
    let mut path = u.path().to_string();
    if let Some(q) = u.query() {
        path.push('?');
        path.push_str(q);
    }
    if path.is_empty() {
        path.push('/');
    }
    // `Url::port()` is None for the scheme's default port, which LMS also omits.
    let host = match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{}:{}", h, p),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    };
    let mut r = format!(
        "GET {} HTTP/1.0\r\nAccept: */*\r\nCache-Control: no-cache\r\nUser-Agent: {}\r\nIcy-MetaData: 1\r\nConnection: close\r\nHost: {}\r\n",
        path, USER_AGENT, host
    );
    if !u.username().is_empty() {
        use base64::Engine;
        let cred = format!(
            "{}:{}",
            percent_encoding::percent_decode_str(u.username()).decode_utf8_lossy(),
            u.password().unwrap_or("")
        );
        r.push_str(&format!(
            "Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(cred)
        ));
    }
    r.push_str("\r\n");
    r
}

/// Parsed response headers a player reports in `RESP` for a direct stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DirectHeaders {
    pub status: u16,
    pub status_line: String,
    pub metaint: u32,
    pub redirect: Option<String>,
    pub content_type: Option<String>,
    pub title: Option<String>,
    pub bitrate: Option<u32>,
    pub length: Option<u64>,
}

/// `Slim::Player::Protocols::HTTP::parseDirectHeaders`.
pub fn parse_direct_headers(raw: &[u8]) -> Option<DirectHeaders> {
    let text = String::from_utf8_lossy(raw)
        .trim_end_matches('\0')
        .replace('\r', "\n");
    let mut lines = text.split('\n').filter(|l| !l.is_empty());
    let status_line = lines.next()?.to_string();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())?;
    let mut h = DirectHeaders {
        status: code,
        status_line,
        ..Default::default()
    };
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        match k.as_str() {
            "icy-metaint" => h.metaint = v.parse().unwrap_or(0),
            "icy-name" | "x-audiocast-name" => {
                h.title = Some(v.to_string()).filter(|s| !s.is_empty())
            }
            "icy-br" | "x-audiocast-bitrate" => {
                h.bitrate = v
                    .split(',')
                    .next()
                    .and_then(|x| x.trim().parse::<u32>().ok())
                    .map(|b| if b < 1000 { b * 1000 } else { b })
            }
            "location" => h.redirect = Some(v.to_string()),
            "content-type" => h.content_type = type_for_mime(v).map(str::to_string),
            "content-length" => h.length = v.parse().ok(),
            _ => {}
        }
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlists() {
        let pls = "[playlist]\nNumberOfEntries=2\nFile1=http://a/1\nTitle1=One\nFile2=http://a/2\n";
        assert_eq!(
            parse_playlist(pls, "pls", "http://x/"),
            vec![
                ("http://a/1".into(), Some("One".into())),
                ("http://a/2".into(), None)
            ]
        );
        let m3u = "#EXTM3U\n#EXTINF:-1,Radio\nhttp://s/stream\nrel.mp3\n";
        assert_eq!(
            parse_m3u(m3u, "http://h/dir/list.m3u"),
            vec![
                ("http://s/stream".into(), Some("Radio".into())),
                ("http://h/dir/rel.mp3".into(), None)
            ]
        );
        let asx =
            "<ASX version=\"3.0\"><Entry><Title>T</Title><Ref href=\"http://w/1\"/></Entry></ASX>";
        assert_eq!(
            parse_asx(asx, ""),
            vec![("http://w/1".into(), Some("T".into()))]
        );
        let xspf = r#"<?xml version="1.0"?><playlist xmlns="http://xspf.org/ns/0/"><trackList><track><location>http://x/a.mp3</location><title>A</title></track></trackList></playlist>"#;
        assert_eq!(
            parse_playlist(xspf, "xspf", ""),
            vec![("http://x/a.mp3".into(), Some("A".into()))]
        );
    }

    #[test]
    fn icy_and_headers() {
        let (t, u) = parse_icy_metadata(b"StreamTitle='Artist - Song';StreamUrl='http://x';\0\0\0");
        assert_eq!(t.as_deref(), Some("Artist - Song"));
        assert_eq!(u.as_deref(), Some("http://x"));
        let h = parse_direct_headers(b"ICY 200 OK\r\nicy-name:Cool FM\r\nicy-br:128\r\nicy-metaint:16000\r\nContent-Type: audio/mpeg\r\n\r\n").unwrap();
        assert_eq!(h.status, 200);
        assert_eq!(h.metaint, 16000);
        assert_eq!(h.bitrate, Some(128000));
        assert_eq!(h.content_type.as_deref(), Some("mp3"));
        let u = url::Url::parse("http://radio.example:8000/live?x=1").unwrap();
        let r = direct_request_string(&u);
        assert!(r.starts_with("GET /live?x=1 HTTP/1.0\r\n"));
        assert!(r.contains("Host: radio.example:8000\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
    }
}
