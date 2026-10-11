//! `file://` URL handling compatible with `Slim::Utils::Misc::fileURLFromPath` (URI::file)
//! and `pathFromFileURL`.

use std::path::{Path, PathBuf};

/// Bytes that URI::file leaves unescaped in a path: unreserved + `/:@&=+$,[]`.
fn keep_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
                | b'/'
                | b':'
                | b'@'
                | b'&'
                | b'='
                | b'+'
                | b'$'
                | b','
                | b'['
                | b']'
        )
}

pub fn escape_path_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 8);
    for &b in bytes {
        if keep_byte(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().replace('\\', "/").into_bytes()
}

/// `fileURLFromPath`: absolute path → `file:///...` with LMS escaping.
pub fn file_url_from_path(p: &Path) -> String {
    let mut bytes = path_bytes(p);
    // canonpath-ish: strip trailing slash (except root)
    while bytes.len() > 1 && bytes.ends_with(b"/") {
        bytes.pop();
    }
    if !bytes.starts_with(b"/") {
        bytes.insert(0, b'/');
    }
    format!("file://{}", escape_path_bytes(&bytes))
}

pub fn is_file_url(u: &str) -> bool {
    u.len() >= 7 && u[..7].eq_ignore_ascii_case("file://")
}

pub fn is_remote_url(u: &str) -> bool {
    match u.find("://") {
        Some(i) => {
            let scheme = u[..i].to_ascii_lowercase();
            scheme != "file" && scheme != "tmp" && scheme != "db" && scheme != "playlist"
        }
        None => false,
    }
}

pub fn percent_decode_bytes(s: &str) -> Vec<u8> {
    percent_encoding::percent_decode_str(s).collect()
}

/// `pathFromFileURL`: strips the fragment, percent-decodes to bytes.
pub fn path_from_file_url(u: &str) -> Option<PathBuf> {
    if !is_file_url(u) {
        return None;
    }
    let mut rest = &u[7..];
    // host part (should be empty or localhost)
    if !rest.starts_with('/') {
        {
            let i = rest.find('/')?;
            rest = &rest[i..]
        }
    }
    let rest = match rest.find('#') {
        Some(i) => &rest[..i],
        None => rest,
    };
    let rest = rest.replace('\\', "/");
    if rest.contains("/../") {
        return None;
    }
    let bytes = percent_decode_bytes(&rest);
    Some(bytes_to_path(bytes))
}

#[cfg(unix)]
pub fn bytes_to_path(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
pub fn bytes_to_path(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// `anchorFromURL`: text after the last `#`.
pub fn anchor_from_url(u: &str) -> Option<&str> {
    u.rfind('#').map(|i| &u[i + 1..])
}

/// `isFragment`: (start, end) from an anchor matching `([\d\.]+)-([\d\.]+)`.
pub fn fragment(u: &str) -> Option<(f64, f64)> {
    let a = anchor_from_url(u)?;
    let (s, e) = a.split_once('-')?;
    let s = s.trim();
    let e = e.trim();
    if s.is_empty() || e.is_empty() {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_digit() || c == '.')
        || !e.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return None;
    }
    Some((s.parse().ok()?, e.parse().ok()?))
}

/// `stripAnchorFromURL`: removes `#<digits.>-...` when following a 2–4 char extension.
pub fn strip_anchor(u: &str) -> &str {
    if let Some(i) = u.rfind('#') {
        let (base, anchor) = (&u[..i], &u[i + 1..]);
        if anchor
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.')
            && anchor.contains('-')
        {
            if let Some(dot) = base.rfind('.') {
                let ext = &base[dot + 1..];
                if (2..=4).contains(&ext.len()) && !ext.contains('/') {
                    return base;
                }
            }
        }
    }
    u
}

/// `Slim::Music::Info::fileName`: last path element (decoded).
pub fn file_name(u: &str) -> String {
    if is_file_url(u) {
        if let Some(p) = path_from_file_url(u) {
            return p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned());
        }
    }
    if is_remote_url(u) {
        return String::from_utf8_lossy(&percent_decode_bytes(u)).into_owned();
    }
    Path::new(u)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| u.to_string())
}

/// `plainTitle`: file name without extension, `_` → space.
pub fn plain_title(u: &str, is_dir: bool) -> String {
    let mut t = file_name(u);
    if !is_dir {
        if let Some(i) = t.rfind('.') {
            let ext = &t[i + 1..];
            if !ext.is_empty() && !ext.contains(' ') && !ext.contains('.') && i > 0 {
                t.truncate(i);
            }
        }
    }
    t.replace('_', " ")
}

/// Parent "directory URL" of a URL (dirname).
pub fn dirname_url(u: &str) -> String {
    let base = strip_anchor(u);
    match base.rfind('/') {
        Some(i) => base[..i].to_string(),
        None => base.to_string(),
    }
}

pub fn md5_hex(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_roundtrip() {
        let p = Path::new("/music/Björk/Début/01 Human Behaviour.mp3");
        let u = file_url_from_path(p);
        assert_eq!(
            u,
            "file:///music/Bj%C3%B6rk/D%C3%A9but/01%20Human%20Behaviour.mp3"
        );
        assert_eq!(path_from_file_url(&u).unwrap(), p);
        let p2 = Path::new("/m/a;b?c#d%e (1) [x] & y+z.flac");
        let u2 = file_url_from_path(p2);
        assert_eq!(u2, "file:///m/a%3Bb%3Fc%23d%25e%20(1)%20[x]%20&%20y+z.flac");
        assert_eq!(path_from_file_url(&u2).unwrap(), p2);
    }

    #[test]
    fn anchors() {
        let u = "file:///a/b.flac#12.5-300.25";
        assert_eq!(fragment(u), Some((12.5, 300.25)));
        assert_eq!(strip_anchor(u), "file:///a/b.flac");
        assert_eq!(file_name(u), "b.flac");
        assert_eq!(plain_title("file:///a/01_my_song.mp3", false), "01 my song");
    }
}
