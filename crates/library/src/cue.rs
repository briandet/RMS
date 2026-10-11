//! Cue sheet parser (`Slim::Formats::Playlists::CUE::parse`).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use indexmap::IndexMap;

use crate::tags::Attrs;

const STANDARD: &[&str] = &[
    "CATALOG",
    "CDTEXTFILE",
    "FILE",
    "FLAGS",
    "INDEX",
    "ISRC",
    "PERFORMER",
    "POSTGAP",
    "PREGAP",
    "REM",
    "SONGWRITER",
    "TITLE",
    "TRACK",
];
const REFUSED: &[&str] = &["CDTEXTFILE", "FLAGS", "PREGAP", "POSTGAP"];
const REFUSED_REM: &[&str] = &[
    "ALBUM",
    "AUDIO",
    "CONTENT_TYPE",
    "DRM",
    "FILE",
    "FILENAME",
    "FILESIZE",
    "INDEX",
    "LOSSLESS",
    "OFFSET",
    "SECS",
    "SIZE",
    "START",
    "TIMESTAMP",
    "TITLESORT",
    "TRACK",
    "URI",
    "VIRTUAL",
];
const ALBUM_ONLY: &[&str] = &[
    "ALBUMARTIST",
    "ALBUMARTISTSORT",
    "ALBUMSORT",
    "COMPILATION",
    "DISCC",
    "CATALOG",
    "ISRC",
    "MUSICBRAINZ_ALBUM_ID",
    "MUSICBRAINZ_ALBUMARTIST_ID",
    "MUSICBRAINZ_ALBUM_TYPE",
    "MUSICBRAINZ_ALBUM_STATUS",
    "REPLAYGAIN_ALBUM_GAIN",
    "REPLAYGAIN_ALBUM_PEAK",
];

#[derive(Debug, Clone, Default)]
pub struct CueTrack {
    pub tracknum: u32,
    /// file path of the referenced audio file
    pub file: PathBuf,
    pub start: f64,
    pub end: f64,
    pub attrs: IndexMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct CueSheet {
    pub album: IndexMap<String, String>,
    pub tracks: Vec<CueTrack>,
}

/// Decode cue/playlist bytes: UTF-8 (BOM stripped) else Windows-1252.
pub fn decode_text(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if b.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode(&b[2..]).0.into_owned();
    }
    if b.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode(&b[2..]).0.into_owned();
    }
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(b).0.into_owned(),
    }
}

fn remove_quotes(v: &str) -> String {
    let v = v.trim();
    if let Some(stripped) = v.strip_prefix('"') {
        if let Some(i) = stripped.rfind('"') {
            return stripped[..i].to_string();
        }
        return stripped.to_string();
    }
    v.to_string()
}

/// Normalise `a/b/../c` without touching the filesystem.
pub fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `fixPath($file, $base)` for local entries.
pub fn fix_path(entry: &str, base: &Path) -> Option<PathBuf> {
    let e = entry.trim();
    if e.is_empty() {
        return None;
    }
    if crate::url::is_file_url(e) {
        return crate::url::path_from_file_url(e);
    }
    let mut s = e.replace('\\', "/");
    // strip a windows drive letter
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        s = s[2..].to_string();
    }
    let p = Path::new(&s);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    Some(normalize_path(&full))
}

fn cue_time(v: &str) -> Option<f64> {
    let mut it = v.trim().split(':');
    let m: f64 = it.next()?.trim().parse().ok()?;
    let s: f64 = it.next()?.trim().parse().ok()?;
    let f: f64 = it.next()?.split_whitespace().next()?.parse().ok()?;
    Some(m * 60.0 + s + f / 75.0)
}

#[derive(Default)]
struct RawTrack {
    file: Option<PathBuf>,
    start: Option<f64>,
    end: Option<f64>,
    attrs: IndexMap<String, String>,
}

fn add_cmd(
    album: &mut IndexMap<String, String>,
    tracks: &mut BTreeMap<u32, RawTrack>,
    in_album: bool,
    cur: Option<u32>,
    cmd: &str,
    val: String,
) {
    if in_album {
        if !album.contains_key(cmd) {
            album.insert(cmd.to_string(), val);
        }
    } else if let Some(t) = cur.and_then(|c| tracks.get_mut(&c)) {
        if !t.attrs.contains_key(cmd) {
            t.attrs.insert(cmd.to_string(), val);
        }
    }
}

fn merge_cmd(
    old: &str,
    new: &str,
    from: &mut IndexMap<String, String>,
    to: &mut IndexMap<String, String>,
) {
    if let Some(v) = from.shift_remove(old) {
        to.entry(new.to_string()).or_insert(v);
    }
}

fn merge_self(old: &str, new: &str, ctx: &mut IndexMap<String, String>) {
    if let Some(v) = ctx.shift_remove(old) {
        ctx.entry(new.to_string()).or_insert(v);
    }
}

/// Parse a cue sheet. `embedded` = Some(path of the FLAC) for embedded sheets.
/// `file_info(path)` returns (duration secs, album-level attrs) of a referenced audio file.
pub fn parse(
    lines: &[String],
    base_dir: &Path,
    embedded: Option<&Path>,
    mut file_info: impl FnMut(&Path) -> Option<(Option<f64>, Attrs)>,
) -> Option<CueSheet> {
    if lines.is_empty() {
        return None;
    }
    let mut album: IndexMap<String, String> = IndexMap::new();
    let mut tracks: BTreeMap<u32, RawTrack> = BTreeMap::new();
    let mut filename: Option<PathBuf> = None;
    let mut cur: Option<u32> = None;
    let mut in_album = true;
    let mut file_changed = false;
    let mut last_cur: Option<u32> = None;

    for raw in lines {
        let line = raw.trim_start_matches('\u{feff}').trim_end();
        let trimmed = line.trim_start();
        let Some(sp) = trimmed.find(char::is_whitespace) else {
            continue;
        };
        let command = &trimmed[..sp];
        let value = trimmed[sp..].trim_start();
        if value.is_empty() {
            continue;
        }
        let command_uc = command.to_string();
        if !STANDARD.contains(&command_uc.as_str()) || REFUSED.contains(&command_uc.as_str()) {
            continue;
        }
        match command_uc.as_str() {
            "TRACK" => {
                in_album = false;
                file_changed = false;
                let mut it = value.split_whitespace();
                let num = it.next().and_then(|n| n.parse::<u32>().ok());
                let kind = it.next().unwrap_or("");
                if let (Some(n), true, Some(f)) =
                    (num, kind.eq_ignore_ascii_case("AUDIO"), filename.clone())
                {
                    cur = Some(n);
                    last_cur = Some(n);
                    tracks.entry(n).or_default().file = Some(f);
                } else {
                    cur = None;
                }
            }
            "INDEX" => {
                let Some(c) = cur else { continue };
                let mut it = value.split_whitespace();
                let idx = it.next().unwrap_or("");
                let t = it.next().and_then(cue_time);
                if idx == "00" {
                    // PREGAP – recorded but unused
                } else if idx == "01" {
                    if file_changed {
                        tracing::warn!("Found INDEX 01 after FILE but before TRACK. Looks like non-compliant CUE. Skipping.");
                        return None;
                    }
                    if let (Some(t), Some(tr)) = (t, tracks.get_mut(&c)) {
                        tr.start = Some(t);
                    }
                }
            }
            "REM" => {
                if value.starts_with('"') {
                    continue;
                }
                let mut parts = value.splitn(2, char::is_whitespace);
                let rc = parts.next().unwrap_or("").to_string();
                let rv = parts.next().map(|s| s.trim_start().to_string());
                let Some(rv) = rv else { continue };
                if rc.is_empty()
                    || REFUSED_REM.contains(&rc.as_str())
                    || REFUSED.contains(&rc.as_str())
                {
                    continue;
                }
                match rc.as_str() {
                    "END" => {
                        if let Some(tr) = cur.and_then(|c| tracks.get_mut(&c)) {
                            tr.end = cue_time(&rv).or_else(|| rv.trim().parse::<f64>().ok());
                        }
                    }
                    "REPLAYGAIN_ALBUM_GAIN" | "REPLAYGAIN_TRACK_GAIN" => {
                        let re = crate::re!(r#"(?i)^"?(.*)dB"#);
                        if let Some(c) = re.captures(&rv) {
                            add_cmd(
                                &mut album,
                                &mut tracks,
                                in_album,
                                cur,
                                &rc,
                                c[1].trim().to_string(),
                            );
                        }
                    }
                    "COMPILATION" => {
                        let v = rv.trim().to_ascii_lowercase();
                        let comp = if ["1", "yes", "y", "true"].contains(&v.as_str()) {
                            Some("1")
                        } else if ["0", "no", "n", "false"].contains(&v.as_str()) {
                            Some("0")
                        } else {
                            None
                        };
                        if let Some(c) = comp {
                            add_cmd(&mut album, &mut tracks, in_album, cur, &rc, c.to_string());
                        }
                    }
                    _ => add_cmd(
                        &mut album,
                        &mut tracks,
                        in_album,
                        cur,
                        &rc,
                        remove_quotes(&rv),
                    ),
                }
            }
            "FILE" => {
                let name = if let Some(rest) = value.strip_prefix('"') {
                    file_changed = true;
                    rest.split('"').next().unwrap_or("").to_string()
                } else {
                    value
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_matches('"')
                        .to_string()
                };
                filename = match embedded {
                    Some(p) => Some(p.to_path_buf()),
                    None => fix_path(&name, base_dir),
                };
            }
            _ => {
                add_cmd(
                    &mut album,
                    &mut tracks,
                    in_album,
                    cur,
                    &command_uc,
                    remove_quotes(value),
                );
            }
        }
    }

    merge_self("TITLE", "ALBUM", &mut album);
    merge_self("PERFORMER", "ALBUMARTIST", &mut album);
    if !album.contains_key("ARTIST") {
        if let Some(aa) = album.get("ALBUMARTIST").cloned() {
            album.insert("ARTIST".into(), aa);
        }
    }
    merge_self("SONGWRITER", "COMPOSER", &mut album);
    merge_self("DISCNUMBER", "DISC", &mut album);
    merge_self("DISCTOTAL", "DISCC", &mut album);
    merge_self("TOTALDISCS", "DISCC", &mut album);
    merge_self("DATE", "YEAR", &mut album);
    if let Some(y) = album
        .get("YEAR")
        .and_then(|y| crate::text::sanitize_year(y))
    {
        album.insert("YEAR".into(), y);
    }

    for tr in tracks.values_mut() {
        for k in ALBUM_ONLY {
            merge_cmd(k, k, &mut tr.attrs, &mut album);
        }
        if let Some(p) = tr.attrs.shift_remove("PERFORMER") {
            tr.attrs.insert("ARTIST".into(), p.clone());
            tr.attrs.insert("TRACKARTIST".into(), p);
        }
        merge_self("SONGWRITER", "COMPOSER", &mut tr.attrs);
        merge_cmd("DISCTOTAL", "DISCC", &mut tr.attrs, &mut album);
        merge_cmd("TOTALDISCS", "DISCC", &mut tr.attrs, &mut album);
        merge_self("DATE", "YEAR", &mut tr.attrs);
        if let Some(y) = tr
            .attrs
            .get("YEAR")
            .and_then(|y| crate::text::sanitize_year(y))
        {
            tr.attrs.insert("YEAR".into(), y);
        }
        merge_self("DISCNUMBER", "DISC", &mut tr.attrs);
    }

    // drop tracks whose file is not readable (external cues)
    if embedded.is_none() {
        tracks.retain(|_, t| t.file.as_ref().map(|f| f.is_file()).unwrap_or(false));
    }
    if tracks.is_empty() || last_cur.is_none() || filename.is_none() {
        return None;
    }

    // fill album-level values from the referenced audio file
    let mut durations: std::collections::HashMap<PathBuf, Option<f64>> =
        std::collections::HashMap::new();
    if embedded.is_none() {
        if let Some(f) = filename.as_ref() {
            if let Some((secs, attrs)) = file_info(f) {
                durations.insert(f.clone(), secs);
                for fa in [
                    "CONTENT_TYPE",
                    "ALBUMARTIST",
                    "ARTIST",
                    "ALBUM",
                    "YEAR",
                    "GENRE",
                    "DISC",
                    "DISCNUMBER",
                    "DISCC",
                    "DISCTOTAL",
                    "TOTALDISCS",
                    "REPLAYGAIN_ALBUM_GAIN",
                    "REPLAYGAIN_ALBUM_PEAK",
                    "ARTISTSORT",
                    "ALBUMARTISTSORT",
                    "ALBUMSORT",
                    "COMPILATION",
                ] {
                    let target = match fa {
                        "DISCNUMBER" => "DISC",
                        "TOTALDISCS" | "DISCTOTAL" => "DISCC",
                        x => x,
                    };
                    if album.get(target).map(|v| v.is_empty()).unwrap_or(true) {
                        if let Some(v) = attrs.get(fa).and_then(|v| v.first()) {
                            album.insert(target.to_string(), v.clone());
                        }
                    }
                }
            }
        }
    }

    // end times, descending
    let keys: Vec<u32> = tracks.keys().rev().copied().collect();
    let mut lastpos: Option<f64> = None;
    let mut last_file: Option<PathBuf> = None;
    for k in keys {
        let tr = tracks.get_mut(&k).unwrap();
        let file = tr.file.clone();
        if last_file.as_ref() != file.as_ref() {
            // different file than the following track: its end is the file's duration
            lastpos = None;
        }
        if tr.end.is_none() {
            if lastpos.is_none() {
                if let Some(f) = file.as_ref() {
                    let d = match durations.get(f) {
                        Some(d) => *d,
                        None => {
                            let d = file_info(f).and_then(|(s, _)| s);
                            durations.insert(f.clone(), d);
                            d
                        }
                    };
                    lastpos = d;
                }
            }
            tr.end = lastpos;
        }
        lastpos = tr.start;
        last_file = file;
    }

    let mut out = CueSheet {
        album: album.clone(),
        tracks: Vec::new(),
    };
    for (k, tr) in tracks {
        let (Some(start), Some(end), Some(file)) = (tr.start, tr.end, tr.file.clone()) else {
            continue;
        };
        let mut attrs = tr.attrs.clone();
        for (ak, av) in &album {
            if attrs.contains_key(ak) {
                continue;
            }
            if let Some(base) = ak.strip_suffix("SORT") {
                if attrs.get(base) != album.get(base) {
                    continue;
                }
            }
            attrs.insert(ak.clone(), av.clone());
        }
        out.tracks.push(CueTrack {
            tracknum: k,
            file,
            start,
            end,
            attrs,
        });
    }
    if out.tracks.is_empty() {
        return None;
    }
    Some(out)
}

/// Format a cue time value like Perl would stringify the float (`75` frames per second).
pub fn fmt_secs(v: f64) -> String {
    perl_num(v)
}

/// Perl's default number stringification (`%.15g`).
pub fn perl_num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let digits = if v.abs() >= 1.0 {
        (v.abs().log10().floor() as i32) + 1
    } else {
        0
    };
    let decimals = (15 - digits).max(0) as usize;
    let s = format!("{:.*}", decimals, v);
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("album.flac");
        std::fs::write(&f, b"x").unwrap();
        let cue = r#"REM GENRE Rock
REM DATE 1999
PERFORMER "The Band"
TITLE "Live Album"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Intro"
    PERFORMER "Guest"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Song"
    INDEX 00 01:59:00
    INDEX 01 02:00:37
"#;
        let lines: Vec<String> = cue.lines().map(|s| s.to_string()).collect();
        let sheet = parse(&lines, dir.path(), None, |_| {
            Some((Some(300.0), IndexMap::new()))
        })
        .unwrap();
        assert_eq!(sheet.tracks.len(), 2);
        let t1 = &sheet.tracks[0];
        assert_eq!(t1.start, 0.0);
        assert_eq!(t1.end, 120.0 + 37.0 / 75.0);
        assert_eq!(t1.attrs["ARTIST"], "Guest");
        assert_eq!(t1.attrs["ALBUM"], "Live Album");
        assert_eq!(t1.attrs["ALBUMARTIST"], "The Band");
        assert_eq!(t1.attrs["GENRE"], "Rock");
        assert_eq!(t1.attrs["YEAR"], "1999");
        let t2 = &sheet.tracks[1];
        assert_eq!(t2.end, 300.0);
        assert_eq!(t2.attrs["ARTIST"], "The Band");
        assert_eq!(fmt_secs(120.0 + 37.0 / 75.0), "120.493333333333");
    }
}
