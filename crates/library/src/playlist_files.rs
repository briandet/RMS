//! Playlist file parsing/writing (`Slim::Formats::Playlists::M3U`, `PLS`).

use std::path::{Path, PathBuf};

use crate::cue::{decode_text, fix_path};

#[derive(Debug, Clone, Default)]
pub struct PlaylistEntry {
    pub url: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub secs: Option<f64>,
}

fn entry_url(entry: &str, base: &Path, media_dirs: &[PathBuf]) -> Option<String> {
    let e = entry.trim();
    if e.is_empty() {
        return None;
    }
    if crate::url::is_remote_url(e) {
        return Some(e.to_string());
    }
    let mut candidates = Vec::new();
    if let Some(p) = fix_path(e, base) {
        candidates.push(p);
    }
    if !Path::new(e).is_absolute() && !crate::url::is_file_url(e) {
        for d in media_dirs {
            if let Some(p) = fix_path(e, d) {
                candidates.push(p);
            }
        }
    }
    for p in &candidates {
        if p.is_file() {
            return Some(crate::url::file_url_from_path(p));
        }
    }
    None
}

/// Parse an M3U/M3U8 playlist.
pub fn parse_m3u(
    bytes: &[u8],
    base: &Path,
    media_dirs: &[PathBuf],
    self_url: &str,
) -> Vec<PlaylistEntry> {
    let text = decode_text(bytes);
    let mut out = Vec::new();
    let mut cur = PlaylistEntry::default();
    let mut ext_url: Option<String> = None;
    let extinf_legacy = crate::re!(r"^#EXTINF:\s*(-?\d+),<(.*)> - <(.*)> - <(.*)>");
    let extinf_secs = crate::re!(r"^#EXTINF:\s*(-?[\d\.]+)\s*,\s*(.*)$");
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if let Some(c) = extinf_legacy.captures(line) {
                cur.secs = c[1].parse().ok();
                cur.artist = Some(c[2].to_string());
                cur.album = Some(c[3].to_string());
                cur.title = Some(c[4].to_string());
            } else if let Some(c) = extinf_secs.captures(line) {
                cur.secs = c[1].parse().ok();
                cur.title = Some(c[2].to_string());
            } else if let Some(t) = line.strip_prefix("#EXTINF:") {
                cur.title = Some(t.to_string());
            } else if let Some(u) = line.strip_prefix("#EXTURL:") {
                ext_url = Some(u.trim().to_string());
            }
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("<html") || lower.starts_with("<!doctype html") {
            break;
        }
        if line.starts_with('<') {
            continue;
        }
        let url = match ext_url.take() {
            Some(u) => Some(u),
            None => entry_url(line, base, media_dirs),
        };
        if let Some(u) = url {
            if u != self_url {
                let mut e = std::mem::take(&mut cur);
                if e.secs.map(|s| s <= 0.0).unwrap_or(false) {
                    e.secs = None;
                }
                e.url = u;
                out.push(e);
            }
        }
        cur = PlaylistEntry::default();
    }
    out
}

/// Parse a PLS playlist.
pub fn parse_pls(
    bytes: &[u8],
    base: &Path,
    media_dirs: &[PathBuf],
    self_url: &str,
) -> Vec<PlaylistEntry> {
    let text = decode_text(bytes).replace("\r\n", "\n").replace('\r', "\n");
    let mut files: std::collections::BTreeMap<u32, String> = Default::default();
    let mut titles: std::collections::BTreeMap<u32, String> = Default::default();
    let re_file = crate::re!(r"(?i)^File(\d+)=(.*)$");
    let re_title = crate::re!(r"(?i)^Title(\d+)=(.*)$");
    for line in text.lines() {
        let line = line.trim();
        if let Some(c) = re_file.captures(line) {
            files.insert(c[1].parse().unwrap_or(0), c[2].trim().to_string());
        } else if let Some(c) = re_title.captures(line) {
            titles.insert(c[1].parse().unwrap_or(0), c[2].trim().to_string());
        }
    }
    let mut out = Vec::new();
    for (n, f) in files {
        if let Some(u) = entry_url(&f, base, media_dirs) {
            if u != self_url {
                out.push(PlaylistEntry {
                    url: u,
                    title: titles.get(&n).cloned(),
                    ..Default::default()
                });
            }
        }
    }
    out
}

/// Write an M3U the way LMS does for saved playlists.
pub fn write_m3u(
    path: &Path,
    entries: &[(String, Option<String>, Option<f64>)],
    add_titles: bool,
) -> std::io::Result<()> {
    let mut s: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
    let mut body = String::new();
    if add_titles {
        body.push_str("#EXTM3U\n");
    }
    for (url, title, secs) in entries {
        body.push_str(&format!("#EXTURL:{}\n", url));
        if add_titles {
            if let Some(t) = title {
                let secs = secs.map(|s| s as i64).unwrap_or(-1);
                body.push_str(&format!("#EXTINF:{},{}\n", secs, t));
            }
        }
        let line = if crate::url::is_file_url(url) && crate::url::anchor_from_url(url).is_none() {
            crate::url::path_from_file_url(url)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| url.clone())
        } else {
            url.clone()
        };
        body.push_str(&line);
        body.push('\n');
    }
    s.extend_from_slice(body.as_bytes());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, s)
}

/// `cleanupFilename`: colon, control chars and slashes → space, strip leading '.'.
pub fn cleanup_filename(name: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for c in name.chars() {
        let bad = c == ':' || c == '/' || c == '\\' || (c as u32) < 0x20;
        if bad {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim_start_matches('.').to_string()
}
