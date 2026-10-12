//! Finding tracks by what the library knows about them beyond their names: genres, years,
//! tempo and key (from the rhythm analysis, else the `BPM` tag), rating, listening history and
//! when they were added.  For clients that pick songs themselves (an assistant building a
//! playlist) rather than browse; the LMS queries in [`crate::queries`] can't express these.
//!
//! [`Library::find_tracks`] returns track ids; [`Library::track_details`] the facts about each
//! that the filters work on, so a client can see why a track matched.

use crate::text::TextRules;
use crate::Library;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use std::collections::HashMap;

/// How [`Library::find_tracks`] orders what it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindSort {
    /// By title (the default).
    #[default]
    Title,
    /// Shuffled.
    Random,
    /// Newest first.
    Added,
    /// Most played first.
    MostPlayed,
    /// Played longest ago first; never played before anything played.
    LeastRecentlyPlayed,
    /// Played most recently first.
    RecentlyPlayed,
    /// Highest rated first.
    Rating,
    /// Slowest first.
    Bpm,
    /// Oldest first.
    Year,
}

impl FindSort {
    pub fn parse(s: &str) -> Option<FindSort> {
        Some(match s {
            "title" => FindSort::Title,
            "random" => FindSort::Random,
            "added" => FindSort::Added,
            "most_played" => FindSort::MostPlayed,
            "least_recently_played" => FindSort::LeastRecentlyPlayed,
            "recently_played" => FindSort::RecentlyPlayed,
            "rating" => FindSort::Rating,
            "bpm" => FindSort::Bpm,
            "year" => FindSort::Year,
            _ => return None,
        })
    }

    pub const NAMES: &'static str = "title, random, added, most_played, \
        least_recently_played, recently_played, rating, bpm or year";
}

/// What to find.  Every filter given must hold.
#[derive(Debug, Clone, Default)]
pub struct FindTracks {
    /// Search words, matched like every other search (each word starts a word of the title,
    /// album or artists).
    pub search: Option<String>,
    /// By this artist, in any role.
    pub artist_id: Option<i64>,
    pub album_id: Option<i64>,
    /// In any of these genres.
    pub genre_ids: Vec<i64>,
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// Tempo range, from the rhythm analysis, else the `BPM` tag.  A track with neither is
    /// left out when either bound is given.
    pub bpm_min: Option<f64>,
    pub bpm_max: Option<f64>,
    /// In one of these keys (Camelot codes, `8A`).
    pub keys: Vec<String>,
    /// Rated at least this (0–100).
    pub rating_min: Option<i64>,
    /// `true`: played at least once; `false`: never played.
    pub played: Option<bool>,
    /// Last played at or after this Unix time.
    pub played_since: Option<i64>,
    /// Not played at or after this Unix time (never played counts).
    pub not_played_since: Option<i64>,
    /// Added to the library at or after this Unix time.
    pub added_since: Option<i64>,
    /// Length range in seconds.
    pub duration_min: Option<f64>,
    pub duration_max: Option<f64>,
    pub sort: FindSort,
    pub offset: usize,
    pub limit: usize,
}

/// The facts [`FindTracks`] filters on, for one track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackDetail {
    pub genres: Vec<String>,
    /// From the rhythm analysis, else the `BPM` tag.
    pub bpm: Option<f64>,
    /// Camelot code (`8A`), from the rhythm analysis.
    pub key: Option<String>,
    /// 0–100.
    pub rating: Option<i64>,
    pub play_count: i64,
    pub skip_count: i64,
    pub last_played: Option<i64>,
    pub added: Option<i64>,
    pub mbid: Option<String>,
    pub isrc: Option<String>,
    /// Integrated loudness, LUFS.
    pub loudness: Option<f64>,
    /// The sonic analysis is done, so similar-song mixes can use how it sounds.
    pub analyzed: bool,
}

/// The tables the filters and the details read, joined to `tracks` (aliased `t`).
const JOINS: &str = "LEFT JOIN track_stats s ON s.url = t.url
     LEFT JOIN track_rhythm r ON r.fingerprint = t.fingerprint
     LEFT JOIN track_analysis a ON a.fingerprint = t.fingerprint";

/// Effective tempo: measured, else tagged (a tag of 0 means none).
const BPM: &str = "COALESCE(r.bpm, NULLIF(t.bpm, 0))";

/// A valid Camelot code in canonical form (`8a` → `8A`), or `None`.
pub fn camelot(s: &str) -> Option<String> {
    let s = s.trim().to_ascii_uppercase();
    let (num, letter) = s.split_at(s.len().checked_sub(1)?);
    let n: u8 = num.parse().ok()?;
    ((1..=12).contains(&n) && (letter == "A" || letter == "B")).then(|| format!("{n}{letter}"))
}

/// The keys that mix harmonically with `key` on the Camelot wheel: itself, one step either way
/// round the wheel, and its relative major or minor.
pub fn compatible_keys(key: &str) -> Option<Vec<String>> {
    let k = camelot(key)?;
    let (num, letter) = k.split_at(k.len() - 1);
    let n: u8 = num.parse().ok()?;
    let other = if letter == "A" { "B" } else { "A" };
    let prev = if n == 1 { 12 } else { n - 1 };
    let next = if n == 12 { 1 } else { n + 1 };
    Some(vec![
        k.clone(),
        format!("{prev}{letter}"),
        format!("{next}{letter}"),
        format!("{n}{other}"),
    ])
}

fn where_clause(f: &FindTracks, rules: &TextRules) -> (String, Vec<SqlValue>) {
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(q) = f
        .search
        .as_deref()
        .and_then(|s| crate::queries::fts_query(rules, s))
    {
        w.push("t.id IN (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?)".into());
        p.push(SqlValue::Text(q));
    }
    if let Some(a) = f.artist_id {
        w.push("t.id IN (SELECT track_id FROM track_artists WHERE artist_id = ?)".into());
        p.push(SqlValue::Integer(a));
    }
    if let Some(a) = f.album_id {
        w.push("t.album_id = ?".into());
        p.push(SqlValue::Integer(a));
    }
    if !f.genre_ids.is_empty() {
        w.push(format!(
            "t.id IN (SELECT track_id FROM track_genres WHERE genre_id IN ({}))",
            vec!["?"; f.genre_ids.len()].join(",")
        ));
        p.extend(f.genre_ids.iter().map(|g| SqlValue::Integer(*g)));
    }
    let mut range = |expr: &str, op: &str, v: SqlValue| {
        w.push(format!("{expr} {op} ?"));
        p.push(v);
    };
    if let Some(y) = f.year_from {
        range("NULLIF(t.year, 0)", ">=", SqlValue::Integer(y));
    }
    if let Some(y) = f.year_to {
        range("NULLIF(t.year, 0)", "<=", SqlValue::Integer(y));
    }
    if let Some(b) = f.bpm_min {
        range(BPM, ">=", SqlValue::Real(b));
    }
    if let Some(b) = f.bpm_max {
        range(BPM, "<=", SqlValue::Real(b));
    }
    if let Some(r) = f.rating_min {
        range("s.rating", ">=", SqlValue::Integer(r));
    }
    if let Some(d) = f.duration_min {
        range("t.duration", ">=", SqlValue::Real(d));
    }
    if let Some(d) = f.duration_max {
        range("t.duration", "<=", SqlValue::Real(d));
    }
    if let Some(since) = f.played_since {
        range("s.last_played", ">=", SqlValue::Integer(since));
    }
    if let Some(since) = f.added_since {
        range(
            "COALESCE(s.added_at, t.added_at)",
            ">=",
            SqlValue::Integer(since),
        );
    }
    if let Some(since) = f.not_played_since {
        w.push("(s.last_played IS NULL OR s.last_played < ?)".into());
        p.push(SqlValue::Integer(since));
    }
    match f.played {
        Some(true) => w.push("COALESCE(s.play_count, 0) > 0".into()),
        Some(false) => w.push("COALESCE(s.play_count, 0) = 0".into()),
        None => {}
    }
    if !f.keys.is_empty() {
        w.push(format!(
            "r.camelot IN ({})",
            vec!["?"; f.keys.len()].join(",")
        ));
        p.extend(f.keys.iter().map(|k| SqlValue::Text(k.clone())));
    }
    let clause = if w.is_empty() {
        "1".to_string()
    } else {
        w.join(" AND ")
    };
    (clause, p)
}

fn order_by(sort: FindSort) -> &'static str {
    match sort {
        FindSort::Title => "t.title_sort, t.id",
        FindSort::Random => "RANDOM()",
        FindSort::Added => "COALESCE(s.added_at, t.added_at) DESC, t.id",
        FindSort::MostPlayed => "COALESCE(s.play_count, 0) DESC, s.last_played DESC, t.id",
        FindSort::LeastRecentlyPlayed => "COALESCE(s.last_played, 0), t.title_sort, t.id",
        FindSort::RecentlyPlayed => "s.last_played IS NULL, s.last_played DESC, t.id",
        FindSort::Rating => "s.rating IS NULL, s.rating DESC, t.title_sort, t.id",
        FindSort::Bpm => {
            "COALESCE(r.bpm, NULLIF(t.bpm, 0)) IS NULL, COALESCE(r.bpm, NULLIF(t.bpm, 0)), t.id"
        }
        FindSort::Year => "NULLIF(t.year, 0) IS NULL, t.year, t.title_sort, t.id",
    }
}

fn find(c: &Connection, rules: &TextRules, f: &FindTracks) -> rusqlite::Result<(usize, Vec<i64>)> {
    let (clause, params) = where_clause(f, rules);
    let total: i64 = c.query_row(
        &format!("SELECT COUNT(*) FROM tracks t {JOINS} WHERE {clause}"),
        params_from_iter(params.iter()),
        |r| r.get(0),
    )?;
    let sql = format!(
        "SELECT t.id FROM tracks t {JOINS} WHERE {clause} ORDER BY {} LIMIT {} OFFSET {}",
        order_by(f.sort),
        f.limit,
        f.offset
    );
    let ids = c
        .prepare(&sql)?
        .query_map(params_from_iter(params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok((total.max(0) as usize, ids))
}

fn details(c: &Connection, ids: &[i64]) -> rusqlite::Result<HashMap<i64, TrackDetail>> {
    let mut out: HashMap<i64, TrackDetail> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let list = vec!["?"; ids.len()].join(",");
    let mut st = c.prepare(&format!(
        "SELECT t.id, {BPM}, r.camelot, s.rating, COALESCE(s.play_count, 0),
                COALESCE(s.skip_count, 0), s.last_played, COALESCE(s.added_at, t.added_at),
                t.mbid, t.isrc, a.loudness, a.features IS NOT NULL
         FROM tracks t {JOINS} WHERE t.id IN ({list})"
    ))?;
    let rows = st.query_map(params_from_iter(ids.iter()), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            TrackDetail {
                genres: Vec::new(),
                bpm: r.get(1)?,
                key: r.get(2)?,
                rating: r.get(3)?,
                play_count: r.get(4)?,
                skip_count: r.get(5)?,
                last_played: r.get(6)?,
                added: r.get(7)?,
                mbid: r.get(8)?,
                isrc: r.get(9)?,
                loudness: r.get(10)?,
                analyzed: r.get::<_, Option<bool>>(11)?.unwrap_or(false),
            },
        ))
    })?;
    for row in rows {
        let (id, d) = row?;
        out.insert(id, d);
    }
    let mut st = c.prepare(&format!(
        "SELECT g.track_id, genres.name FROM track_genres g JOIN genres ON genres.id = g.genre_id
         WHERE g.track_id IN ({list}) ORDER BY genres.name_sort"
    ))?;
    let rows = st.query_map(params_from_iter(ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, g) = row?;
        if let Some(d) = out.get_mut(&id) {
            d.genres.push(g);
        }
    }
    Ok(out)
}

impl Library {
    /// The tracks matching `f`, in its order: (how many match in all, the ids of the page).
    pub fn find_tracks(&self, f: &FindTracks) -> anyhow::Result<(usize, Vec<i64>)> {
        let cfg = self.config();
        let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
        let c = self.conn()?;
        Ok(find(&c, &rules, f)?)
    }

    /// The tracks of the file at `url`: the whole file, or the tracks of its embedded cue
    /// sheet (`url#start-end`), in order.
    pub fn tracks_of_file(&self, url: &str) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let Ok(mut st) = c.prepare(
            "SELECT id FROM tracks WHERE url = ?1 OR substr(url, 1, length(?1) + 1) = ?1 || '#'
             ORDER BY COALESCE(cue_start, 0), id",
        ) else {
            return Vec::new();
        };
        st.query_map([url], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The facts about each of `ids` that [`FindTracks`] filters on (ids that aren't library
    /// tracks are left out).
    pub fn track_details(&self, ids: &[i64]) -> HashMap<i64, TrackDetail> {
        let Ok(c) = self.conn() else {
            return HashMap::new();
        };
        details(&c, ids).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camelot_codes() {
        assert_eq!(camelot("8a").as_deref(), Some("8A"));
        assert_eq!(camelot(" 12B ").as_deref(), Some("12B"));
        assert_eq!(camelot("13A"), None);
        assert_eq!(camelot("0B"), None);
        assert_eq!(camelot("8C"), None);
        assert_eq!(camelot(""), None);
        assert_eq!(
            compatible_keys("12a").unwrap(),
            ["12A", "11A", "1A", "12B"].map(String::from)
        );
        assert_eq!(
            compatible_keys("1B").unwrap(),
            ["1B", "12B", "2B", "1A"].map(String::from)
        );
    }

    #[test]
    fn every_sort_parses() {
        for name in FindSort::NAMES.replace(" or ", ", ").split(", ") {
            assert!(FindSort::parse(name).is_some(), "{name}");
        }
        assert_eq!(FindSort::parse("nope"), None);
    }
}
