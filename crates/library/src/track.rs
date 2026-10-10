//! The `Track` value type (one row of `tracks` plus the most used relations).

use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Track {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub titlesort: Option<String>,
    pub album_id: Option<i64>,
    pub album: Option<String>,
    /// primary (track) artist name (`tracks.primary_artist`)
    pub artist: Option<String>,
    pub artist_id: Option<i64>,
    /// album artist (`albums.contributor`)
    pub albumartist: Option<String>,
    pub albumartist_id: Option<i64>,
    /// all COMPOSER contributors joined with ", "
    pub composer: Option<String>,
    /// first genre (by genre id, like `Track->genrename`)
    pub genre: Option<String>,
    pub genre_id: Option<i64>,
    pub year: Option<i64>,
    pub tracknum: Option<i64>,
    pub disc: Option<i64>,
    pub discc: Option<i64>,
    /// LMS content type code: flc mp3 mp4 alc aac ogg ops wav aif wvp ape dsf dff wma mpc ...
    pub content_type: String,
    pub secs: Option<f64>,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub samplesize: Option<u32>,
    pub channels: Option<u32>,
    pub lossless: bool,
    pub vbr: bool,
    /// `tracks.endian`: 0 = little endian (WAV), 1 = big endian (AIFF)
    pub endian: Option<i64>,
    pub block_alignment: Option<u32>,
    pub audio_offset: Option<u64>,
    pub audio_size: Option<u64>,
    pub filesize: Option<u64>,
    /// file mtime
    pub timestamp: Option<i64>,
    pub added_time: Option<i64>,
    pub updated_time: Option<i64>,
    pub replay_gain: Option<f64>,
    pub replay_peak: Option<f64>,
    pub album_replay_gain: Option<f64>,
    pub album_replay_peak: Option<f64>,
    pub coverid: Option<String>,
    /// track has artwork (embedded or folder)
    pub cover: bool,
    /// raw `tracks.cover` value: embedded length, image path, "0" or NULL
    pub cover_value: Option<String>,
    pub remote: bool,
    pub audio: bool,
    pub virtual_cue: bool,
    pub cue_start: Option<f64>,
    pub cue_end: Option<f64>,
    pub musicbrainz_id: Option<String>,
    /// comments joined by "\n"
    pub comment: Option<String>,
    pub lyrics: Option<String>,
    pub rating: Option<u8>,
    pub playcount: Option<u32>,
    pub lastplayed: Option<i64>,
    pub compilation: bool,
    pub bpm: Option<i64>,
    pub tagversion: Option<String>,
    pub extid: Option<String>,
    pub work_id: Option<i64>,
    pub performance: Option<String>,
    pub grouping: Option<String>,
    pub subtitle: Option<String>,
    pub discsubtitle: Option<String>,
    pub album_artwork: Option<String>,
    pub release_type: Option<String>,
    pub urlmd5: String,
}

impl Track {
    /// File path (anchor stripped) for local files.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        crate::url::path_from_file_url(&self.url)
    }

    /// `Track->duration`: `m:ss`
    pub fn duration_str(&self) -> Option<String> {
        self.secs
            .map(|s| format!("{}:{:02}", (s / 60.0) as i64, (s as i64) % 60))
    }

    pub fn is_playlist(&self) -> bool {
        crate::types::is_playlist_type(&self.content_type)
    }

    pub fn is_dir(&self) -> bool {
        self.content_type == "dir"
    }
}

const SELECT: &str = "SELECT t.id, t.url, t.title, t.titlesort, t.album, a.title, t.primary_artist, pa.name,
    a.contributor, ac.name, t.year, t.tracknum, t.disc, a.discc, t.content_type, t.secs, t.bitrate, t.samplerate,
    t.samplesize, t.channels, t.lossless, t.vbr_scale, t.endian, t.block_alignment, t.audio_offset, t.audio_size,
    t.filesize, t.timestamp, t.added_time, t.updated_time, t.replay_gain, t.replay_peak, a.replay_gain, a.replay_peak,
    t.coverid, t.cover, t.remote, t.audio, t.virtual, t.musicbrainz_id, t.lyrics, tp.rating, tp.playCount, tp.lastPlayed,
    a.compilation, t.bpm, t.tagversion, t.extid, t.work, t.performance, t.grouping, t.subtitle, t.discsubtitle,
    a.artwork, a.release_type, t.urlmd5
    FROM tracks t
    LEFT JOIN albums a ON a.id = t.album
    LEFT JOIN contributors pa ON pa.id = t.primary_artist
    LEFT JOIN contributors ac ON ac.id = a.contributor
    LEFT JOIN tracks_persistent tp ON tp.urlmd5 = t.urlmd5 ";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Track> {
    let cover: Option<String> =
        r.get::<_, Option<rusqlite::types::Value>>(35)?
            .and_then(|v| match v {
                rusqlite::types::Value::Text(s) => Some(s),
                rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                rusqlite::types::Value::Real(f) => Some(f.to_string()),
                rusqlite::types::Value::Blob(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                rusqlite::types::Value::Null => None,
            });
    let url: String = r.get(1)?;
    let frag = crate::url::fragment(&url);
    let is_virtual = r.get::<_, Option<String>>(38)?.is_some();
    Ok(Track {
        id: r.get(0)?,
        title: crate::songdata::get_text(r, 2)?.unwrap_or_default(),
        url,
        titlesort: r.get(3)?,
        album_id: r.get(4)?,
        album: crate::songdata::get_text(r, 5)?,
        artist_id: r.get(6)?,
        artist: crate::songdata::get_text(r, 7)?,
        albumartist_id: r.get(8)?,
        albumartist: crate::songdata::get_text(r, 9)?,
        year: r.get(10)?,
        tracknum: r.get(11)?,
        disc: r.get(12)?,
        discc: r.get(13)?,
        content_type: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
        secs: r.get(15)?,
        bitrate: r.get::<_, Option<f64>>(16)?.map(|v| v as u32),
        samplerate: r.get::<_, Option<i64>>(17)?.map(|v| v as u32),
        samplesize: r.get::<_, Option<i64>>(18)?.map(|v| v as u32),
        channels: r.get::<_, Option<i64>>(19)?.map(|v| v as u32),
        lossless: r.get::<_, Option<i64>>(20)?.unwrap_or(0) != 0,
        vbr: r.get::<_, Option<i64>>(21).ok().flatten().unwrap_or(0) == 1,
        endian: r.get(22)?,
        block_alignment: r.get::<_, Option<i64>>(23)?.map(|v| v as u32),
        audio_offset: r.get::<_, Option<i64>>(24)?.map(|v| v as u64),
        audio_size: r.get::<_, Option<i64>>(25)?.map(|v| v as u64),
        filesize: r.get::<_, Option<i64>>(26)?.map(|v| v as u64),
        timestamp: r.get(27)?,
        added_time: r.get(28)?,
        updated_time: r.get(29)?,
        replay_gain: r.get(30)?,
        replay_peak: r.get(31)?,
        album_replay_gain: r.get(32)?,
        album_replay_peak: r.get(33)?,
        coverid: r.get(34)?,
        cover: cover
            .as_deref()
            .map(|c| !c.is_empty() && c != "0")
            .unwrap_or(false),
        cover_value: cover,
        remote: r.get::<_, Option<i64>>(36)?.unwrap_or(0) != 0,
        audio: r.get::<_, Option<i64>>(37)?.unwrap_or(0) != 0,
        virtual_cue: is_virtual,
        cue_start: frag.map(|f| f.0),
        cue_end: frag.map(|f| f.1),
        musicbrainz_id: r.get(39)?,
        lyrics: r.get(40)?,
        rating: r.get::<_, Option<i64>>(41)?.map(|v| v.clamp(0, 255) as u8),
        playcount: r.get::<_, Option<i64>>(42)?.map(|v| v as u32),
        lastplayed: r.get(43)?,
        compilation: r.get::<_, Option<i64>>(44)?.unwrap_or(0) == 1,
        bpm: r.get(45)?,
        tagversion: r.get(46)?,
        extid: r.get(47)?,
        work_id: r.get(48)?,
        performance: crate::songdata::get_text(r, 49)?,
        grouping: crate::songdata::get_text(r, 50)?,
        subtitle: crate::songdata::get_text(r, 51)?,
        discsubtitle: crate::songdata::get_text(r, 52)?,
        album_artwork: r.get(53)?,
        release_type: r.get(54)?,
        urlmd5: r.get(55)?,
        composer: None,
        genre: None,
        genre_id: None,
        comment: None,
    })
}

fn fill_relations(c: &Connection, t: &mut Track) {
    if let Ok(Some((gid, gname))) = c
        .query_row(
            "SELECT genres.id, genres.name FROM genre_track JOIN genres ON genres.id = genre_track.genre WHERE track = ? ORDER BY genre_track.genre LIMIT 1",
            [t.id],
            |r| Ok((r.get::<_, i64>(0)?, crate::songdata::get_text(r, 1)?)),
        )
        .optional()
    {
        t.genre_id = Some(gid);
        t.genre = gname;
    }
    if let Ok(mut st) = c.prepare_cached(
        "SELECT DISTINCT contributors.name FROM contributor_track JOIN contributors ON contributors.id = contributor_track.contributor
         WHERE track = ? AND role = 2",
    ) {
        let names: Vec<String> = st
            .query_map([t.id], |r| crate::songdata::get_text(r, 0))
            .map(|rows| rows.filter_map(|r| r.ok().flatten()).collect())
            .unwrap_or_default();
        if !names.is_empty() {
            t.composer = Some(names.join(", "));
        }
    }
    if let Ok(mut st) = c.prepare_cached("SELECT value FROM comments WHERE track = ? ORDER BY id") {
        let vals: Vec<String> = st
            .query_map([t.id], |r| r.get::<_, Option<String>>(0))
            .map(|rows| {
                rows.filter_map(|r| r.ok().flatten())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if !vals.is_empty() {
            t.comment = Some(vals.join("\n"));
        }
    }
}

pub fn load_by_id(c: &Connection, id: i64) -> Option<Track> {
    let sql = format!("{} WHERE t.id = ?", SELECT);
    let mut t = c.query_row(&sql, [id], from_row).ok()?;
    fill_relations(c, &mut t);
    Some(t)
}

pub fn load_by_url(c: &Connection, u: &str) -> Option<Track> {
    let sql = format!("{} WHERE t.url = ?", SELECT);
    let mut t = c.query_row(&sql, [u], from_row).optional().ok().flatten();
    if t.is_none() {
        // LMS canonicalises URLs; retry with the canonical escaping of a local path
        if let Some(p) = crate::url::path_from_file_url(u) {
            let mut cu = crate::url::file_url_from_path(&p);
            if let Some(a) = crate::url::anchor_from_url(u) {
                cu.push('#');
                cu.push_str(a);
            }
            if cu != u {
                t = c.query_row(&sql, [&cu], from_row).optional().ok().flatten();
            }
        }
    }
    let mut t = t?;
    fill_relations(c, &mut t);
    Some(t)
}

/// Load many tracks preserving the given id order.
pub fn load_many(c: &Connection, ids: &[i64]) -> Vec<Track> {
    ids.iter().filter_map(|id| load_by_id(c, *id)).collect()
}
