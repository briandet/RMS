//! Tag reading (lofty based) with the LMS per-format tag mapping (`Slim::Formats::*`).
//!
//! The output is an ordered attribute map using LMS attribute names (TITLE, ARTIST,
//! ALBUMARTIST, TRACKNUM, DISC, ...) plus the audio properties LMS stores in `tracks`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use indexmap::IndexMap;
use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::{AudioFile, FileType};
use lofty::picture::{Picture, PictureType};

/// Attribute name → values (multi-valued tags keep every value).
pub type Attrs = IndexMap<String, Vec<String>>;

#[derive(Debug, Clone, Default)]
pub struct AudioInfo {
    pub secs: Option<f64>,
    pub bitrate: Option<f64>,
    pub samplerate: Option<u32>,
    pub samplesize: Option<u32>,
    pub channels: Option<u32>,
    pub vbr: Option<bool>,
    pub lossless: bool,
    pub offset: Option<u64>,
    pub size: Option<u64>,
    pub block_align: Option<u32>,
    pub endian: Option<u32>,
    pub tagversion: Option<String>,
    pub drm: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReadTags {
    pub attrs: Attrs,
    pub content_type: String,
    pub info: AudioInfo,
    /// byte length of the embedded picture (LMS `COVER_LENGTH`)
    pub cover_len: Option<usize>,
    /// Embedded cue sheet lines (Vorbis `CUESHEET` comment or FLAC CUESHEET block)
    pub cuesheet: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct TagOptions {
    pub use_tpe2_as_album_artist: bool,
    pub use_tit1_as_work: bool,
}

impl Default for TagOptions {
    fn default() -> Self {
        TagOptions {
            use_tpe2_as_album_artist: true,
            use_tit1_as_work: true,
        }
    }
}

fn push(attrs: &mut Attrs, key: &str, val: impl Into<String>) {
    let v: String = val.into();
    attrs.entry(key.to_string()).or_default().push(v);
}

fn rename(attrs: &mut Attrs, from: &str, to: &str) {
    if from == to {
        return;
    }
    if let Some(v) = attrs.shift_remove(from) {
        attrs.entry(to.to_string()).or_default().extend(v);
    }
}

pub fn first<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a str> {
    attrs.get(key).and_then(|v| v.first()).map(|s| s.as_str())
}

fn pick_picture<'a>(pics: impl Iterator<Item = &'a Picture>) -> Option<&'a Picture> {
    let mut best: Option<&Picture> = None;
    for p in pics {
        if p.data().is_empty() {
            continue;
        }
        if p.pic_type() == PictureType::CoverFront {
            return Some(p);
        }
        match best {
            None => best = Some(p),
            Some(b) if p.pic_type().as_u8() < b.pic_type().as_u8() => best = Some(p),
            _ => {}
        }
    }
    best
}

fn parse_options(read_cover: bool) -> ParseOptions {
    ParseOptions::new()
        .read_cover_art(read_cover)
        .parsing_mode(ParsingMode::Relaxed)
}

// ---------------------------------------------------------------------------------------------
// Vorbis comments (FLAC / Ogg / Opus)
// ---------------------------------------------------------------------------------------------

fn vorbis_map(key: &str, flac: bool) -> Option<&'static str> {
    Some(match key {
        "TRACKNUMBER" => "TRACKNUM",
        "DISCNUMBER" => "DISC",
        "DISCTOTAL" if flac => "DISCC",
        "TOTALDISCS" => "DISCC",
        "URL" => "URLTAG",
        "MUSICBRAINZ_SORTNAME" => "ARTISTSORT",
        "MUSICBRAINZ_ALBUMARTIST" => "ALBUMARTIST",
        "MUSICBRAINZ_ALBUMARTISTID" => "MUSICBRAINZ_ALBUMARTIST_ID",
        "MUSICBRAINZ_ALBUMID" => "MUSICBRAINZ_ALBUM_ID",
        "MUSICBRAINZ_ALBUMSTATUS" => "MUSICBRAINZ_ALBUM_STATUS",
        "MUSICBRAINZ_ALBUMTYPE" | "MUSICBRAINZ_ALBUM_TYPE" => "RELEASETYPE",
        "MUSICBRAINZ_ARTISTID" => "MUSICBRAINZ_ARTIST_ID",
        "MUSICBRAINZ_TRACKID" => "MUSICBRAINZ_ID",
        "MUSICBRAINZ_TRMID" => "MUSICBRAINZ_TRM_ID",
        "DESCRIPTION" => "COMMENT",
        "ORIGINALYEAR" if flac => "YEAR",
        "ORIGINALDATE" if flac => "DATE",
        "UNSYNCEDLYRICS" if flac => "LYRICS",
        "REPLAY GAIN" => "REPLAYGAIN_TRACK_GAIN",
        "PEAK LEVEL" => "REPLAYGAIN_TRACK_PEAK",
        "DISC #" => "DISC",
        "ALBUM ARTIST" => "ALBUMARTIST",
        "SETSUBTITLE" if flac => "DISCSUBTITLE",
        _ => return None,
    })
}

fn vorbis_attrs<'a>(
    items: impl Iterator<Item = (&'a str, &'a str)>,
    flac: bool,
) -> (Attrs, Option<Vec<String>>) {
    let mut raw: Attrs = IndexMap::new();
    let mut cuesheet = None;
    for (k, v) in items {
        let key = k.to_uppercase();
        if key == "CUESHEET" {
            cuesheet = Some(v.lines().map(|l| l.to_string()).collect());
            continue;
        }
        if key == "METADATA_BLOCK_PICTURE" || key == "COVERART" {
            continue;
        }
        push(&mut raw, &key, v);
    }
    let mut attrs: Attrs = IndexMap::new();
    for (k, v) in raw {
        let target = vorbis_map(&k, flac)
            .map(|s| s.to_string())
            .unwrap_or(k.clone());
        // do not overwrite an explicitly present target key
        let entry = attrs.entry(target).or_default();
        entry.extend(v);
    }
    if !flac {
        // Ogg: YEAR ||= first of ORIGINALYEAR, ORIGINALDATE, DATE (sanitized)
        if !attrs.contains_key("YEAR") {
            for k in ["ORIGINALYEAR", "ORIGINALDATE", "DATE"] {
                if let Some(v) = first(&attrs, k) {
                    if let Some(y) = crate::text::sanitize_year(v) {
                        if !y.is_empty() {
                            attrs.insert("YEAR".into(), vec![y]);
                            break;
                        }
                    }
                }
            }
        }
    }
    if let Some(rt) = attrs.get("MUSICBRAINZ_RELEASETRACKID").cloned() {
        attrs.insert("MUSICBRAINZ_ID".into(), rt);
    }
    if !attrs.contains_key("YEAR") {
        if let Some(dates) = attrs.get("DATE") {
            let mut d = dates.clone();
            d.sort();
            if let Some(y) = d.first().and_then(|v| crate::text::sanitize_year(v)) {
                attrs.insert("YEAR".into(), vec![y]);
            }
        }
    }
    if let Some(bpm) = first(&attrs, "BPM") {
        let b = bpm.trim().split('.').next().unwrap_or("").to_string();
        attrs.insert("BPM".into(), vec![b]);
    }
    (attrs, cuesheet)
}

// ---------------------------------------------------------------------------------------------
// ID3v2 (MP3, AIFF, WAV, AAC, DSF, FLAC-with-ID3)
// ---------------------------------------------------------------------------------------------

fn id3_key_map(key: &str, opts: &TagOptions) -> Option<&'static str> {
    Some(match key {
        "MUSICBRAINZ RELEASE TRACK ID" => "MUSICBRAINZ_RELEASETRACKID",
        "MUSICBRAINZ ALBUM ARTIST" => "ALBUMARTIST",
        "MUSICBRAINZ ALBUM ARTIST ID" => "MUSICBRAINZ_ALBUMARTIST_ID",
        "MUSICBRAINZ ALBUM ID" => "MUSICBRAINZ_ALBUM_ID",
        "MUSICBRAINZ ALBUM STATUS" => "MUSICBRAINZ_ALBUM_STATUS",
        "MUSICBRAINZ ALBUM TYPE" => "RELEASETYPE",
        "MUSICBRAINZ ARTIST ID" => "MUSICBRAINZ_ARTIST_ID",
        "MUSICBRAINZ TRM ID" => "MUSICBRAINZ_TRM_ID",
        "MEDIA JUKEBOX: REPLAY GAIN" => "REPLAYGAIN_TRACK_GAIN",
        "MEDIA JUKEBOX: ALBUM GAIN" => "REPLAYGAIN_ALBUM_GAIN",
        "MEDIA JUKEBOX: PEAK LEVEL" => "REPLAYGAIN_TRACK_PEAK",
        "MEDIA JUKEBOX: ALBUM ARTIST" => "ALBUMARTIST",
        "ALBUM ARTIST" => "ALBUMARTIST",
        "COMM" => "COMMENT",
        "TALB" => "ALBUM",
        "TBPM" => "BPM",
        "TCOM" => "COMPOSER",
        "TCMP" | "YTCP" => "COMPILATION",
        "TCON" => "GENRE",
        "TIT1" => {
            if opts.use_tit1_as_work {
                "WORK"
            } else {
                "GROUPING"
            }
        }
        "GRP1" => "GROUPING",
        "TIT2" => "TITLE",
        "TIT3" => "SUBTITLE",
        "TSST" => "DISCSUBTITLE",
        "TPE1" => "ARTIST",
        "TPE2" => {
            if opts.use_tpe2_as_album_artist {
                "ALBUMARTIST"
            } else {
                "BAND"
            }
        }
        "TPE3" => "CONDUCTOR",
        "TPOS" => "SET",
        "TRCK" => "TRACKNUM",
        "TSOA" | "YTSA" => "ALBUMSORT",
        "TSOP" | "YTSP" | "XSOP" => "ARTISTSORT",
        "TSOT" | "YTST" | "TST " => "TITLESORT",
        "TSO2" | "YTS2" => "ALBUMARTISTSORT",
        "TSOC" | "YTSC" => "COMPOSERSORT",
        "USLT" => "LYRICS",
        _ => return None,
    })
}

fn id3_attrs(
    tag: &lofty::id3::v2::Id3v2Tag,
    opts: &TagOptions,
    cover: &mut Option<usize>,
) -> Attrs {
    use lofty::id3::v2::Frame;
    let mut raw: Attrs = IndexMap::new();
    let mut itunnorm: Option<String> = None;
    let mut pics: Vec<&Picture> = Vec::new();
    let mut comments: Vec<String> = Vec::new();
    for frame in tag {
        let id = frame.id_str().to_string();
        match frame {
            Frame::Text(t) => {
                if id == "TCON" {
                    continue; // handled via genres()
                }
                for v in t.value.split('\0') {
                    if !v.is_empty() {
                        push(&mut raw, &id, v);
                    }
                }
            }
            Frame::Timestamp(t) => {
                push(&mut raw, &id, t.timestamp.to_string());
            }
            Frame::UserText(t) => {
                let desc = t.description.to_uppercase();
                if desc == "ITUNNORM" {
                    itunnorm = Some(t.content.clone());
                    continue;
                }
                for v in t.content.split('\0') {
                    if !v.is_empty() {
                        push(&mut raw, &desc, v);
                    }
                }
            }
            Frame::Comment(c) => {
                if c.description == "iTunNORM" {
                    itunnorm = Some(c.content.clone());
                    continue;
                }
                if c.content.is_empty() {
                    continue;
                }
                if c.description.is_empty() {
                    comments.push(c.content.clone());
                } else {
                    comments.push(format!("{}: {}", c.description, c.content));
                }
            }
            Frame::UnsynchronizedText(u) => {
                push(&mut raw, "USLT", u.content.clone());
            }
            Frame::Picture(p) => pics.push(&p.picture),
            Frame::UniqueFileIdentifier(u) if u.owner.contains("musicbrainz") => {
                let s = String::from_utf8_lossy(&u.identifier).to_string();
                if s.len() == 36 {
                    push(&mut raw, "MUSICBRAINZ_ID", s);
                }
            }
            _ => {}
        }
    }
    if let Some(genres) = tag.genres() {
        for g in genres {
            if !g.is_empty() {
                push(&mut raw, "TCON", g);
            }
        }
    }
    if !comments.is_empty() {
        raw.insert("COMM".into(), comments);
    }
    if let Some(p) = pick_picture(pics.into_iter()) {
        *cover = Some(p.data().len());
    }

    let mut attrs: Attrs = IndexMap::new();
    // year: TDOR, XDOR, TDRC, TYER, TDAT – first defined, only if no YEAR
    let mut year: Option<String> = None;
    for k in ["TDOR", "XDOR", "TDRC", "TYER", "TDAT"] {
        if let Some(v) = raw.shift_remove(k) {
            if year.is_none() {
                year = v.into_iter().next();
            }
        }
    }
    for (k, v) in raw {
        let target = id3_key_map(&k, opts).map(|s| s.to_string()).unwrap_or(k);
        attrs.entry(target).or_default().extend(v);
    }
    if !attrs.contains_key("YEAR") {
        if let Some(y) = year.and_then(|y| crate::text::sanitize_year(&y)) {
            attrs.insert("YEAR".into(), vec![y]);
        }
    }
    if let Some(v) = attrs.get("MUSICBRAINZ_RELEASETRACKID").cloned() {
        attrs.insert("MUSICBRAINZ_ID".into(), v);
    }
    if !attrs.contains_key("REPLAYGAIN_TRACK_GAIN") {
        if let Some(g) = itunnorm.and_then(|n| norm_string_to_db(&n)) {
            attrs.insert("REPLAYGAIN_TRACK_GAIN".into(), vec![format!("{:.2}", g)]);
        }
    }
    attrs
}

/// `Slim::Utils::SoundCheck::normStringTodB`
pub fn norm_string_to_db(tag: &str) -> Option<f64> {
    let t = tag
        .trim()
        .trim_start_matches("iTunNORM")
        .trim_start_matches(':')
        .trim();
    if t.is_empty() {
        return None;
    }
    let vals: Vec<f64> = t
        .split_whitespace()
        .filter_map(|h| u64::from_str_radix(h, 16).ok())
        .map(|v| v as f64)
        .collect();
    if vals.len() < 4 {
        return None;
    }
    let max1 = vals[0].max(vals[1]);
    let max2 = vals[2].max(vals[3]);
    let to_db = |v: f64, base: f64| -(v / base).log10() * 10.0;
    let to_norm = |db: f64, base: f64| {
        let r = (10f64.powf(-db / 10.0) * base).round();
        r.min(65534.0)
    };
    let max2 = to_norm(to_db(max2, 2500.0), 1000.0);
    let m = max1.max(max2);
    if m <= 0.0 {
        return None;
    }
    let db = to_db(m, 1000.0);
    // sprintf('%.2f')
    Some((db * 100.0).round() / 100.0)
}

// ---------------------------------------------------------------------------------------------
// MP4
// ---------------------------------------------------------------------------------------------

fn mp4_map(key: &str) -> Option<&'static str> {
    let up = key.to_uppercase();
    Some(match up.as_str() {
        "AART" => "ALBUMARTIST",
        "ALB" => "ALBUM",
        "ART" => "ARTIST",
        "CMT" => "COMMENT",
        "CON" => "CONDUCTOR",
        "CPIL" => "COMPILATION",
        "DAY" => "YEAR",
        "GNRE" | "GEN" => "GENRE",
        "LYR" => "LYRICS",
        "NAM" => "TITLE",
        "SOAA" => "ALBUMARTISTSORT",
        "SOAL" => "ALBUMSORT",
        "SOAR" => "ARTISTSORT",
        "SOCO" => "COMPOSERSORT",
        "SONM" => "TITLESORT",
        "TMPO" => "BPM",
        "TRKN" => "TRACKNUM",
        "WRT" => "COMPOSER",
        "WRK" => "WORK",
        "GRP" => "GROUPING",
        "MUSICBRAINZ ALBUM ID" => "MUSICBRAINZ_ALBUM_ID",
        "MUSICBRAINZ ALBUM TYPE" => "RELEASETYPE",
        "MUSICBRAINZ ARTIST ID" => "MUSICBRAINZ_ARTIST_ID",
        "MUSICBRAINZ ALBUM ARTIST" => "ALBUMARTIST",
        "MUSICBRAINZ ALBUM ARTIST ID" => "MUSICBRAINZ_ALBUMARTIST_ID",
        "MUSICBRAINZ TRACK ID" => "MUSICBRAINZ_ID",
        "MUSICBRAINZ RELEASE TRACK ID" => "MUSICBRAINZ_RELEASETRACKID",
        "MUSICBRAINZ SORTNAME" => "ARTISTSORT",
        "MUSICBRAINZ ALBUM STATUS" => "MUSICBRAINZ_ALBUM_STATUS",
        _ => return None,
    })
}

fn mp4_attrs(ilst: &lofty::mp4::Ilst, cover: &mut Option<usize>) -> Attrs {
    use lofty::mp4::{AtomData, AtomIdent};
    let mut raw: Attrs = IndexMap::new();
    let mut itunnorm = None;
    for atom in ilst {
        let key: String = match atom.ident() {
            AtomIdent::Fourcc(f) => {
                let bytes: Vec<u8> = f.iter().copied().filter(|&b| b != 0xA9).collect();
                String::from_utf8_lossy(&bytes).to_uppercase()
            }
            // Audio::Scan reports freeform (`----:com.apple.iTunes:NAME`) atoms by name
            AtomIdent::Freeform { name, .. } => name.to_uppercase(),
        };
        if key == "COVR" {
            continue;
        }
        for d in atom.data() {
            match d {
                AtomData::UTF8(s) | AtomData::UTF16(s) => {
                    if key.eq_ignore_ascii_case("ITUNNORM") {
                        itunnorm = Some(s.clone());
                    } else {
                        push(&mut raw, &key, s.clone());
                    }
                }
                AtomData::SignedInteger(i) => push(&mut raw, &key, i.to_string()),
                AtomData::UnsignedInteger(i) => push(&mut raw, &key, i.to_string()),
                AtomData::Bool(b) => push(&mut raw, &key, if *b { "1" } else { "0" }),
                AtomData::Unknown { data, .. } => {
                    if key == "TRKN" || key == "DISK" {
                        if data.len() >= 6 {
                            let n = u16::from_be_bytes([data[2], data[3]]);
                            let t = u16::from_be_bytes([data[4], data[5]]);
                            let v = if t > 0 {
                                format!("{}/{}", n, t)
                            } else {
                                n.to_string()
                            };
                            push(&mut raw, &key, v);
                        }
                    } else if key == "TMPO" && data.len() >= 2 {
                        push(
                            &mut raw,
                            &key,
                            u16::from_be_bytes([data[0], data[1]]).to_string(),
                        );
                    } else if let Ok(s) = std::str::from_utf8(data) {
                        if !s.is_empty() && s.chars().all(|c| !c.is_control()) {
                            push(&mut raw, &key, s.to_string());
                        }
                    }
                }
                AtomData::Picture(_) => {}
            }
        }
    }
    if let Some(pics) = ilst.pictures() {
        if let Some(p) = pick_picture(pics) {
            *cover = Some(p.data().len());
        }
    }
    let mut attrs: Attrs = IndexMap::new();
    for (k, v) in raw {
        if k == "DISK" {
            if let Some(first) = v.first() {
                let mut it = first.split('/');
                if let Some(d) = it.next() {
                    push(&mut attrs, "DISC", d.trim());
                }
                if let Some(c) = it.next() {
                    push(&mut attrs, "DISCC", c.trim());
                }
            }
            continue;
        }
        let target = mp4_map(&k).map(|s| s.to_string()).unwrap_or(k);
        attrs.entry(target).or_default().extend(v);
    }
    if let Some(v) = attrs.get("MUSICBRAINZ_RELEASETRACKID").cloned() {
        attrs.insert("MUSICBRAINZ_ID".into(), v);
    }
    if let Some(y) = first(&attrs, "YEAR").and_then(crate::text::sanitize_year) {
        attrs.insert("YEAR".into(), vec![y]);
    }
    if !attrs.contains_key("REPLAYGAIN_TRACK_GAIN") {
        if let Some(g) = itunnorm.and_then(|n| norm_string_to_db(&n)) {
            attrs.insert("REPLAYGAIN_TRACK_GAIN".into(), vec![format!("{:.2}", g)]);
        }
    }
    attrs
}

// ---------------------------------------------------------------------------------------------
// APE tags (APE, Musepack, WavPack)
// ---------------------------------------------------------------------------------------------

fn ape_attrs(tag: &lofty::ape::ApeTag, cover: &mut Option<usize>) -> Attrs {
    use lofty::tag::ItemValue;
    let mut attrs: Attrs = IndexMap::new();
    for item in tag {
        let key = item.key().to_uppercase();
        match item.value() {
            ItemValue::Text(s) | ItemValue::Locator(s) => {
                let target = match key.as_str() {
                    "TRACK" => "TRACKNUM".to_string(),
                    "DATE" => "YEAR".to_string(),
                    "DISCNUMBER" => "DISC".to_string(),
                    "ALBUM ARTIST" => "ALBUMARTIST".to_string(),
                    "MUSICBRAINZ_TRACKID" => "MUSICBRAINZ_ID".to_string(),
                    _ => key.clone(),
                };
                for v in s.split('\0') {
                    if !v.is_empty() {
                        push(&mut attrs, &target, v);
                    }
                }
            }
            ItemValue::Binary(b) => {
                if key.starts_with("COVER ART") {
                    // "filename\0<data>"
                    let len = match b.iter().position(|&c| c == 0) {
                        Some(i) => b.len() - i - 1,
                        None => b.len(),
                    };
                    if key == "COVER ART (FRONT)" || cover.is_none() {
                        *cover = Some(len);
                    }
                }
            }
        }
    }
    if let Some(v) = attrs.get("MUSICBRAINZ_RELEASETRACKID").cloned() {
        attrs.insert("MUSICBRAINZ_ID".into(), v);
    }
    attrs
}

fn merge_missing(into: &mut Attrs, from: Attrs) {
    for (k, v) in from {
        into.entry(k).or_insert(v);
    }
}

// ---------------------------------------------------------------------------------------------
// Low level helpers for offsets
// ---------------------------------------------------------------------------------------------

fn id3v2_size(f: &mut File) -> u64 {
    let mut hdr = [0u8; 10];
    if f.seek(SeekFrom::Start(0)).is_err() || f.read_exact(&mut hdr).is_err() {
        return 0;
    }
    if &hdr[0..3] != b"ID3" {
        return 0;
    }
    let size = ((hdr[6] as u64 & 0x7f) << 21)
        | ((hdr[7] as u64 & 0x7f) << 14)
        | ((hdr[8] as u64 & 0x7f) << 7)
        | (hdr[9] as u64 & 0x7f);
    let footer = if hdr[5] & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// MP3: (audio_offset, audio_size, vbr)
fn mp3_layout(path: &Path, file_size: u64) -> (u64, u64, bool) {
    let Ok(mut f) = File::open(path) else {
        return (0, file_size, false);
    };
    let start = id3v2_size(&mut f);
    let mut buf = vec![0u8; 64 * 1024];
    let _ = f.seek(SeekFrom::Start(start));
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    let mut off = start;
    let mut frame_pos = None;
    for i in 0..buf.len().saturating_sub(4) {
        if buf[i] == 0xFF
            && (buf[i + 1] & 0xE0) == 0xE0
            && (buf[i + 2] & 0xF0) != 0xF0
            && (buf[i + 1] & 0x06) != 0
        {
            frame_pos = Some(i);
            break;
        }
    }
    let mut vbr = false;
    if let Some(p) = frame_pos {
        off = start + p as u64;
        let end = (p + 200).min(buf.len());
        let win = &buf[p..end];
        if win.windows(4).any(|w| w == b"Xing" || w == b"VBRI") {
            vbr = true;
        }
    }
    // trailing tags
    let mut end = file_size;
    let mut tail = [0u8; 128];
    if file_size >= 128
        && f.seek(SeekFrom::Start(file_size - 128)).is_ok()
        && f.read_exact(&mut tail).is_ok()
        && &tail[0..3] == b"TAG"
    {
        end -= 128;
    }
    if end >= 32 {
        let mut ape = [0u8; 32];
        if f.seek(SeekFrom::Start(end - 32)).is_ok()
            && f.read_exact(&mut ape).is_ok()
            && &ape[0..8] == b"APETAGEX"
        {
            let size = u32::from_le_bytes([ape[12], ape[13], ape[14], ape[15]]) as u64;
            let flags = u32::from_le_bytes([ape[20], ape[21], ape[22], ape[23]]);
            let total = size + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
            if total <= end {
                end -= total;
            }
        }
    }
    (off, end.saturating_sub(off), vbr)
}

/// RIFF/WAVE: (data offset, data size, block align)
fn wav_layout(path: &Path) -> Option<(u64, u64, u32)> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12u64;
    let mut block_align = 0u32;
    loop {
        let mut ch = [0u8; 8];
        f.seek(SeekFrom::Start(pos)).ok()?;
        f.read_exact(&mut ch).ok()?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"fmt " {
            let mut fmt = [0u8; 16];
            f.read_exact(&mut fmt).ok()?;
            block_align = u16::from_le_bytes([fmt[12], fmt[13]]) as u32;
        } else if &ch[0..4] == b"data" {
            return Some((pos + 8, size, block_align));
        }
        pos += 8 + size + (size & 1);
    }
}

/// AIFF: (SSND data offset, size, block align, endian)
fn aiff_layout(path: &Path) -> Option<(u64, u64, u32, u32)> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"FORM" {
        return None;
    }
    let aifc = &hdr[8..12] == b"AIFC";
    let mut pos = 12u64;
    let mut endian = 1u32;
    let mut channels = 0u32;
    let mut bits = 0u32;
    loop {
        let mut ch = [0u8; 8];
        f.seek(SeekFrom::Start(pos)).ok()?;
        f.read_exact(&mut ch).ok()?;
        let size = u32::from_be_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"COMM" {
            let mut c = vec![0u8; size.min(64) as usize];
            f.read_exact(&mut c).ok()?;
            channels = u16::from_be_bytes([c[0], c[1]]) as u32;
            bits = u16::from_be_bytes([c[6], c[7]]) as u32;
            if aifc && c.len() >= 22 && &c[18..22] == b"sowt" {
                endian = 0;
            }
        } else if &ch[0..4] == b"SSND" {
            let mut s = [0u8; 8];
            f.read_exact(&mut s).ok()?;
            let off = u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64;
            let ba = channels * bits.div_ceil(8);
            return Some((pos + 16 + off, size.saturating_sub(8 + off), ba, endian));
        }
        pos += 8 + size + (size & 1);
    }
}

/// Raw FLAC metadata: CUESHEET block converted to cue lines (embedded mode).
fn flac_cuesheet_block(path: &Path, samplerate: u32) -> Option<Vec<String>> {
    let mut f = File::open(path).ok()?;
    let start = id3v2_size(&mut f);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" || samplerate == 0 {
        return None;
    }
    loop {
        let mut h = [0u8; 4];
        f.read_exact(&mut h).ok()?;
        let last = h[0] & 0x80 != 0;
        let btype = h[0] & 0x7f;
        let len = ((h[1] as usize) << 16) | ((h[2] as usize) << 8) | h[3] as usize;
        if btype == 5 {
            let mut b = vec![0u8; len];
            f.read_exact(&mut b).ok()?;
            let mut lines = vec!["FILE \"embedded\" FLAC".to_string()];
            let ntracks = *b.get(395)? as usize;
            let mut p = 396;
            for _ in 0..ntracks {
                if p + 36 > b.len() {
                    break;
                }
                let offset = u64::from_be_bytes(b[p..p + 8].try_into().ok()?);
                let num = b[p + 8];
                let nidx = b[p + 35] as usize;
                p += 36;
                let audio = num != 170 && num != 255;
                if audio {
                    lines.push(format!("  TRACK {:02} AUDIO", num));
                }
                for _ in 0..nidx {
                    if p + 12 > b.len() {
                        break;
                    }
                    let ioff = u64::from_be_bytes(b[p..p + 8].try_into().ok()?);
                    let inum = b[p + 8];
                    p += 12;
                    if audio {
                        let samples = offset + ioff;
                        let total_frames =
                            (samples as f64 * 75.0 / samplerate as f64).round() as u64;
                        let mm = total_frames / (75 * 60);
                        let ss = (total_frames / 75) % 60;
                        let ff = total_frames % 75;
                        lines.push(format!(
                            "    INDEX {:02} {:02}:{:02}:{:02}",
                            inum, mm, ss, ff
                        ));
                    }
                }
            }
            return Some(lines);
        } else {
            f.seek(SeekFrom::Current(len as i64)).ok()?;
        }
        if last {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// DSF (DSD stream file)
// ---------------------------------------------------------------------------------------------

fn read_dsf(path: &Path, opts: &TagOptions) -> Option<(Attrs, AudioInfo, Option<usize>)> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 28];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"DSD " {
        return None;
    }
    let meta_off = u64::from_le_bytes(hdr[20..28].try_into().ok()?);
    let mut fmt = [0u8; 52];
    f.read_exact(&mut fmt).ok()?;
    if &fmt[0..4] != b"fmt " {
        return None;
    }
    let channels = u32::from_le_bytes(fmt[24..28].try_into().ok()?);
    let rate = u32::from_le_bytes(fmt[28..32].try_into().ok()?);
    let samples = u64::from_le_bytes(fmt[36..44].try_into().ok()?);
    let mut data = [0u8; 12];
    f.read_exact(&mut data).ok()?;
    let data_size = u64::from_le_bytes(data[4..12].try_into().ok()?).saturating_sub(12);
    let audio_offset = 28 + 52 + 12;
    let mut info = AudioInfo {
        secs: if rate > 0 {
            Some(samples as f64 / rate as f64)
        } else {
            None
        },
        samplerate: Some(rate),
        samplesize: Some(1),
        channels: Some(channels),
        lossless: true,
        offset: Some(audio_offset),
        size: Some(audio_offset + data_size),
        ..Default::default()
    };
    if let Some(s) = info.secs {
        if s > 0.0 {
            info.bitrate = Some((rate as f64) * channels as f64);
        }
    }
    let mut attrs = IndexMap::new();
    let mut cover = None;
    if meta_off > 0 {
        let mut tagbytes = Vec::new();
        f.seek(SeekFrom::Start(meta_off)).ok()?;
        f.read_to_end(&mut tagbytes).ok()?;
        if tagbytes.starts_with(b"ID3") {
            if let Some(tag) = id3_from_bytes(&tagbytes) {
                attrs = id3_attrs(&tag, opts, &mut cover);
                info.tagversion = Some(format!("ID3v2.{}.0", tagbytes[3]));
            }
        }
    }
    Some((attrs, info, cover))
}

/// Parse a standalone ID3v2 tag by wrapping it into a minimal WAV container that lofty can read.
fn id3_from_bytes(tag: &[u8]) -> Option<lofty::id3::v2::Id3v2Tag> {
    let mut wav: Vec<u8> = Vec::new();
    let fmt: [u8; 16] = {
        let mut b = [0u8; 16];
        b[0..2].copy_from_slice(&1u16.to_le_bytes());
        b[2..4].copy_from_slice(&2u16.to_le_bytes());
        b[4..8].copy_from_slice(&44100u32.to_le_bytes());
        b[8..12].copy_from_slice(&(44100u32 * 4).to_le_bytes());
        b[12..14].copy_from_slice(&4u16.to_le_bytes());
        b[14..16].copy_from_slice(&16u16.to_le_bytes());
        b
    };
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&4u32.to_le_bytes());
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend_from_slice(b"ID3 ");
    body.extend_from_slice(&(tag.len() as u32).to_le_bytes());
    body.extend_from_slice(tag);
    if tag.len() % 2 == 1 {
        body.push(0);
    }
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(body.len() as u32).to_le_bytes());
    wav.extend_from_slice(&body);
    let mut cur = std::io::Cursor::new(wav);
    let wf = lofty::iff::wav::WavFile::read_from(&mut cur, parse_options(true)).ok()?;
    wf.id3v2().cloned()
}

fn dsf_cover(path: &Path) -> Option<(Vec<u8>, String)> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 28];
    f.read_exact(&mut hdr).ok()?;
    let meta_off = u64::from_le_bytes(hdr[20..28].try_into().ok()?);
    if meta_off == 0 {
        return None;
    }
    let mut tagbytes = Vec::new();
    f.seek(SeekFrom::Start(meta_off)).ok()?;
    f.read_to_end(&mut tagbytes).ok()?;
    let tag = id3_from_bytes(&tagbytes)?;
    let pics: Vec<&Picture> = (&tag)
        .into_iter()
        .filter_map(|fr| match fr {
            lofty::id3::v2::Frame::Picture(p) => Some(&p.picture),
            _ => None,
        })
        .collect();
    let p = pick_picture(pics.into_iter())?;
    Some((p.data().to_vec(), sniff_image_type(p.data()).to_string()))
}

// ---------------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------------

fn file_type_for(path: &Path) -> Option<FileType> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp3" | "mp2" => FileType::Mpeg,
        "flac" | "flc" | "fla" => FileType::Flac,
        "m4a" | "mp4" | "m4b" => FileType::Mp4,
        "aac" => FileType::Aac,
        "ogg" | "oga" => FileType::Vorbis,
        "opus" => FileType::Opus,
        "wav" | "wave" => FileType::Wav,
        "aif" | "aiff" => FileType::Aiff,
        "ape" => FileType::Ape,
        "wv" => FileType::WavPack,
        "mpc" | "mp+" => FileType::Mpc,
        _ => return None,
    })
}

fn dur_secs(d: std::time::Duration) -> Option<f64> {
    let s = d.as_secs_f64();
    if s > 0.0 {
        Some(s)
    } else {
        None
    }
}

fn kbps(v: u32) -> Option<f64> {
    if v > 0 {
        Some(v as f64 * 1000.0)
    } else {
        None
    }
}

/// Read tags + audio info of a file. Errors are returned (never panics); unknown formats
/// yield a minimal result with `TITLE` guessed from the file name by the caller.
pub fn read_tags(path: &Path, opts: &TagOptions) -> anyhow::Result<ReadTags> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let ct = crate::types::type_for_suffix(&ext)
        .unwrap_or("unk")
        .to_string();
    let file_size = std::fs::metadata(path)?.len();
    let mut out = ReadTags {
        content_type: ct.clone(),
        ..Default::default()
    };

    if ext == "dsf" {
        let (attrs, info, cover) =
            read_dsf(path, opts).ok_or_else(|| anyhow::anyhow!("invalid DSF file"))?;
        out.attrs = attrs;
        out.info = info;
        out.cover_len = cover;
        return Ok(out);
    }
    if ext == "dff" {
        out.info = read_dff_info(path).unwrap_or_default();
        out.info.lossless = true;
        out.info.samplesize = Some(1);
        return Ok(out);
    }

    let Some(ft) = file_type_for(path) else {
        // formats lofty cannot parse (wma etc.): no tags
        return Ok(out);
    };
    let mut f = File::open(path)?;
    let po = parse_options(true);
    let mut cover: Option<usize> = None;
    match ft {
        FileType::Flac => {
            let ff = lofty::flac::FlacFile::read_from(&mut f, po)?;
            let p = ff.properties();
            let mut cue = None;
            if let Some(vc) = ff.vorbis_comments() {
                let (a, c) = vorbis_attrs(vc.items(), true);
                out.attrs = a;
                cue = c;
            }
            if let Some(id3) = ff.id3v2() {
                let a = id3_attrs(id3, opts, &mut cover);
                merge_missing(&mut out.attrs, a);
                out.info.tagversion = Some(format!("FLAC, ID3v2.{}.0", id3_version(path)));
            }
            use lofty::ogg::OggPictureStorage;
            let pics = ff.pictures();
            if let Some(pic) = pick_picture(pics.iter().map(|(p, _)| p)) {
                cover = Some(pic.data().len());
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate()).or(kbps(p.overall_bitrate()));
            out.info.samplerate = Some(p.sample_rate());
            out.info.samplesize = Some(p.bit_depth() as u32);
            out.info.channels = Some(p.channels() as u32);
            out.info.vbr = Some(true);
            out.info.lossless = true;
            out.info.offset = Some(0);
            out.info.size = Some(file_size);
            if cue.is_none() {
                cue = flac_cuesheet_block(path, p.sample_rate());
            }
            out.cuesheet = cue;
        }
        FileType::Vorbis => {
            let vf = lofty::ogg::VorbisFile::read_from(&mut f, po)?;
            let p = vf.properties();
            let (a, c) = vorbis_attrs(vf.vorbis_comments().items(), false);
            out.attrs = a;
            out.cuesheet = c;
            use lofty::ogg::OggPictureStorage;
            if let Some(pic) = pick_picture(vf.vorbis_comments().pictures().iter().map(|(p, _)| p))
            {
                cover = Some(pic.data().len());
            }
            out.info.secs = dur_secs(p.duration());
            let avg = p.audio_bitrate();
            out.info.bitrate = kbps(avg).or_else(|| {
                if p.bitrate_nominal() > 0 {
                    Some(p.bitrate_nominal() as f64)
                } else {
                    None
                }
            });
            out.info.samplerate = Some(p.sample_rate());
            out.info.channels = Some(p.channels() as u32);
            out.info.vbr = Some(p.bitrate_max() != p.bitrate_min());
            out.info.offset = Some(0);
            out.info.size = Some(file_size);
        }
        FileType::Opus => {
            let of = lofty::ogg::OpusFile::read_from(&mut f, po)?;
            let p = of.properties();
            let (a, c) = vorbis_attrs(of.vorbis_comments().items(), false);
            out.attrs = a;
            out.cuesheet = c;
            use lofty::ogg::OggPictureStorage;
            if let Some(pic) = pick_picture(of.vorbis_comments().pictures().iter().map(|(p, _)| p))
            {
                cover = Some(pic.data().len());
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate());
            out.info.samplerate = Some(48000);
            out.info.channels = Some(p.channels() as u32);
            out.info.offset = Some(0);
            out.info.size = Some(file_size);
        }
        FileType::Mpeg => {
            let mf = lofty::mpeg::MpegFile::read_from(&mut f, po)?;
            let p = mf.properties();
            if let Some(id3) = mf.id3v2() {
                out.attrs = id3_attrs(id3, opts, &mut cover);
                out.info.tagversion = Some(format!("ID3v2.{}.0", id3_version(path)));
            } else if let Some(v1) = mf.id3v1() {
                use lofty::tag::Accessor;
                if let Some(t) = v1.title() {
                    push(&mut out.attrs, "TITLE", t.to_string());
                }
                if let Some(t) = v1.artist() {
                    push(&mut out.attrs, "ARTIST", t.to_string());
                }
                if let Some(t) = v1.album() {
                    push(&mut out.attrs, "ALBUM", t.to_string());
                }
                if let Some(t) = v1.year() {
                    push(&mut out.attrs, "YEAR", t.to_string());
                }
                if let Some(t) = v1.track() {
                    push(&mut out.attrs, "TRACKNUM", t.to_string());
                }
                if let Some(t) = v1.genre() {
                    push(&mut out.attrs, "GENRE", t.to_string());
                }
                if let Some(t) = v1.comment() {
                    push(&mut out.attrs, "COMMENT", t.to_string());
                }
                out.info.tagversion = Some("ID3v1".into());
            }
            if p.duration().is_zero() {
                anyhow::bail!("Unable to read tags from file (no audio frames)");
            }
            let (off, size, vbr) = mp3_layout(path, file_size);
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate());
            out.info.samplerate = Some(p.sample_rate());
            out.info.channels = Some(if p.channels() >= 2 { 2 } else { 1 });
            out.info.offset = Some(off);
            out.info.size = Some(size);
            out.info.block_align = Some(1);
            if vbr {
                out.info.vbr = Some(true);
            }
        }
        FileType::Mp4 => {
            let mf = lofty::mp4::Mp4File::read_from(&mut f, po)?;
            let p = mf.properties();
            if p.is_drm_protected() {
                out.info.drm = true;
            }
            if let Some(ilst) = mf.ilst() {
                out.attrs = mp4_attrs(ilst, &mut cover);
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate());
            out.info.samplerate = Some(p.sample_rate());
            out.info.samplesize = p.bit_depth().map(|b| b as u32);
            out.info.channels = Some(p.channels() as u32);
            out.info.offset = Some(0);
            out.info.size = Some(file_size);
            if matches!(p.codec(), lofty::mp4::Mp4Codec::ALAC) {
                out.info.lossless = true;
                out.info.vbr = Some(true);
                out.content_type = "alc".into();
            } else if matches!(p.codec(), lofty::mp4::Mp4Codec::FLAC) {
                out.info.lossless = true;
            }
        }
        FileType::Aac => {
            let af = lofty::aac::AacFile::read_from(&mut f, po)?;
            let p = af.properties();
            if let Some(id3) = af.id3v2() {
                out.attrs = id3_attrs(id3, opts, &mut cover);
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate());
            out.info.samplerate = Some(p.sample_rate());
            out.info.channels = Some(p.channels() as u32);
            let mut g = File::open(path)?;
            out.info.offset = Some(id3v2_size(&mut g));
            out.info.size = Some(file_size - out.info.offset.unwrap_or(0));
        }
        FileType::Wav => {
            let wf = lofty::iff::wav::WavFile::read_from(&mut f, po)?;
            let p = wf.properties();
            if let Some(info) = wf.riff_info() {
                for (k, v) in info {
                    let key = k.to_string();
                    let target = match key.as_str() {
                        "IART" => "ARTIST",
                        "ICMT" => "COMMENT",
                        "ICRD" => "YEAR",
                        "IGNR" => "GENRE",
                        "INAM" => "TITLE",
                        "IPRD" => "ALBUM",
                        "TRCK" | "ITRK" => "TRACKNUM",
                        _ => continue,
                    };
                    push(&mut out.attrs, target, v.to_string());
                }
            }
            if let Some(id3) = wf.id3v2() {
                let a = id3_attrs(id3, opts, &mut cover);
                for (k, v) in a {
                    out.attrs.insert(k, v);
                }
                out.info.tagversion = Some("ID3v2.4.0".into());
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.bitrate()).or(kbps(p.overall_bitrate()));
            out.info.samplerate = Some(p.sample_rate());
            out.info.samplesize = Some(p.bit_depth() as u32);
            out.info.channels = Some(p.channels() as u32);
            out.info.endian = Some(0);
            if let Some((off, size, ba)) = wav_layout(path) {
                out.info.offset = Some(off);
                out.info.size = Some(size);
                out.info.block_align = Some(ba);
            }
        }
        FileType::Aiff => {
            let af = lofty::iff::aiff::AiffFile::read_from(&mut f, po)?;
            let p = af.properties();
            if let Some(id3) = af.id3v2() {
                out.attrs = id3_attrs(id3, opts, &mut cover);
                out.info.tagversion = Some("ID3v2.4.0".into());
            }
            if let Some(tc) = af.text_chunks() {
                if let Some(n) = &tc.name {
                    out.attrs
                        .entry("TITLE".into())
                        .or_insert_with(|| vec![n.clone()]);
                }
                if let Some(a) = &tc.author {
                    out.attrs
                        .entry("ARTIST".into())
                        .or_insert_with(|| vec![a.clone()]);
                }
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate()).or(kbps(p.overall_bitrate()));
            out.info.samplerate = Some(p.sample_rate());
            out.info.samplesize = Some(p.sample_size() as u32);
            out.info.channels = Some(p.channels() as u32);
            out.info.endian = Some(1);
            if let Some((off, size, ba, endian)) = aiff_layout(path) {
                out.info.offset = Some(off);
                out.info.size = Some(size);
                out.info.block_align = Some(ba);
                out.info.endian = Some(endian);
            }
        }
        FileType::Ape => {
            let af = lofty::ape::ApeFile::read_from(&mut f, po)?;
            let p = af.properties();
            if let Some(t) = af.ape() {
                out.attrs = ape_attrs(t, &mut cover);
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.bitrate()).or(kbps(p.overall_bitrate()));
            out.info.samplerate = Some(p.sample_rate());
            out.info.channels = Some(p.channels() as u32);
            out.info.size = Some(file_size);
        }
        FileType::WavPack => {
            let wf = lofty::wavpack::WavPackFile::read_from(&mut f, po)?;
            let p = wf.properties();
            if let Some(t) = wf.ape() {
                out.attrs = ape_attrs(t, &mut cover);
            }
            out.info.secs = dur_secs(p.duration());
            out.info.bitrate = kbps(p.audio_bitrate()).or(kbps(p.overall_bitrate()));
            out.info.samplerate = Some(p.sample_rate());
            out.info.samplesize = Some(p.bit_depth() as u32);
            out.info.channels = Some(p.channels() as u32);
            out.info.vbr = Some(true);
            out.info.size = Some(file_size);
            if p.bit_depth() == 1 {
                out.content_type = "wvpx".into();
            }
        }
        FileType::Mpc => {
            let mf = lofty::musepack::MpcFile::read_from(&mut f, po)?;
            use lofty::file::AudioFile as _;
            let p = mf.properties();
            if let Some(t) = mf.ape() {
                out.attrs = ape_attrs(t, &mut cover);
            }
            let fp: lofty::properties::FileProperties = match p {
                lofty::musepack::MpcProperties::Sv8(s) => s.clone().into(),
                lofty::musepack::MpcProperties::Sv7(s) => s.clone().into(),
                lofty::musepack::MpcProperties::Sv4to6(s) => s.clone().into(),
            };
            out.info.secs = dur_secs(fp.duration());
            out.info.bitrate = fp.audio_bitrate().and_then(kbps);
            out.info.samplerate = fp.sample_rate();
            out.info.channels = fp.channels().map(|c| c as u32);
            out.info.size = Some(file_size);
        }
        _ => {}
    }
    out.cover_len = cover;
    Ok(out)
}

fn id3_version(path: &Path) -> u8 {
    let mut hdr = [0u8; 4];
    if let Ok(mut f) = File::open(path) {
        let _ = f.read_exact(&mut hdr);
    }
    if &hdr[0..3] == b"ID3" {
        hdr[3]
    } else {
        // FLAC with id3 prefix / unknown
        4
    }
}

fn read_dff_info(path: &Path) -> Option<AudioInfo> {
    let mut f = File::open(path).ok()?;
    let mut hdr = [0u8; 16];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[0..4] != b"FRM8" || &hdr[12..16] != b"DSD " {
        return None;
    }
    let mut info = AudioInfo::default();
    let mut pos = 16u64;
    let total = std::fs::metadata(path).ok()?.len();
    while pos + 12 <= total {
        let mut ch = [0u8; 12];
        f.seek(SeekFrom::Start(pos)).ok()?;
        f.read_exact(&mut ch).ok()?;
        let size = u64::from_be_bytes(ch[4..12].try_into().ok()?);
        let id = &ch[0..4];
        if id == b"PROP" {
            let mut p = vec![0u8; size.min(4096) as usize];
            f.read_exact(&mut p).ok()?;
            let mut q = 4usize;
            while q + 12 <= p.len() {
                let sid = &p[q..q + 4];
                let ssz = u64::from_be_bytes(p[q + 4..q + 12].try_into().ok()?) as usize;
                let body = &p[(q + 12).min(p.len())..(q + 12 + ssz).min(p.len())];
                if sid == b"FS  " && body.len() >= 4 {
                    info.samplerate = Some(u32::from_be_bytes(body[0..4].try_into().ok()?));
                } else if sid == b"CHNL" && body.len() >= 2 {
                    info.channels = Some(u16::from_be_bytes(body[0..2].try_into().ok()?) as u32);
                }
                q += 12 + ssz + (ssz & 1);
            }
        } else if id == b"DSD " {
            info.offset = Some(pos + 12);
            info.size = Some(total);
            if let (Some(r), Some(c)) = (info.samplerate, info.channels) {
                if r > 0 && c > 0 {
                    info.secs = Some(size as f64 * 8.0 / (r as f64 * c as f64));
                    info.bitrate = Some(r as f64 * c as f64);
                }
            }
        }
        pos += 12 + size + (size & 1);
    }
    Some(info)
}

/// Read the embedded cover image (bytes, mime type) from an audio file.
pub fn read_embedded_cover(path: &Path) -> Option<(Vec<u8>, String)> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext == "dsf" {
        return dsf_cover(path);
    }
    let ft = file_type_for(path)?;
    let mut f = File::open(path).ok()?;
    let po = parse_options(true);
    let pic: Option<Picture> = match ft {
        FileType::Flac => {
            let ff = lofty::flac::FlacFile::read_from(&mut f, po).ok()?;
            use lofty::ogg::OggPictureStorage;
            let p = pick_picture(ff.pictures().iter().map(|(p, _)| p)).cloned();
            p.or_else(|| ff.id3v2().and_then(id3_pic))
        }
        FileType::Vorbis => {
            let vf = lofty::ogg::VorbisFile::read_from(&mut f, po).ok()?;
            use lofty::ogg::OggPictureStorage;
            pick_picture(vf.vorbis_comments().pictures().iter().map(|(p, _)| p)).cloned()
        }
        FileType::Opus => {
            let vf = lofty::ogg::OpusFile::read_from(&mut f, po).ok()?;
            use lofty::ogg::OggPictureStorage;
            pick_picture(vf.vorbis_comments().pictures().iter().map(|(p, _)| p)).cloned()
        }
        FileType::Mpeg => {
            let mf = lofty::mpeg::MpegFile::read_from(&mut f, po).ok()?;
            mf.id3v2().and_then(id3_pic)
        }
        FileType::Mp4 => {
            let mf = lofty::mp4::Mp4File::read_from(&mut f, po).ok()?;
            mf.ilst()
                .and_then(|i| i.pictures().and_then(|p| pick_picture(p).cloned()))
        }
        FileType::Aac => {
            let af = lofty::aac::AacFile::read_from(&mut f, po).ok()?;
            af.id3v2().and_then(id3_pic)
        }
        FileType::Wav => {
            let wf = lofty::iff::wav::WavFile::read_from(&mut f, po).ok()?;
            wf.id3v2().and_then(id3_pic)
        }
        FileType::Aiff => {
            let af = lofty::iff::aiff::AiffFile::read_from(&mut f, po).ok()?;
            af.id3v2().and_then(id3_pic)
        }
        FileType::Ape | FileType::WavPack | FileType::Mpc => {
            use lofty::prelude::TaggedFileExt;
            drop(f);
            let tf = lofty::read_from_path(path).ok()?;
            let t = tf.primary_tag().or_else(|| tf.first_tag())?;
            pick_picture(t.pictures().iter()).cloned()
        }
        _ => None,
    };
    let pic = pic?;
    let data = pic.into_data();
    if data.is_empty() {
        return None;
    }
    let mime = sniff_image_type(&data).to_string();
    Some((data, mime))
}

fn id3_pic(tag: &lofty::id3::v2::Id3v2Tag) -> Option<Picture> {
    let pics: Vec<&Picture> = tag
        .into_iter()
        .filter_map(|fr| match fr {
            lofty::id3::v2::Frame::Picture(p) => Some(&p.picture),
            _ => None,
        })
        .collect();
    pick_picture(pics.into_iter()).cloned()
}

pub fn sniff_image_type(data: &[u8]) -> &'static str {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.starts_with(b"BM") {
        "image/bmp"
    } else if data.len() > 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// `Slim::Formats->readTags` common post-processing. `url` is used for guessTags/plainTitle.
pub fn post_process(rt: &mut ReadTags, path: &Path, url: &str, guess_formats: &[String]) {
    let attrs = &mut rt.attrs;
    // URLTAG (FLAC URL) is appended as comment later
    // SET → DISC/DISCC
    if let Some(set) = first(attrs, "SET").map(|s| s.to_string()) {
        let re = crate::re!(r"(\d+)(?:/(\d+))?");
        if let Some(c) = re.captures(&set) {
            attrs.insert(
                "DISC".into(),
                vec![c[1].parse::<i64>().unwrap_or(0).to_string()],
            );
            if let Some(dc) = c.get(2) {
                attrs.insert(
                    "DISCC".into(),
                    vec![dc.as_str().parse::<i64>().unwrap_or(0).to_string()],
                );
            }
        }
        attrs.shift_remove("SET");
    }
    // sanitize: strip BOMs / trailing NULs
    for vals in attrs.values_mut() {
        for v in vals.iter_mut() {
            let t = v
                .trim_start_matches('\u{feff}')
                .trim_end_matches('\0')
                .to_string();
            *v = t;
        }
    }
    // MusicBrainz ids validation
    let keys: Vec<String> = attrs
        .keys()
        .filter(|k| k.starts_with("MUSICBRAINZ") && k.ends_with("ID"))
        .cloned()
        .collect();
    let uuid = crate::re!(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$");
    for k in keys {
        let vals = attrs.get(&k).cloned().unwrap_or_default();
        let mut out = Vec::new();
        let mut ok = true;
        for v in &vals {
            for part in crate::text::split_tag_with(v, &[";".to_string(), "/".to_string()]) {
                let p = part.to_lowercase();
                if k == "MUSICBRAINZ_DISCID" || uuid.is_match(&p) {
                    out.push(p);
                } else {
                    ok = false;
                }
            }
        }
        if ok && !out.is_empty() {
            attrs.insert(k, out);
        } else {
            attrs.shift_remove(&k);
        }
    }
    // no TITLE → guessTags
    let has_title = first(attrs, "TITLE")
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    if !has_title {
        guess_tags(attrs, path, guess_formats);
    }
    if first(attrs, "TITLE").map(|t| t.is_empty()).unwrap_or(true) {
        attrs.insert("TITLE".into(), vec![crate::url::plain_title(url, false)]);
    }
    // DISC "1/2" handled in pre-check; YEAR sanitize
    if let Some(y) = first(attrs, "YEAR").map(|s| s.to_string()) {
        if let Some(s) = crate::text::sanitize_year(&y) {
            attrs.insert("YEAR".into(), vec![s]);
        }
    }
}

/// `Slim::Music::Info::guessTags`
pub fn guess_tags(attrs: &mut Attrs, path: &Path, formats: &[String]) {
    let mut file = path.to_string_lossy().replace('\\', "/");
    if let Some(i) = file.rfind('.') {
        if !file[i..].contains('/') {
            file.truncate(i);
        }
    }
    let elem = crate::re!(
        r"\b(TRACKNUM|DISCC|DISC|ARTIST|ALBUMARTIST|ALBUM|TITLE|YEAR|GENRE|COMPOSER|CONDUCTOR|BAND)\b"
    );
    for guess in formats {
        let mut pat = String::new();
        let mut names = Vec::new();
        let mut last = 0;
        for m in elem.find_iter(guess) {
            pat.push_str(&regex::escape(&guess[last..m.start()]));
            let name = m.as_str();
            if matches!(name, "TRACKNUM" | "DISC" | "DISCC") {
                pat.push_str(r"(\d+)");
            } else {
                pat.push_str(r"([^\\/]+)");
            }
            names.push(name.to_string());
            last = m.end();
        }
        pat.push_str(&regex::escape(&guess[last..]));
        pat.push('$');
        let Ok(re) = regex::Regex::new(&pat) else {
            continue;
        };
        if let Some(c) = re.captures(&file) {
            for (i, name) in names.iter().enumerate() {
                if let Some(v) = c.get(i + 1) {
                    let mut val = v.as_str().replace('_', " ");
                    if matches!(name.as_str(), "TRACKNUM" | "DISC" | "DISCC") {
                        val = val.parse::<i64>().map(|n| n.to_string()).unwrap_or(val);
                    }
                    attrs.insert(name.clone(), vec![val]);
                }
            }
            return;
        }
    }
}

pub fn default_guess_formats() -> Vec<String> {
    vec![
        "(ARTIST - ALBUM) TRACKNUM - TITLE".into(),
        "/ARTIST/ALBUM/TRACKNUM - TITLE".into(),
        "/ARTIST/ALBUM/TRACKNUM TITLE".into(),
        "/ARTIST/ALBUM/TRACKNUM. TITLE".into(),
    ]
}

/// Read only the attrs relevant for album-level fill of external cue sheets.
pub fn read_for_cue(path: &Path, opts: &TagOptions) -> Option<ReadTags> {
    read_tags(path, opts).ok()
}

/// Helper used by tests and the cue code: attrs → rename map
pub fn rename_attr(attrs: &mut Attrs, from: &str, to: &str) {
    rename(attrs, from, to)
}
