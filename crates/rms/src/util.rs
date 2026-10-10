//! Small shared helpers.

use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn now_secs() -> i64 {
    now() as i64
}

/// Characters escaped in `file://` URLs by the reference server (`Slim::Utils::Misc::fileURLFromPath`):
/// everything outside the unreserved set plus `/`.
const FILE_URL_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b';')
    .add(b'&')
    .add(b'=')
    .add(b'+')
    .add(b'$')
    .add(b',')
    .add(b':')
    .add(b'@');

/// `file:///path/to/file.flac` with the same escaping as the reference server.
pub fn file_url_from_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.replace('\\', "/");
    let s = s.strip_prefix('/').unwrap_or(&s).to_string();
    format!("file:///{}", utf8_percent_encode(&s, FILE_URL_SET))
}

/// Inverse of [`file_url_from_path`]; also strips a `#start-end` cue fragment.
pub fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode_str(rest).decode_utf8_lossy().to_string();
    Some(PathBuf::from(decoded))
}

/// Split a cue-fragment URL `file:///x.flac#12.5-80.25` into (base url, start, end).
pub fn split_url_fragment(url: &str) -> (&str, Option<f64>, Option<f64>) {
    if let Some((base, frag)) = url.rsplit_once('#') {
        if let Some((s, e)) = frag.split_once('-') {
            let start = s.parse::<f64>().ok();
            let end = e.parse::<f64>().ok();
            if start.is_some() || end.is_some() {
                return (base, start, end);
            }
        }
    }
    (url, None, None)
}

pub fn is_remote_url(url: &str) -> bool {
    !url.starts_with("file://") && url.contains("://")
}

/// URL-decode a CLI parameter (`%20` → space; `+` is *not* a space in the LMS CLI).
pub fn cli_unescape(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// URL-encode a CLI value like `URI::Escape::uri_escape_utf8` with the reference server's
/// unsafe set (everything but `A-Za-z0-9_.!~*'()-` … and we also keep `:` and `/` readable
/// is *not* done by LMS, so we escape them too).
pub fn cli_escape(s: &str) -> String {
    const SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}')
        .add(b';')
        .add(b'/')
        .add(b':')
        .add(b'@')
        .add(b'&')
        .add(b'=')
        .add(b'+')
        .add(b'$')
        .add(b',');
    utf8_percent_encode(s, SET).to_string()
}

/// Format seconds as `m:ss` or `h:mm:ss`.
pub fn format_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

/// Lower-cased file extension without the dot.
pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        let p = Path::new("/music/Artist Name/Album #1/01 - Song?.flac");
        let u = file_url_from_path(p);
        assert_eq!(
            u,
            "file:///music/Artist%20Name/Album%20%231/01%20-%20Song%3F.flac"
        );
        assert_eq!(path_from_file_url(&u).unwrap(), p);
        assert_eq!(
            split_url_fragment("file:///a.flac#10.5-20"),
            ("file:///a.flac", Some(10.5), Some(20.0))
        );
        assert_eq!(
            split_url_fragment("file:///a#1.flac"),
            ("file:///a#1.flac", None, None)
        );
        assert!(is_remote_url("http://x/y.mp3"));
        assert!(!is_remote_url("file:///x.mp3"));
    }

    #[test]
    fn cli_escaping() {
        assert_eq!(cli_escape("Hello World/a:b"), "Hello%20World%2Fa%3Ab");
        assert_eq!(cli_unescape("Hello%20World"), "Hello World");
        assert_eq!(format_duration(3725.0), "1:02:05");
        assert_eq!(format_duration(65.0), "1:05");
    }
}

/// Best guess of this host's LAN IP address (for `serverstatus ip`).
pub fn server_ip() -> String {
    local_ip_address::local_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}
