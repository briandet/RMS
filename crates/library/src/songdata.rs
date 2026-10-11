//! Track tag-letter rendering for the LMS queries: `_getTagDataForTracks` + `_songDataFromHash` (SQL path, used by
//! `titles` and `status`) and `_songData` (object path, used by `songinfo`, `playlists tracks`).

use std::collections::HashMap;

use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::config::{role_to_type, type_to_role, LibraryConfig, CONTRIBUTOR_ROLES};
use crate::track::Track;
use crate::Row;

/// Contributors of one track by role: role → (ids, names).
type RoleContributors = IndexMap<i64, (Vec<i64>, Vec<String>)>;
/// `albums` columns the object path needs: disc count, artwork, compilation, release type,
/// replay gain.
type AlbumRow = (
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<f64>,
);

pub fn get_text(r: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<String>> {
    Ok(match r.get::<_, SqlValue>(idx)? {
        SqlValue::Null => None,
        SqlValue::Text(s) => Some(s),
        SqlValue::Blob(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        SqlValue::Integer(i) => Some(i.to_string()),
        SqlValue::Real(f) => Some(crate::cue::perl_num(f)),
    })
}

/// SQL value → JSON value (integers/reals as numbers, text/blob as strings).
pub fn sql_to_json(v: &SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        SqlValue::Text(s) => json!(s),
        SqlValue::Blob(b) => json!(String::from_utf8_lossy(b)),
    }
}

/// Perl `defined $value && $value ne ''`
pub fn is_present(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `Slim::Schema::Track->buildPrettyBitRate`
pub fn pretty_bitrate(
    bitrate: Option<f64>,
    vbr: Option<i64>,
    ct: Option<&str>,
    kbps: &str,
) -> Value {
    match bitrate {
        Some(b) if b != 0.0 => {
            let mode = if crate::types::is_lossy(ct.unwrap_or("")) {
                if vbr == Some(1) {
                    " VBR"
                } else {
                    " CBR"
                }
            } else {
                ""
            };
            json!(format!("{}{}{}", (b / 1000.0) as i64, kbps, mode))
        }
        _ => json!(0),
    }
}

/// LMS long date + time ("Saturday, October 5, 2026, 2:21 AM").
pub fn format_time(epoch: i64, cfg: &LibraryConfig) -> String {
    use chrono::{Local, TimeZone};
    let Some(dt) = Local.timestamp_opt(epoch, 0).single() else {
        return String::new();
    };
    let date = dt.format(&cfg.long_date_format).to_string();
    let re = crate::re!(r"\|0*");
    let date = re.replacen(&date, 1, "").to_string();
    let time = dt.format(&cfg.time_format).to_string();
    let re2 = crate::re!(r"\|0?(\d+)");
    let time = re2.replacen(&time, 1, "$1").to_string();
    format!("{}, {}", date, time)
}

#[derive(Debug, Clone, Default)]
pub struct TagDataArgs {
    pub where_base: Option<String>,
    pub sort: Option<String>,
    pub search: Option<String>,
    pub album_id: Option<String>,
    pub track_id: Option<String>,
    pub work_id: Option<String>,
    pub performance: Option<String>,
    pub year: Option<String>,
    pub only_album_years: bool,
    pub release_type: Option<String>,
    pub genre_id: Option<String>,
    pub contributor_id: Option<String>,
    pub track_ids: Option<Vec<i64>>,
    pub role_id: Option<String>,
    /// (start, quantity) – set by the caller after the count is known
    pub paged: bool,
    pub index: Option<usize>,
    pub quantity: Option<usize>,
}

pub struct TagDataResult {
    pub rows: Vec<IndexMap<String, Value>>,
    pub total: usize,
    pub album_header: Option<Value>,
    /// output of `normalize` when paged: (valid, start, end)
    pub valid: bool,
}

/// Request::normalize
pub fn normalize(from: Option<usize>, qty: Option<usize>, count: usize) -> (bool, usize, usize) {
    let from_v = from.unwrap_or(0);
    let qty = match (qty, from) {
        (None, Some(_)) => Some(count),
        (q, _) => q,
    };
    match qty {
        Some(q) if q > 0 && count > 0 => {
            let last = count - 1;
            if from_v > last {
                return (false, 0, 0);
            }
            let end = (from_v + q - 1).min(last);
            (true, from_v, end)
        }
        _ => (false, 0, 0),
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Role ids used for the `a`/`s` tag filter.
fn ta_roles(cfg: &LibraryConfig, role_id: Option<&str>) -> Vec<i64> {
    let names: Vec<String> = if let Some(r) = role_id.filter(|r| !r.is_empty()) {
        let mut v = split_list(r);
        if r == "ALBUMARTIST" && !cfg.use_unified_artists_list {
            v.push("ARTIST".into());
        }
        v
    } else if cfg.use_unified_artists_list {
        cfg.active_contributor_roles(true)
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        CONTRIBUTOR_ROLES
            .iter()
            .map(|(n, _)| n.to_string())
            .collect()
    };
    names.iter().filter_map(|n| type_to_role(n)).collect()
}

/// `_getTagDataForTracks`
pub fn tag_data(
    c: &Connection,
    cfg: &LibraryConfig,
    tags_in: &str,
    args: &TagDataArgs,
) -> anyhow::Result<TagDataResult> {
    let rules = crate::text::TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let mut tags = tags_in.to_string();
    let mut sort = args.sort.clone();
    let count_only = tags == "CC";
    if count_only {
        tags.clear();
        sort = None;
    }
    let mut from = String::from("FROM tracks JOIN files ON files.id = tracks.file_id ");
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(wb) = &args.where_base {
        w.push(wb.clone());
    }
    let mut one_album = false;

    if let Some(search) = args
        .search
        .as_ref()
        .filter(|s| crate::queries::specified(s))
    {
        if args.track_id.is_none() {
            let (clause, params, order) = crate::queries::search_clause(
                &rules,
                cfg,
                search,
                "tracks.search_text",
                "tracks.title_search",
            );
            w.insert(0, clause);
            for (i, v) in params.into_iter().enumerate() {
                p.insert(i, v.into());
            }
            if !count_only {
                sort = Some(match sort {
                    Some(s) => format!("{}, {}", order, s),
                    None => order,
                });
            }
        }
    }
    if let Some(a) = args.album_id.as_ref().filter(|a| !a.is_empty()) {
        let ids = split_list(a);
        one_album = ids.len() == 1;
        w.push(format!(
            "tracks.album_id IN ({})",
            vec!["?"; ids.len()].join(",")
        ));
        p.extend(ids.into_iter().map(SqlValue::Text));
    }
    if let Some(t) = args.track_id.as_ref().filter(|a| !a.is_empty()) {
        let ids = split_list(t);
        w.push(format!("tracks.id IN ({})", vec!["?"; ids.len()].join(",")));
        p.extend(ids.into_iter().map(SqlValue::Text));
    }
    if let Some(wk) = args.work_id.as_ref().filter(|a| !a.is_empty()) {
        if wk == "-1" {
            w.push("tracks.work_id IS NOT NULL".into());
        } else {
            w.push("tracks.work_id = ?".into());
            p.push(SqlValue::Text(wk.clone()));
            match &args.performance {
                Some(perf) if !perf.is_empty() => {
                    if perf != "-1" {
                        w.push("tracks.performance = ?".into());
                        p.push(SqlValue::Text(perf.clone()));
                    }
                }
                _ => w.push("tracks.performance IS NULL".into()),
            }
        }
    }
    let join = |from: &mut String, what: &str| {
        let s = match what {
            "genre_track" => "LEFT JOIN track_genres ON track_genres.track_id = tracks.id ",
            "genres" => "LEFT JOIN genres ON genres.id = track_genres.genre_id ",
            "works" => "LEFT JOIN works ON works.id = tracks.work_id ",
            "contributor_track" => "LEFT JOIN track_artists ON track_artists.track_id = tracks.id ",
            "contributors" => "LEFT JOIN artists ON artists.id = tracks.artist_id ",
            "albums" => "LEFT JOIN albums ON albums.id = tracks.album_id ",
            "tracks_persistent" => "LEFT JOIN track_stats ON track_stats.url = tracks.url ",
            _ => "",
        };
        if !from.contains(s) {
            from.push_str(s);
        }
    };
    if let Some(y) = args.year.as_ref().filter(|a| !a.is_empty() && *a != "0") {
        w.push("tracks.year = ?".into());
        p.push(SqlValue::Text(y.clone()));
        if args.only_album_years {
            join(&mut from, "albums");
            w.push("albums.year = ?".into());
            p.push(SqlValue::Text(y.clone()));
        }
    }
    if args.album_id.is_none() {
        if let Some(rt) = args.release_type.as_ref().filter(|a| !a.is_empty()) {
            join(&mut from, "albums");
            let rts: Vec<String> = split_list(rt)
                .into_iter()
                .map(|s| s.to_uppercase())
                .collect();
            w.push(format!(
                "albums.release_type IN ({})",
                vec!["?"; rts.len()].join(", ")
            ));
            p.extend(rts.into_iter().map(SqlValue::Text));
        }
    }
    if let Some(g) = args.genre_id.as_ref().filter(|a| !a.is_empty()) {
        join(&mut from, "genre_track");
        let ids = split_list(g);
        w.push(format!(
            "track_genres.genre_id IN ({})",
            vec!["?"; ids.len()].join(", ")
        ));
        p.extend(ids.into_iter().map(SqlValue::Text));
    }
    if let Some(cid) = args.contributor_id.as_ref().filter(|a| !a.is_empty()) {
        let va = crate::queries::va_id_cfg(c, cfg);
        if cid.parse::<i64>().ok() == va {
            join(&mut from, "albums");
            w.push("albums.compilation = 1".into());
        } else {
            join(&mut from, "contributor_track");
            w.push("track_artists.artist_id = ?".into());
            p.push(SqlValue::Text(cid.clone()));
        }
    }
    if let Some(ids) = &args.track_ids {
        let list: Vec<String> = ids
            .iter()
            .filter(|i| **i > 0)
            .map(|i| i.to_string())
            .collect();
        w.push(format!(
            "tracks.id IN ({})",
            if list.is_empty() {
                "-1".to_string()
            } else {
                list.join(",")
            }
        ));
    }

    let has = |ch: char| tags.contains(ch);
    if has('b') {
        join(&mut from, "works");
    }
    if has('g') || has('p') {
        join(&mut from, "genre_track");
        join(&mut from, "genres");
    }
    if has('a') || has('s') {
        join(&mut from, "contributor_track");
        join(&mut from, "contributors");
        let roles = ta_roles(cfg, args.role_id.as_deref());
        if args.track_ids.is_some() {
            w.push("(artists.id = tracks.artist_id OR tracks.artist_id IS NULL)".into());
        }
        w.push(format!(
            "track_artists.role IN ({})",
            vec!["?"; roles.len()].join(", ")
        ));
        p.extend(roles.into_iter().map(SqlValue::Integer));
    }
    if has('4') {
        join(&mut from, "contributor_track");
        join(&mut from, "contributors");
    }
    if has('l') || has('q') || has('J') || has('C') || has('W') || has('X') {
        join(&mut from, "albums");
    }
    if has('R') || has('O') {
        join(&mut from, "tracks_persistent");
    }
    if let Some(s) = &sort {
        if s.contains("albums.") {
            join(&mut from, "albums");
        }
        if s.contains("artists.") {
            join(&mut from, "contributor_track");
            join(&mut from, "contributors");
        }
        if s.contains("track_stats.") {
            join(&mut from, "tracks_persistent");
        }
    }
    for clause in &w {
        if clause.contains("track_stats.") {
            join(&mut from, "tracks_persistent");
        }
        if clause.contains("albums.") {
            join(&mut from, "albums");
        }
    }

    let cols: Vec<&str> = vec![
        "tracks.id",
        "tracks.title",
        "tracks.album_id",
        "tracks.duration",
        "tracks.track_number",
        "tracks.year",
        "tracks.bpm",
        "tracks.lossless",
        "tracks.format",
        "tracks.tag_version",
        "tracks.bitrate",
        "tracks.vbr",
        "files.size",
        "tracks.cover",
        "files.mtime",
        "tracks.added_at",
        "tracks.updated_at",
        "tracks.sample_rate",
        "tracks.channels",
        "tracks.sample_size",
        "tracks.url",
        "tracks.lyrics",
        "tracks.cover_id",
        "tracks.replay_gain",
        "tracks.disc",
        "tracks.disc_subtitle",
        "tracks.grouping",
        "tracks.performance",
        "tracks.subtitle",
        "tracks.artist_id",
    ];
    let mut cols: Vec<String> = cols.into_iter().map(|s| s.to_string()).collect();
    if from.contains("JOIN works ") {
        cols.push("works.title".into());
        cols.push("works.id".into());
    }
    if from.contains("JOIN genres ") {
        cols.push("genres.name".into());
        cols.push("genres.id".into());
    }
    if from.contains("JOIN artists ") {
        cols.push("artists.name".into());
        cols.push("artists.id".into());
        cols.push("artists.portrait_id".into());
    }
    if from.contains("JOIN albums ") {
        for a in [
            "albums.title",
            "albums.disc_count",
            "albums.artwork",
            "albums.compilation",
            "albums.release_type",
            "albums.replay_gain",
        ] {
            cols.push(a.into());
        }
    }
    if from.contains("JOIN track_stats ") {
        cols.push("track_stats.rating".into());
        cols.push("track_stats.play_count".into());
    }

    let mut sql = format!("SELECT {} {}", cols.join(", "), from);
    if !w.is_empty() {
        sql.push_str("WHERE ");
        sql.push_str(&w.join(" AND "));
        sql.push(' ');
    }
    if from.contains("LEFT JOIN ") {
        sql.push_str("GROUP BY tracks.id ");
    }
    if let Some(s) = &sort {
        sql.push_str(&format!("ORDER BY {} ", s));
    }

    let mut total = 0usize;
    let mut valid = true;
    if count_only || args.paged {
        let csql = format!("SELECT COUNT(1) FROM ({}) AS t1", sql);
        total = c.query_row(&csql, rusqlite::params_from_iter(p.iter()), |r| {
            r.get::<_, i64>(0)
        })? as usize;
        if count_only {
            return Ok(TagDataResult {
                rows: Vec::new(),
                total,
                album_header: None,
                valid: false,
            });
        }
        let (v, _s, _e) = normalize(args.index, args.quantity, total);
        valid = v;
        if !valid {
            return Ok(TagDataResult {
                rows: Vec::new(),
                total,
                album_header: None,
                valid,
            });
        }
        let qty = match (args.quantity, args.index) {
            (None, Some(_)) => total,
            (q, _) => q.unwrap_or(total),
        };
        sql.push_str(&format!("LIMIT {}, {} ", args.index.unwrap_or(0), qty));
    }

    let mut st = c.prepare(&sql)?;
    let ncols = cols.len();
    let mut rows = st.query(rusqlite::params_from_iter(p.iter()))?;
    let mut results: Vec<IndexMap<String, Value>> = Vec::new();
    let mut order: Vec<i64> = Vec::new();
    while let Some(r) = rows.next()? {
        let mut h: IndexMap<String, Value> = IndexMap::new();
        for (i, col) in cols.iter().enumerate().take(ncols) {
            let v: SqlValue = r.get(i)?;
            h.insert(col.clone(), sql_to_json(&v));
        }
        if has('W') && cfg.ignore_release_types {
            h.insert("albums.release_type".into(), json!("ALBUM"));
        }
        let id = h["tracks.id"].as_i64().unwrap_or(0);
        order.push(id);
        results.push(h);
    }
    drop(rows);
    drop(st);
    if !(count_only || args.paged) {
        total = results.len();
    }

    let id_list: Vec<String> = order.iter().map(|i| i.to_string()).collect();
    let mut album_header = None;
    if (has('A') || has('S')) && !order.is_empty() {
        let sql = format!(
            "SELECT artists.id, artists.name, track_artists.track_id, track_artists.role FROM track_artists
             JOIN artists ON artists.id = track_artists.artist_id WHERE track_artists.track_id IN ({})
             ORDER BY track_artists.role DESC, track_artists.position",
            id_list.join(",")
        );
        let mut st = c.prepare(&sql)?;
        let mut q = st.query([])?;
        // track → role → (ids, names)
        let mut values: HashMap<i64, RoleContributors> = HashMap::new();
        while let Some(r) = q.next()? {
            let cid: i64 = r.get(0)?;
            let name = get_text(r, 1)?.unwrap_or_default();
            let track: i64 = r.get(2)?;
            let role: i64 = r.get(3)?;
            let e = values.entry(track).or_default().entry(role).or_default();
            e.0.push(cid);
            e.1.push(name);
        }
        let sep = if tags.contains("AA") { "," } else { ", " };
        let want_names = has('A');
        let want_ids = has('S');
        let album_roles: Vec<i64> = cfg
            .all_album_link_roles()
            .iter()
            .filter_map(|r| type_to_role(r))
            .collect();
        let mut names_h: Vec<String> = Vec::new();
        let mut ids_h: Vec<i64> = Vec::new();
        for (i, tid) in order.iter().enumerate() {
            let Some(roles) = values.get(tid) else {
                continue;
            };
            for (role_id, (ids, names)) in roles {
                let role = role_to_type(*role_id)
                    .map(|s| s.to_lowercase())
                    .unwrap_or_else(|| role_id.to_string());
                if want_ids {
                    results[i].insert(
                        format!("{}_ids", role),
                        json!(ids
                            .iter()
                            .map(|x| x.to_string())
                            .collect::<Vec<_>>()
                            .join(",")),
                    );
                }
                if want_names {
                    results[i].insert(role.clone(), json!(names.join(sep)));
                }
            }
            if one_album {
                for r in &album_roles {
                    if let Some((ids, names)) = roles.get(r) {
                        names_h.extend(names.iter().cloned());
                        ids_h.extend(ids.iter().copied());
                    }
                }
            }
        }
        if !ids_h.is_empty() {
            album_header = Some(json!({"title_names": uniq(names_h), "title_ids": uniq(ids_h)}));
        } else if one_album {
            if let Some(aid) = &args.album_id {
                if let Ok((id, name)) = c.query_row(
                    "SELECT albums.artist_id, artists.name FROM albums JOIN artists ON albums.artist_id = artists.id WHERE albums.id = ?",
                    [aid],
                    |r| Ok((r.get::<_, i64>(0)?, get_text(r, 1)?.unwrap_or_default())),
                ) {
                    album_header = Some(json!({"title_names": [name], "title_ids": [id]}));
                }
            }
        }
    }
    if (has('G') || has('P')) && !order.is_empty() {
        let sql = format!(
            "SELECT genres.id, genres.name, track_genres.track_id FROM track_genres JOIN genres ON genres.id = track_genres.genre_id
             WHERE track_genres.track_id IN ({}) ORDER BY genres.name_sort COLLATE perllocale",
            id_list.join(",")
        );
        let mut st = c.prepare(&sql)?;
        let mut q = st.query([])?;
        let mut values: HashMap<i64, (Vec<String>, Vec<String>)> = HashMap::new();
        while let Some(r) = q.next()? {
            let gid: i64 = r.get(0)?;
            let name = get_text(r, 1)?.unwrap_or_default();
            let track: i64 = r.get(2)?;
            let e = values.entry(track).or_default();
            e.0.push(gid.to_string());
            e.1.push(name);
        }
        for (i, tid) in order.iter().enumerate() {
            if let Some((ids, names)) = values.get(tid) {
                if has('P') {
                    results[i].insert("genre_ids".into(), json!(ids.join(",")));
                }
                if has('G') {
                    results[i].insert("genres".into(), json!(names.join(", ")));
                    if cfg.is_classical_genre(names) {
                        results[i].insert("isClassical".into(), json!(1));
                    }
                }
            }
        }
    }
    if has('k') && !order.is_empty() {
        let sql = format!(
            "SELECT track_id, text FROM track_comments WHERE track_id IN ({}) ORDER BY track_id, position",
            id_list.join(",")
        );
        let mut st = c.prepare(&sql)?;
        let mut q = st.query([])?;
        let mut values: HashMap<i64, String> = HashMap::new();
        while let Some(r) = q.next()? {
            let track: i64 = r.get(0)?;
            let v = get_text(r, 1)?.unwrap_or_default();
            let e = values.entry(track).or_default();
            if !e.is_empty() {
                e.push_str(" / ");
            }
            e.push_str(&v);
        }
        for (i, tid) in order.iter().enumerate() {
            if let Some(v) = values.get(tid) {
                results[i].insert("comment".into(), json!(v));
            }
        }
    }
    Ok(TagDataResult {
        rows: results,
        total,
        album_header,
        valid,
    })
}

fn uniq<T: PartialEq + Clone>(v: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for x in v {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

fn col_value(res: &IndexMap<String, Value>, tag: char, cfg: &LibraryConfig) -> Option<Value> {
    let g = |k: &str| res.get(k).cloned().unwrap_or(Value::Null);
    let truthy = |k: &str| -> bool {
        match res.get(k) {
            Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
            Some(Value::String(s)) => !s.is_empty() && s != "0",
            Some(Value::Bool(b)) => *b,
            _ => false,
        }
    };
    let v = match tag {
        'g' => g("genres.name"),
        'G' => g("genres"),
        'p' => g("genres.id"),
        'P' => g("genre_ids"),
        'a' => g("artists.name"),
        's' => g("artists.id"),
        '4' => g("artists.portrait_id"),
        'l' => g("albums.title"),
        'e' => g("tracks.album_id"),
        'd' => g("tracks.duration"),
        'i' => g("tracks.disc"),
        'q' => g("albums.disc_count"),
        't' => g("tracks.track_number"),
        'y' => g("tracks.year"),
        'm' => g("tracks.bpm"),
        'M' => json!(0),
        'Q' => json!(if truthy("tracks.lossless") { 1 } else { 0 }),
        'k' => g("comment"),
        'o' => g("tracks.format"),
        'v' => g("tracks.tag_version"),
        'r' => pretty_bitrate(
            res.get("tracks.bitrate").and_then(|v| v.as_f64()),
            res.get("tracks.vbr").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }),
            res.get("tracks.format").and_then(|v| v.as_str()),
            &cfg.string_kbps,
        ),
        'f' => g("files.size"),
        'j' => json!(if truthy("tracks.cover") { 1 } else { 0 }),
        'J' => g("albums.artwork"),
        'n' => g("files.mtime"),
        'F' => Value::Null,
        'D' => g("tracks.added_at"),
        'U' => g("tracks.updated_at"),
        'C' => json!(if truthy("albums.compilation") { 1 } else { 0 }),
        'Y' => g("tracks.replay_gain"),
        'X' => g("albums.replay_gain"),
        'R' => g("track_stats.rating"),
        // never played: no play count, as in LMS
        'O' => g("track_stats.play_count")
            .as_i64()
            .filter(|n| *n > 0)
            .map(|n| json!(n))
            .unwrap_or(Value::Null),
        'T' => g("tracks.sample_rate"),
        'I' => g("tracks.sample_size"),
        'u' => g("tracks.url"),
        'w' => g("tracks.lyrics"),
        'W' => g("albums.release_type"),
        'x' => json!(0),
        'c' => g("tracks.cover_id"),
        'H' => g("tracks.channels"),
        'E' => Value::Null,
        'b' => g("works.title"),
        'h' => g("tracks.grouping"),
        '1' => g("tracks.performance"),
        'z' => g("tracks.subtitle"),
        _ => return None,
    };
    Some(v)
}

fn field_name(tag: char) -> Option<&'static str> {
    Some(match tag {
        'u' => "url",
        'o' => "type",
        'a' => "artist",
        's' => "artist_id",
        'e' => "album_id",
        'l' => "album",
        'b' => "work",
        '1' => "performance",
        'h' => "grouping",
        'z' => "subtitle",
        't' => "tracknum",
        'n' => "modificationTime",
        'D' => "addedTime",
        'U' => "lastUpdated",
        'f' => "filesize",
        'i' => "disc",
        'j' => "coverart",
        'x' => "remote",
        'y' => "year",
        'd' => "duration",
        'r' => "bitrate",
        'T' => "samplerate",
        'I' => "samplesize",
        'H' => "channels",
        'F' => "dlna_profile",
        'm' => "bpm",
        'v' => "tagversion",
        'M' => "musicmagic_mixable",
        'Q' => "lossless",
        'w' => "lyrics",
        'R' => "rating",
        'O' => "playcount",
        'Y' => "replay_gain",
        'c' => "coverid",
        'K' => "artwork_url",
        'B' => "buttons",
        'L' => "info_link",
        'N' => "remote_title",
        'E' => "extid",
        'V' => "live_edge",
        'g' => "genre",
        'p' => "genre_id",
        'A' => "<role>",
        'S' => "<role>_ids",
        '4' => "portraitid",
        'q' => "disccount",
        'J' => "artwork_track_id",
        'C' => "compilation",
        'W' => "release_type",
        'X' => "album_replay_gain",
        'G' => "genres",
        'P' => "genre_ids",
        'k' => "comment",
        '2' => "added_from_work",
        _ => return None,
    })
}

/// `_songDataFromHash`
pub fn song_data_from_hash(
    res: &mut IndexMap<String, Value>,
    tags: &str,
    cfg: &LibraryConfig,
) -> Row {
    let mut out = Row::new();
    out.insert(
        "id".into(),
        res.get("tracks.id").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "title".into(),
        res.get("tracks.title").cloned().unwrap_or(Value::Null),
    );
    for tag in tags.chars() {
        let Some(fname) = field_name(tag) else {
            continue;
        };
        match tag {
            'A' => {
                let artist = res.get("artist").cloned();
                let aa = res.get("albumartist").cloned();
                if let (Some(a), Some(b)) = (&artist, &aa) {
                    if is_present(a) && is_present(b) && a != b && !res.contains_key("trackartist")
                    {
                        res.insert("trackartist".into(), a.clone());
                        if let Some(ids) = res.get("artist_ids").cloned() {
                            res.insert("trackartist_ids".into(), ids);
                        }
                    }
                }
                for (r, _) in CONTRIBUTOR_ROLES {
                    let k = r.to_lowercase();
                    if let Some(v) = res.get(&k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            'S' => {
                for (r, _) in CONTRIBUTOR_ROLES {
                    let k = format!("{}_ids", r.to_lowercase());
                    if let Some(v) = res.get(&k) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
            'b' => {
                if let Some(v) = res.get("works.title").filter(|v| truthy_json(v)) {
                    out.insert("work".into(), v.clone());
                }
                if let Some(v) = res.get("works.id").filter(|v| truthy_json(v)) {
                    out.insert("work_id".into(), v.clone());
                }
            }
            'i' => {
                if let Some(v) = res.get("tracks.disc").filter(|v| truthy_json(v)) {
                    out.insert("disc".into(), v.clone());
                }
                if let Some(v) = res.get("tracks.disc_subtitle").filter(|v| truthy_json(v)) {
                    out.insert("discsubtitle".into(), v.clone());
                }
            }
            'G' => {
                if let Some(v) = res.get("genres").filter(|v| truthy_json(v)) {
                    out.insert("genres".into(), v.clone());
                    if let Some(c) = res.get("isClassical") {
                        out.insert("isClassical".into(), c.clone());
                    }
                }
            }
            '2' => {
                if let Some(v) = res.get("added_from_work").filter(|v| truthy_json(v)) {
                    out.insert("added_from_work".into(), v.clone());
                }
            }
            _ => {
                if let Some(v) = col_value(res, tag, cfg) {
                    if is_present(&v) {
                        out.insert(fname.to_string(), v);
                    }
                }
            }
        }
    }
    out
}

fn truthy_json(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::Bool(b) => *b,
        _ => true,
    }
}

/// Rows for the given track ids (SQL path), in the order of `ids` (duplicates allowed).
pub fn sql_rows(
    c: &Connection,
    cfg: &LibraryConfig,
    ids: &[i64],
    tags: &str,
) -> anyhow::Result<Vec<Row>> {
    let args = TagDataArgs {
        track_ids: Some(ids.to_vec()),
        ..Default::default()
    };
    let res = tag_data(c, cfg, tags, &args)?;
    let mut by_id: HashMap<i64, IndexMap<String, Value>> = HashMap::new();
    for r in res.rows {
        let id = r["tracks.id"].as_i64().unwrap_or(0);
        by_id.insert(id, r);
    }
    let mut out = Vec::new();
    for id in ids {
        if let Some(h) = by_id.get(id) {
            let mut h = h.clone();
            out.push(song_data_from_hash(&mut h, tags, cfg));
        }
    }
    Ok(out)
}

pub fn minimal_row(t: &Track) -> Row {
    let mut r = Row::new();
    r.insert("id".into(), json!(t.id));
    r.insert("title".into(), json!(t.title));
    r
}

/// `_songData` object path for a local track.
pub fn object_row(c: &Connection, cfg: &LibraryConfig, t: &Track, tags: &str) -> Row {
    let mut out = Row::new();
    out.insert("id".into(), json!(t.id));
    out.insert("title".into(), json!(t.title));
    let mut seen = std::collections::HashSet::new();
    let opt_i = |v: Option<i64>| v.map(|x| json!(x)).unwrap_or(Value::Null);
    let opt_s = |v: &Option<String>| v.as_ref().map(|x| json!(x)).unwrap_or(Value::Null);
    let opt_f = |v: Option<f64>| v.map(num_json).unwrap_or(Value::Null);
    let album_row: Option<AlbumRow> = t.album_id.and_then(|a| {
        c.query_row("SELECT disc_count, artwork, compilation, release_type, replay_gain FROM albums WHERE id = ?", [a], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .ok()
    });
    for tag in tags.chars() {
        if !seen.insert(tag) {
            continue;
        }
        let Some(fname) = field_name(tag) else {
            continue;
        };
        let value: Value = match tag {
            'N' | 'B' | 'L' | 'V' => continue,
            'b' => {
                // local tracks: `work` comes from remote metadata only → undef
                out.insert("work".into(), Value::Null);
                continue;
            }
            '2' => continue,
            'A' | 'S' => {
                let ids = tag == 'S';
                for (role, rid) in CONTRIBUTOR_ROLES {
                    let sql = if ids {
                        "SELECT artist_id FROM track_artists WHERE track_id = ? AND role = ? ORDER BY position"
                    } else {
                        "SELECT artists.name FROM track_artists JOIN artists ON artists.id = track_artists.artist_id
                         WHERE track_id = ? AND role = ? ORDER BY track_artists.position"
                    };
                    let vals: Vec<String> = c
                        .prepare_cached(sql)
                        .and_then(|mut st| {
                            let rows =
                                st.query_map(rusqlite::params![t.id, rid], |r| get_text(r, 0))?;
                            Ok(rows.filter_map(|r| r.ok().flatten()).collect())
                        })
                        .unwrap_or_default();
                    if !vals.is_empty() {
                        let key =
                            format!("{}{}", role.to_lowercase(), if ids { "_ids" } else { "" });
                        out.insert(key, json!(vals.join(", ")));
                    }
                }
                continue;
            }
            'u' => json!(t.url),
            'o' => json!(t.content_type),
            'a' => opt_s(&t.artist),
            's' => opt_i(t.artist_id),
            'e' => opt_i(t.album_id),
            'l' => opt_s(&t.album),
            '1' => opt_s(&t.performance),
            'h' => opt_s(&t.grouping),
            'z' => opt_s(&t.subtitle),
            't' => opt_i(t.tracknum),
            'n' => t
                .timestamp
                .map(|x| json!(format_time(x, cfg)))
                .unwrap_or(Value::Null),
            'D' => t
                .added_time
                .map(|x| json!(format_time(x, cfg)))
                .unwrap_or(Value::Null),
            'U' => t
                .updated_time
                .map(|x| json!(format_time(x, cfg)))
                .unwrap_or(Value::Null),
            'f' => t.filesize.map(|x| json!(x)).unwrap_or(Value::Null),
            'i' => opt_i(t.disc),
            'j' => {
                // coverArtExists: defined($self->cover) ? 1 : ...
                if t.cover_value.is_some() {
                    json!(1)
                } else {
                    Value::Null
                }
            }
            'x' => json!(if t.remote { 1 } else { 0 }),
            'y' => opt_i(t.year),
            'd' => opt_f(t.secs),
            'r' => pretty_bitrate(
                t.bitrate.map(|b| b as f64),
                Some(if t.vbr { 1 } else { 0 }),
                Some(&t.content_type),
                &cfg.string_kbps,
            ),
            'T' => t.samplerate.map(|x| json!(x)).unwrap_or(Value::Null),
            'I' => t.samplesize.map(|x| json!(x)).unwrap_or(Value::Null),
            'H' => t.channels.map(|x| json!(x)).unwrap_or(Value::Null),
            'F' => Value::Null,
            'm' => opt_i(t.bpm),
            'v' => opt_s(&t.tagversion),
            'M' => Value::Null,
            'Q' => json!(if t.lossless { 1 } else { 0 }),
            'w' => opt_s(&t.lyrics),
            'R' => match t.rating {
                Some(r) if r > 0 => json!(r),
                _ => Value::Null,
            },
            'O' => t.playcount.map(|x| json!(x)).unwrap_or(Value::Null),
            'Y' => opt_f(t.replay_gain),
            'c' => opt_s(&t.coverid),
            'K' => match &t.cover_value {
                Some(cv) if cv.starts_with("http") => json!(proxied_image(cv)),
                _ => Value::Null,
            },
            'E' => Value::Null,
            'g' => opt_s(&t.genre),
            'p' => opt_i(t.genre_id),
            '4' => t
                .artist_id
                .and_then(|a| {
                    c.query_row("SELECT portrait_id FROM artists WHERE id = ?", [a], |r| {
                        r.get::<_, Option<String>>(0)
                    })
                    .ok()
                    .flatten()
                })
                .map(|x| json!(x))
                .unwrap_or(Value::Null),
            'q' => album_row
                .as_ref()
                .and_then(|a| a.0)
                .map(|x| json!(x))
                .unwrap_or(Value::Null),
            'J' => album_row
                .as_ref()
                .and_then(|a| a.1.clone())
                .map(|x| json!(x))
                .unwrap_or(Value::Null),
            'C' => album_row
                .as_ref()
                .and_then(|a| a.2)
                .map(|x| json!(x))
                .unwrap_or(Value::Null),
            'W' => {
                if cfg.ignore_release_types {
                    json!("ALBUM")
                } else {
                    album_row
                        .as_ref()
                        .and_then(|a| a.3.clone())
                        .map(|x| json!(x))
                        .unwrap_or(Value::Null)
                }
            }
            'X' => album_row
                .as_ref()
                .and_then(|a| a.4)
                .map(num_json)
                .unwrap_or(Value::Null),
            'G' | 'P' => {
                let ids = tag == 'P';
                let vals: Vec<String> = c
                    .prepare_cached(
                        "SELECT genres.id, genres.name FROM track_genres JOIN genres ON genres.id = track_genres.genre_id WHERE track_id = ? ORDER BY track_genres.genre_id",
                    )
                    .and_then(|mut st| {
                        let rows = st.query_map([t.id], |r| Ok((r.get::<_, i64>(0)?, get_text(r, 1)?.unwrap_or_default())))?;
                        Ok(rows.filter_map(|r| r.ok()).map(|(i, n)| if ids { i.to_string() } else { n }).collect())
                    })
                    .unwrap_or_default();
                if vals.is_empty() {
                    Value::Null
                } else {
                    json!(vals.join(", "))
                }
            }
            'k' => opt_s(&t.comment),
            _ => continue,
        };
        if is_present(&value) {
            out.insert(fname.to_string(), value);
        }
    }
    out
}

pub fn num_json(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// `Slim::Web::ImageProxy::proxiedImage`
pub fn proxied_image(u: &str) -> String {
    if !u.starts_with("http") {
        return u.to_string();
    }
    let lower = u.to_ascii_lowercase();
    let ext = [".jpg", ".jpeg", ".png", ".gif"]
        .iter()
        .find(|e| lower.contains(*e))
        .map(|e| if *e == ".jpeg" { ".jpg" } else { e })
        .unwrap_or(".png");
    format!(
        "/imageproxy/{}/image{}",
        crate::text::uri_escape_utf8(u),
        ext
    )
}
