//! LMS CLI library queries (`Slim::Control::Queries`): albums, artists, genres, years, titles,
//! search, musicfolder, playlists, playlists tracks, songinfo, roles, works, libraries.

use std::path::{Path, PathBuf};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::config::{role_to_type, type_to_role, LibraryConfig, CONTRIBUTOR_ROLES};
use crate::songdata::{self, get_text, normalize, sql_to_json, TagDataArgs};
use crate::text::{self, TextRules};
use crate::{LibQuery, LibQueryName, Library, Row, Totals, Track, TrackFilter};

const COLLATE: &str = "COLLATE perllocale";

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("bad params: {0}")]
    BadParams(String),
}

fn bad(msg: &str) -> anyhow::Error {
    QueryError::BadParams(msg.to_string()).into()
}

/// `Slim::Utils::Misc::specified`: defined and not containing `*`.
pub fn specified(s: &str) -> bool {
    !s.contains('*')
}

/// Id of the Various Artists artist (`variousArtistsObject`), looked up by search key.
pub fn va_id_cfg(c: &Connection, cfg: &LibraryConfig) -> Option<i64> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let search = rules.search_key(&cfg.various_artists_string);
    c.query_row(
        "SELECT id FROM artists WHERE name_search = ? LIMIT 1",
        [search],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// Multi-word search (each word must match at a word start). Returns (where clause, params, order expr).
pub fn search_clause(
    rules: &TextRules,
    cfg: &LibraryConfig,
    search: &str,
    col: &str,
    title_col: &str,
) -> (String, Vec<String>, String) {
    let norm = rules.normalize_search(search);
    let words: Vec<String> = norm.split_whitespace().map(|s| s.to_string()).collect();
    let words = if words.is_empty() {
        vec![norm.clone()]
    } else {
        words
    };
    let mut clauses = Vec::new();
    let mut title_clauses = Vec::new();
    let mut params = Vec::new();
    let colx = format!("COALESCE({}, {})", col, title_col);
    for w in &words {
        if cfg.search_substring {
            clauses.push(format!("{} LIKE ?", colx));
            params.push(format!("%{}%", w));
            title_clauses.push(format!("{} LIKE '%{}%'", title_col, w.replace('\'', "''")));
        } else {
            clauses.push(format!("({} LIKE ? OR {} LIKE ?)", colx, colx));
            params.push(format!("{}%", w));
            params.push(format!("% {}%", w));
            let e = w.replace('\'', "''");
            title_clauses.push(format!(
                "({t} LIKE '{e}%' OR {t} LIKE '% {e}%')",
                t = title_col,
                e = e
            ));
        }
    }
    let order = format!(
        "(CASE WHEN {} THEN 0 ELSE 1 END)",
        title_clauses.join(" AND ")
    );
    (format!("({})", clauses.join(" AND ")), params, order)
}

fn get<'a>(q: &'a LibQuery, k: &str) -> Option<&'a str> {
    q.params.get(k).map(|s| s.as_str())
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

pub(crate) fn run(lib: &Library, q_in: &LibQuery) -> anyhow::Result<Row> {
    let cfg = lib.config();
    let c = lib.conn()?;
    let scanning = lib.is_scanning();
    // Virtual libraries are not supported: like `VirtualLibraries->getRealId` for an unknown
    // id, `library_id` is ignored.
    let q_owned;
    let mut q = q_in;
    if q_in.params.contains_key("library_id") {
        let mut o = q_in.clone();
        o.params.shift_remove("library_id");
        q_owned = o;
        q = &q_owned;
    }
    let mut out = match q.name {
        LibQueryName::Albums => albums(&c, &cfg, q, scanning)?,
        LibQueryName::Artists => artists(&c, &cfg, q, scanning)?,
        LibQueryName::Genres => genres(&c, &cfg, q, scanning)?,
        LibQueryName::Years => years(&c, q, scanning)?,
        LibQueryName::Titles => titles(&c, &cfg, q, scanning)?,
        LibQueryName::Search => search(&c, &cfg, q, scanning)?,
        LibQueryName::MusicFolder => {
            drop(c);
            musicfolder(lib, &cfg, q, scanning)?
        }
        LibQueryName::Playlists => playlists(&c, &cfg, q, scanning)?,
        LibQueryName::PlaylistTracks => playlist_tracks(&c, &cfg, q, scanning)?,
        LibQueryName::SongInfo => songinfo(&c, &cfg, q, scanning)?,
        LibQueryName::Roles => roles(&c, &cfg, q, scanning)?,
        LibQueryName::Works => works(&c, &cfg, q, scanning)?,
        LibQueryName::Libraries => Row::new(),
    };
    // keep `count` numeric
    if let Some(Value::String(s)) = out.get("count") {
        if let Ok(n) = s.parse::<i64>() {
            out.insert("count".into(), json!(n));
        }
    }
    Ok(out)
}

fn count_sql(c: &Connection, sql: &str, p: &[SqlValue]) -> anyhow::Result<usize> {
    let csql = format!("SELECT COUNT(1) FROM ( {} ) AS t1", sql);
    Ok(c.query_row(&csql, params_from_iter(p.iter()), |r| r.get::<_, i64>(0))? as usize)
}

/// `_createIndexList`
fn create_index_list(c: &Connection, sql: &str, p: &[SqlValue]) -> anyhow::Result<Value> {
    let mut st = c.prepare(sql)?;
    let mut rows = st.query(params_from_iter(p.iter()))?;
    let mut list: Vec<(String, i64)> = Vec::new();
    while let Some(r) = rows.next()? {
        let ch = get_text(r, 0)?.unwrap_or_default();
        match list.iter().position(|(k, _)| *k == ch) {
            Some(mut i) => {
                let is_upper = ch.len() == 1 && ch.chars().all(|x| x.is_ascii_uppercase());
                if is_upper
                    && list.iter().any(|(k, _)| {
                        k.len() == 1
                            && k.chars().all(|x| x.is_ascii_uppercase())
                            && ch.as_str() < k.as_str()
                    })
                {
                    i = list.len() - 1;
                }
                while i < list.len() - 1 {
                    let m = list.pop().unwrap();
                    list[i].1 += m.1;
                }
                list.last_mut().unwrap().1 += 1;
            }
            None => list.push((ch, 1)),
        }
    }
    Ok(Value::Array(
        list.into_iter().map(|(k, n)| json!([k, n])).collect(),
    ))
}

fn add_loop(out: &mut Row, name: &str, item: Row) {
    match out.get_mut(name) {
        Some(Value::Array(a)) => a.push(Value::Object(item)),
        _ => {
            out.insert(name.to_string(), Value::Array(vec![Value::Object(item)]));
        }
    }
}

fn fetch_map(
    r: &rusqlite::Row,
    names: &[String],
) -> rusqlite::Result<std::collections::HashMap<String, SqlValue>> {
    let mut m = std::collections::HashMap::new();
    for (i, n) in names.iter().enumerate() {
        m.insert(n.clone(), r.get::<_, SqlValue>(i)?);
    }
    Ok(m)
}

fn sv_str(v: Option<&SqlValue>) -> Option<String> {
    match v? {
        SqlValue::Null => None,
        SqlValue::Text(s) => Some(s.clone()),
        SqlValue::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        SqlValue::Integer(i) => Some(i.to_string()),
        SqlValue::Real(f) => Some(crate::cue::perl_num(*f)),
    }
}

fn sv_i64(v: Option<&SqlValue>) -> Option<i64> {
    match v? {
        SqlValue::Integer(i) => Some(*i),
        SqlValue::Real(f) => Some(*f as i64),
        SqlValue::Text(s) => s.parse().ok(),
        _ => None,
    }
}

fn sv_json(v: Option<&SqlValue>) -> Value {
    v.map(sql_to_json).unwrap_or(Value::Null)
}

/// `addResultLoopIfValueDefined`
fn put_defined(item: &mut Row, k: &str, v: Value) {
    if !v.is_null() {
        item.insert(k.to_string(), v);
    }
}

/// `Slim::Music::Info::addDiscNumberToAlbumTitle`
pub fn add_disc_number(
    title: &str,
    disc: Option<i64>,
    discc: Option<i64>,
    cfg: &LibraryConfig,
) -> String {
    let Some(d) = disc.filter(|d| *d > 0) else {
        return title.to_string();
    };
    let mut dc = discc;
    if let Some(n) = dc {
        if n == 1 {
            return title.to_string();
        }
        if n < 1 {
            dc = None;
        }
    }
    let re = regex::Regex::new(&format!(
        r"(?i)\b(({})|(Disc))\s+\d+",
        regex::escape(&cfg.string_disc)
    ))
    .unwrap();
    if re.is_match(title) {
        return title.to_string();
    }
    match dc {
        Some(n) => {
            let w = n.to_string().len();
            format!(
                "{} ({} {:>w$} {} {})",
                title,
                cfg.string_disc,
                d,
                cfg.string_of,
                n,
                w = w
            )
        }
        None => format!("{} ({} {})", title, cfg.string_disc, d),
    }
}

// ---------------------------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------------------------

fn albums(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags")
        .filter(|t| !t.is_empty())
        .unwrap_or("l")
        .to_string();
    let search = get(q, "search");
    let mut compilation = get(q, "compilation").map(|s| s.to_string());
    let contributor_id = get(q, "artist_id");
    let genre_id = get(q, "genre_id");
    let track_id = get(q, "track_id");
    let album_id = get(q, "album_id");
    let role_id = get(q, "role_id").filter(|s| !s.is_empty());
    let release_type = get(q, "release_type");
    let year = get(q, "year");
    let work = get(q, "work_id").filter(|s| !s.is_empty() && *s != "0");
    let composer_id = get(q, "composer_id");
    let from_search = get(q, "from_search");
    let mut sort = get(q, "sort")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if role_id.is_some() {
                "artistalbum".into()
            } else {
                "album".into()
            }
        });
    let valid_sorts = [
        "new",
        "playcount",
        "popular",
        "recentlyplayed",
        "changed",
        "album",
        "artflow",
        "artistalbum",
        "yearalbum",
        "yearartistalbum",
        "random",
    ];
    if !valid_sorts.contains(&sort.as_str()) {
        return Err(bad("sort"));
    }
    let mut index = q.start;
    let mut quantity = q.quantity;
    let cc = tags == "CC";
    let has = |s: &str| tags.contains(s);

    let mut from = String::from("FROM albums ");
    let mut cols: Vec<(String, String)> = vec![
        ("albums.id".into(), "albums.id".into()),
        ("albums.title_search".into(), "albums.title_search".into()),
        ("albums.title_sort".into(), "albums.title_sort".into()),
    ];
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    let mut order_by = format!("albums.title_sort {}, albums.disc", COLLATE);
    let mut limit: Option<usize> = None;
    let mut page_key: Option<String> = Some("SUBSTR(albums.title_sort,1,1)".into());
    let va = va_id_cfg(c, cfg);

    if let Some(t) = track_id {
        from.push_str("JOIN tracks ON tracks.album_id = albums.id ");
        w.push("tracks.id = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else if let Some(a) = album_id.filter(|a| !a.contains(',')) {
        w.push("albums.id = ?".into());
        p.push(SqlValue::Text(a.into()));
    } else {
        if let Some(a) = album_id {
            let ids = split_ids(a);
            w.push(format!("albums.id IN ({})", vec!["?"; ids.len()].join(",")));
            p.extend(ids.into_iter().map(SqlValue::Text));
        } else if let Some(s) = search.filter(|s| specified(s)) {
            let (clause, params, order) =
                search_clause(&rules, cfg, s, "albums.search_text", "albums.title_search");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
            if !cc {
                order_by = format!("{}, LENGTH(albums.title_search)", order);
                sort = order_by.clone();
            }
        }
        let mut roles: Vec<String> = Vec::new();
        if let Some(cid) = contributor_id {
            if cid.parse::<i64>().ok() == va && va.is_some() {
                compilation = Some("1".into());
            } else {
                from.push_str("JOIN album_artists ON album_artists.album_id = albums.id ");
                w.push("album_artists.artist_id = ?".into());
                p.push(SqlValue::Text(cid.into()));
                if let Some(r) = role_id {
                    roles = split_ids(r);
                    if r == "ALBUMARTIST" && !cfg.use_unified_artists_list {
                        roles.push("ARTIST".into());
                    }
                } else if cfg.use_unified_artists_list {
                    let mut v: Vec<String> = cfg
                        .active_contributor_roles(true)
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    for r in cfg.all_album_link_roles() {
                        if !v.iter().any(|x| x == r) {
                            v.push(r.to_string());
                        }
                    }
                    roles = v;
                } else {
                    roles = CONTRIBUTOR_ROLES
                        .iter()
                        .map(|(n, _)| n.to_string())
                        .collect();
                }
            }
        } else if let Some(r) = role_id {
            from.push_str("JOIN album_artists ON album_artists.album_id = albums.id ");
            roles = split_ids(r);
            if r == "ALBUMARTIST" && !cfg.use_unified_artists_list {
                roles.push("ARTIST".into());
            }
        }
        if !roles.is_empty() {
            let ids: Vec<i64> = roles.iter().filter_map(|r| type_to_role(r)).collect();
            w.push(format!(
                "album_artists.role IN ({})",
                vec!["?"; ids.len()].join(", ")
            ));
            p.extend(ids.into_iter().map(SqlValue::Integer));
        }
        if !cc {
            if ["new", "changed", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str())
            {
                from.push_str("JOIN tracks ON tracks.album_id = albums.id ");
                from.push_str("JOIN files ON files.id = tracks.file_id ");
                let l = if cfg.browse_age_limit > 0 {
                    cfg.browse_age_limit
                } else {
                    100
                };
                limit = Some(l);
                order_by = "MAX(files.mtime) DESC".into();
                if let Some(qq) = quantity {
                    if qq > l {
                        quantity = Some(l);
                    }
                }
                if sort != "changed" {
                    from.push_str("LEFT JOIN track_stats ON track_stats.url = tracks.url ");
                    match sort.as_str() {
                        "new" => order_by = "MIN(track_stats.added_at) DESC".into(),
                        "playcount" | "popular" => order_by = "albums_playcount DESC".into(),
                        "recentlyplayed" => order_by = "MAX(track_stats.last_played) DESC".into(),
                        _ => {}
                    }
                    if sort != "new" {
                        w.push("track_stats.last_played > 0".into());
                    }
                }
                page_key = None;
            } else if sort == "artflow" {
                order_by = format!(
                    "artists.name_sort {c}, albums.year, albums.title_sort {c}",
                    c = COLLATE
                );
                cols.push(("artists.name_sort".into(), "artists.name_sort".into()));
                page_key = Some("SUBSTR(artists.name_sort,1,1)".into());
            } else if sort == "artistalbum" {
                order_by = format!("artists.name_sort {c}, albums.title_sort {c}", c = COLLATE);
                cols.push(("artists.name_sort".into(), "artists.name_sort".into()));
                page_key = Some("SUBSTR(artists.name_sort,1,1)".into());
            } else if sort == "yearartistalbum" {
                order_by = format!(
                    "albums.year, artists.name_sort {c}, albums.title_sort {c}",
                    c = COLLATE
                );
                page_key = Some("albums.year".into());
            } else if sort == "yearalbum" {
                order_by = format!("albums.year, albums.title_sort {c}", c = COLLATE);
                page_key = Some("albums.year".into());
            } else if sort == "random" {
                let l = cfg.items_per_page.max(1);
                limit = Some(l);
                if let Some(qq) = quantity {
                    if qq > l {
                        quantity = Some(l);
                    }
                }
                order_by = "RANDOM()".into();
                page_key = None;
            }
        }
        if let Some(rt) = release_type {
            let rts: Vec<String> = split_ids(rt)
                .into_iter()
                .map(|s| s.to_uppercase())
                .collect();
            w.push(format!(
                "albums.release_type IN ({})",
                vec!["?"; rts.len()].join(", ")
            ));
            p.extend(rts.into_iter().map(SqlValue::Text));
        }
        if let Some(y) = year {
            w.push("albums.year = ?".into());
            p.push(SqlValue::Text(y.into()));
        }
        if let (Some(fs), None) = (from_search, search) {
            let (clause, params, _) =
                search_clause(&rules, cfg, fs, "albums.search_text", "albums.title_search");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
        }
        if let Some(g) = genre_id {
            let ids = split_ids(g);
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON tracks.album_id = albums.id ");
            }
            from.push_str("JOIN track_genres ON track_genres.track_id = tracks.id ");
            w.push(format!(
                "track_genres.genre_id IN ({})",
                vec!["?"; ids.len()].join(", ")
            ));
            p.extend(ids.into_iter().map(SqlValue::Text));
        }
        if let Some(comp) = &compilation {
            if comp.trim() == "1" {
                w.push("albums.compilation = 1".into());
            } else {
                w.push("(albums.compilation IS NULL OR albums.compilation = 0)".into());
            }
        }
    }
    if let Some(wk) = work {
        if !from.contains("JOIN tracks ") {
            from.push_str("JOIN tracks ON tracks.album_id = albums.id ");
        }
        from.push_str("JOIN works ON tracks.work_id = works.id ");
        from.push_str("JOIN artists AS composer ON works.composer_id = composer.id ");
        if !from.contains("JOIN track_artists ") {
            from.push_str("JOIN track_artists ON track_artists.track_id = tracks.id ");
        }
        for k in [
            "tracks.work_id",
            "works.title",
            "composer.name",
            "tracks.performance",
        ] {
            cols.push((k.into(), k.into()));
        }
        order_by.push_str(", tracks.track_number");
        if wk != "-1" {
            w.push("tracks.work_id = ?".into());
            p.push(SqlValue::Text(wk.into()));
            if let Some(cid) = composer_id {
                w.push("track_artists.artist_id = ? AND track_artists.role = 2".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
    }
    if has("l") {
        for k in ["albums.title", "albums.disc", "albums.disc_count"] {
            cols.push((k.into(), k.into()));
        }
    }
    if has("y") {
        cols.push(("albums.year".into(), "albums.year".into()));
    }
    if has("j") {
        if work.is_some() {
            cols.push(("tracks.cover_id".into(), "tracks.cover_id".into()));
        } else {
            cols.push(("albums.artwork".into(), "albums.artwork".into()));
        }
    }
    cols.push(("albums.artwork".into(), "albums.artwork".into()));
    for (t, k) in [
        ("t", "albums.title"),
        ("i", "albums.disc"),
        ("q", "albums.disc_count"),
        ("w", "albums.compilation"),
        ("X", "albums.replay_gain"),
    ] {
        if has(t) {
            cols.push((k.into(), k.into()));
        }
    }
    cols.push(("albums.title".into(), "albums.title".into()));
    cols.push(("albums.compilation".into(), "albums.compilation".into()));
    if has("W") && !cfg.ignore_release_types {
        cols.push(("albums.release_type".into(), "albums.release_type".into()));
    }
    cols.push(("albums.artist_id".into(), "albums.artist_id".into()));
    if !cc {
        from.push_str("JOIN artists ON artists.id = albums.artist_id ");
        cols.push(("artists.name".into(), "artists.name".into()));
        if has("4") && work.is_none() {
            cols.push(("artists.portrait_id".into(), "artists.portrait_id".into()));
        }
    }
    if has("2") {
        cols.push((
            "(SELECT COUNT(1) FROM (SELECT 1 FROM tracks WHERE tracks.album_id=albums.id GROUP BY work_id,grouping,performance))".into(),
            "group_count".into(),
        ));
        cols.push((
            "(SELECT GROUP_CONCAT(COALESCE(SUBSTR('00000'||disc,-5),'00001') || SUBSTR('00000'||track_number,-5) || COALESCE(work_id,'') || '##' || COALESCE(performance,'') || '##' || COALESCE(grouping,''),',,') FROM tracks WHERE tracks.album_id = albums.id)".into(),
            "group_structure".into(),
        ));
    }
    if ["playcount", "popular"].contains(&sort.as_str()) {
        cols.push((
            "AVG(track_stats.play_count)".into(),
            "albums_playcount".into(),
        ));
        if sort == "popular" {
            w.push("track_stats.last_played > ?".into());
            p.push(SqlValue::Integer(
                crate::scanner::now() - cfg.browse_popularity_max_days * 86400,
            ));
        }
    }
    // dedupe columns
    let mut seen = std::collections::HashSet::new();
    cols.retain(|(_, k)| seen.insert(k.clone()));

    let mut where_sql = String::new();
    if !w.is_empty() {
        where_sql = format!("WHERE {} ", w.join(" AND "));
    }
    let group = if work.is_some() {
        "GROUP BY tracks.performance, tracks.work_id, albums.id "
    } else {
        "GROUP BY albums.id "
    };
    let mut out = Row::new();
    if let (Some(pk), true) = (&page_key, has("Z")) {
        let isql = format!(
            "SELECT {} AS n {}{}{}ORDER BY {}",
            pk, from, where_sql, group, order_by
        );
        out.insert("indexList".into(), create_index_list(c, &isql, &p)?);
        if has("ZZ") {
            return Ok(out);
        }
    }
    let col_sql: Vec<String> = cols.iter().map(|(e, _)| e.clone()).collect();
    let names: Vec<String> = cols.iter().map(|(_, k)| k.clone()).collect();
    let mut sql = format!(
        "SELECT {} {}{}{}",
        col_sql.join(", "),
        from,
        where_sql,
        group
    );
    if !cc {
        sql.push_str(&format!("ORDER BY {} ", order_by));
    }
    let count_q = match limit {
        Some(l) => format!("{} LIMIT {}", sql, l),
        None => sql.clone(),
    };
    let count = count_sql(c, &count_q, &p)?;
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, _, _) = normalize(index, quantity, count);
    if valid && !cc {
        if limit.is_some() && quantity.is_none() {
            quantity = limit;
            index = Some(index.unwrap_or(0));
        }
        if let (Some(i), Some(qq)) = (index, quantity) {
            sql.push_str(&format!("LIMIT {}, {} ", i, qq));
        }
        let link_roles: Vec<i64> = if has("aa") || has("SS") {
            if work.is_some() {
                vec![1, 4, 3]
            } else {
                let mut lr: Vec<String> = cfg
                    .all_album_link_roles()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                if let (Some(r), None) = (role_id, contributor_id) {
                    let mut pre: Vec<String> = split_ids(r)
                        .into_iter()
                        .map(|x| {
                            x.parse::<i64>()
                                .ok()
                                .and_then(role_to_type)
                                .map(|s| s.to_string())
                                .unwrap_or(x)
                        })
                        .collect();
                    pre.extend(lr);
                    let mut u: Vec<String> = Vec::new();
                    for x in pre {
                        if !u.contains(&x) {
                            u.push(x);
                        }
                    }
                    lr = u;
                }
                lr.iter().filter_map(|r| type_to_role(r)).collect()
            }
        } else {
            Vec::new()
        };
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query(params_from_iter(p.iter()))?;
        let mut items: Vec<Row> = Vec::new();
        while let Some(r) = rows.next()? {
            let m = fetch_map(r, &names)?;
            let mut item = Row::new();
            let album_id_v = sv_i64(m.get("albums.id")).unwrap_or(0);
            let title = sv_str(m.get("albums.title")).unwrap_or_default();
            let cname = sv_str(m.get("artists.name")).unwrap_or_default();
            let work_title = sv_str(m.get("works.title"));
            let composer_name = sv_str(m.get("composer.name"));
            let perf = sv_str(m.get("tracks.performance"));
            item.insert("id".into(), json!(album_id_v));
            put_defined(&mut item, "work_id", sv_json(m.get("tracks.work_id")));
            put_defined(
                &mut item,
                "work_name",
                work_title.clone().map(Value::String).unwrap_or(Value::Null),
            );
            put_defined(
                &mut item,
                "composer",
                composer_name
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            item.insert(
                "performance".into(),
                json!(perf.clone().unwrap_or_default()),
            );
            let fav_url = if work.is_some() {
                format!(
                    "db:album.title={}&contributor.name={}&work.title={}&composer.name={}&track.performance={}",
                    text::uri_escape_utf8(&title),
                    text::uri_escape_utf8(&cname),
                    text::uri_escape_utf8(work_title.as_deref().unwrap_or("")),
                    text::uri_escape_utf8(composer_name.as_deref().unwrap_or("")),
                    text::uri_escape_utf8(perf.as_deref().unwrap_or(""))
                )
            } else {
                format!(
                    "db:album.title={}&contributor.name={}",
                    text::uri_escape_utf8(&title),
                    text::uri_escape_utf8(&cname)
                )
            };
            item.insert("favorites_url".into(), json!(fav_url));
            let fav_title = if work.is_some() {
                let mut t = composer_name
                    .as_ref()
                    .map(|c| format!("{}: ", c))
                    .unwrap_or_default();
                t.push_str(&format!("{} (", work_title.clone().unwrap_or_default()));
                if let Some(pf) = perf.as_ref().filter(|s| !s.is_empty()) {
                    t.push_str(&format!("{} ", pf));
                }
                t.push_str(&format!("from {})", title));
                t
            } else {
                title.clone()
            };
            item.insert("favorites_title".into(), json!(fav_title));
            if has("l") {
                let t = if cfg.group_discs {
                    title.clone()
                } else {
                    add_disc_number(
                        &title,
                        sv_i64(m.get("albums.disc")),
                        sv_i64(m.get("albums.disc_count")),
                        cfg,
                    )
                };
                item.insert("album".into(), json!(t));
            }
            if has("y") {
                put_defined(&mut item, "year", sv_json(m.get("albums.year")));
            }
            let artwork = sv_str(m.get("albums.artwork"));
            if has("j") {
                let cover = if work.is_some() {
                    sv_str(m.get("tracks.cover_id"))
                        .filter(|s| !s.is_empty())
                        .or(artwork.clone())
                } else {
                    artwork.clone()
                };
                if !cover.as_deref().unwrap_or("").starts_with("http") {
                    put_defined(
                        &mut item,
                        "artwork_track_id",
                        cover.map(Value::String).unwrap_or(Value::Null),
                    );
                }
            }
            if has("K") && artwork.as_deref().unwrap_or("").starts_with("http") {
                put_defined(&mut item, "artwork_url", json!(artwork.clone().unwrap()));
            }
            if has("t") {
                item.insert("title".into(), json!(title));
            }
            if has("i") {
                put_defined(&mut item, "disc", sv_json(m.get("albums.disc")));
            }
            if has("q") {
                put_defined(&mut item, "disccount", sv_json(m.get("albums.disc_count")));
            }
            if has("w") {
                put_defined(
                    &mut item,
                    "compilation",
                    sv_json(m.get("albums.compilation")),
                );
            }
            if has("W") {
                let v = if cfg.ignore_release_types {
                    json!("ALBUM")
                } else {
                    sv_json(m.get("albums.release_type"))
                };
                put_defined(&mut item, "release_type", v);
            }
            if has("X") {
                put_defined(
                    &mut item,
                    "album_replay_gain",
                    sv_json(m.get("albums.replay_gain")),
                );
            }
            if has("2") {
                let gc = sv_i64(m.get("group_count")).unwrap_or(0);
                let mut non_contig = false;
                if gc > 1 {
                    let structure = sv_str(m.get("group_structure")).unwrap_or_default();
                    let mut parts: Vec<&str> = structure.split(",,").collect();
                    parts.sort();
                    let mut prev: Option<String> = None;
                    let mut seen_g = std::collections::HashSet::new();
                    for part in parts {
                        let g = part.get(10..).unwrap_or("").to_string();
                        if prev.as_deref() != Some(g.as_str()) {
                            if seen_g.contains(&g) && g != "####" {
                                non_contig = true;
                                break;
                            }
                            seen_g.insert(g.clone());
                            prev = Some(g);
                        }
                    }
                }
                item.insert("group_count".into(), json!(gc));
                item.insert(
                    "contiguous_groups".into(),
                    json!(if non_contig { 0 } else { 1 }),
                );
            }
            let album_contrib = sv_i64(m.get("albums.artist_id"));
            if work.is_none() {
                if has("S") {
                    put_defined(
                        &mut item,
                        "artist_id",
                        album_contrib.map(|x| json!(x)).unwrap_or(Value::Null),
                    );
                }
                if has("a") {
                    put_defined(&mut item, "artist", json!(cname));
                }
                if has("4") {
                    put_defined(
                        &mut item,
                        "portraitid",
                        sv_json(m.get("artists.portrait_id")),
                    );
                }
            }
            if has("s") {
                let tk = if sort == "artflow" || sort == "artistalbum" {
                    Some(text::first_char(
                        &sv_str(m.get("artists.name_sort")).unwrap_or_default(),
                    ))
                } else if sort == "album" {
                    Some(text::first_char(
                        &sv_str(m.get("albums.title_sort")).unwrap_or_default(),
                    ))
                } else {
                    None
                };
                if let Some(t) = tk {
                    item.insert("textkey".into(), json!(t));
                }
            }
            let is_comp = sv_i64(m.get("albums.compilation")).unwrap_or(0) != 0;
            if !link_roles.is_empty() && album_contrib != va && !is_comp {
                let (names_v, ids_v) = album_link_contributors(
                    c,
                    album_id_v,
                    &link_roles,
                    work,
                    perf.as_deref(),
                    if work.is_none() {
                        Some((cname.clone(), album_contrib))
                    } else {
                        None
                    },
                )?;
                if has("aa") && !names_v.is_empty() {
                    if work.is_some() {
                        item.insert("artist".into(), json!(names_v[0]));
                    }
                    item.insert("artists".into(), json!(names_v.join(",")));
                }
                if has("SS") && !ids_v.is_empty() {
                    if work.is_some() {
                        item.insert("artist_id".into(), json!(ids_v[0]));
                    }
                    item.insert(
                        "artist_ids".into(),
                        json!(ids_v
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")),
                    );
                }
            }
            if has("R") {
                let mut st2 = if contributor_id.is_some() {
                    c.prepare_cached(
                        "SELECT role FROM album_artists WHERE album_id = ? AND artist_id = ?",
                    )?
                } else {
                    c.prepare_cached("SELECT role FROM album_artists WHERE album_id = ?")?
                };
                let roles: Vec<String> = if let Some(cid) = contributor_id {
                    st2.query_map(rusqlite::params![album_id_v, cid], |r| r.get::<_, i64>(0))?
                        .filter_map(|r| r.ok())
                        .map(|x| x.to_string())
                        .collect()
                } else {
                    st2.query_map([album_id_v], |r| r.get::<_, i64>(0))?
                        .filter_map(|r| r.ok())
                        .map(|x| x.to_string())
                        .collect()
                };
                item.insert("role_ids".into(), json!(roles.join(",")));
            }
            items.push(item);
        }
        for it in items {
            add_loop(&mut out, "albums_loop", it);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

#[allow(clippy::type_complexity)]
fn album_link_contributors(
    c: &Connection,
    album: i64,
    roles: &[i64],
    work: Option<&str>,
    performance: Option<&str>,
    main: Option<(String, Option<i64>)>,
) -> anyhow::Result<(Vec<String>, Vec<i64>)> {
    let rl = roles
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows: Vec<(i64, String, i64)> = if let Some(wk) = work {
        let sql = format!(
            "SELECT track_artists.role, artists.name, artists.id FROM tracks
             JOIN track_artists ON tracks.id = track_artists.track_id
             JOIN artists ON artists.id = track_artists.artist_id
             WHERE tracks.album_id = ?1 AND tracks.work_id = ?2 AND ((?3 IS NULL AND tracks.performance IS NULL) OR tracks.performance = ?3)
               AND track_artists.role IN ({})
             GROUP BY track_artists.role, artists.name, artists.id
             ORDER BY track_artists.role, artists.name_sort",
            rl
        );
        let mut st = c.prepare_cached(&sql)?;
        let perf: Option<String> = performance.filter(|s| !s.is_empty()).map(|s| s.to_string());
        let v = st
            .query_map(rusqlite::params![album, wk, perf], |r| {
                Ok((r.get(0)?, get_text(r, 1)?.unwrap_or_default(), r.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        v
    } else {
        let sql = format!(
            "SELECT album_artists.role, artists.name, artists.id FROM album_artists
             JOIN artists ON artists.id = album_artists.artist_id
             WHERE album_artists.album_id = ? AND album_artists.role IN ({})
             ORDER BY album_artists.role, artists.name_sort {}",
            rl, COLLATE
        );
        let mut st = c.prepare_cached(&sql)?;
        let v = st
            .query_map([album], |r| {
                Ok((r.get(0)?, get_text(r, 1)?.unwrap_or_default(), r.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let mut names = Vec::new();
    let mut ids = Vec::new();
    for role in roles {
        for (r, n, i) in &rows {
            if r == role {
                names.push(n.clone());
                ids.push(*i);
            }
        }
    }
    if let Some((n, i)) = main {
        if !n.is_empty() {
            names.insert(0, n);
        }
        if let Some(i) = i {
            ids.insert(0, i);
        }
    }
    let mut un: Vec<String> = Vec::new();
    for n in names {
        if !un.contains(&n) {
            un.push(n);
        }
    }
    let mut ui: Vec<i64> = Vec::new();
    for i in ids {
        if !ui.contains(&i) {
            ui.push(i);
        }
    }
    Ok((un, ui))
}

// ---------------------------------------------------------------------------------------------
// artists
// ---------------------------------------------------------------------------------------------

fn artists(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let mut index = q.start;
    let mut quantity = q.quantity;
    let mut sort = get(q, "sort")
        .filter(|s| !s.is_empty())
        .unwrap_or("artist")
        .to_string();
    let search = get(q, "search").filter(|s| !s.is_empty());
    let year = get(q, "year");
    let genre_id = get(q, "genre_id");
    let track_id = get(q, "track_id");
    let album_id = get(q, "album_id");
    let artist_id = get(q, "artist_id");
    let mut role_id = get(q, "role_id")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let work_id = get(q, "work_id");
    let tags = get(q, "tags").unwrap_or("").to_string();
    if !["artist", "new", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str()) {
        return Err(bad("sort"));
    }
    let aa_merge = role_id.as_deref() == Some("ALBUMARTIST") && !cfg.use_unified_artists_list;
    let va_pref = cfg.various_artist_auto_identification && cfg.use_unified_artists_list;
    let va = va_id_cfg(c, cfg);

    let mut from = String::from("FROM artists ");
    let mut sql_va = String::from("SELECT COUNT(*) FROM albums ");
    let mut w: Vec<String> = Vec::new();
    let mut w_va: Vec<String> = vec!["albums.compilation = 1".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    let mut p_va: Vec<SqlValue> = Vec::new();
    let mut order_by = format!("artists.name_sort {}, artists.mbid", COLLATE);
    let use_stats = ["new", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str());
    if use_stats {
        match sort.as_str() {
            "new" => order_by = format!("MIN(track_stats.added_at) DESC, {}", order_by),
            "playcount" | "popular" => {
                order_by = format!(
                    "SUM(track_stats.play_count) DESC, MIN(files.mtime), {}",
                    order_by
                )
            }
            _ => order_by = format!("MAX(track_stats.last_played) DESC, {}", order_by),
        }
        let l = if cfg.browse_age_limit > 0 {
            cfg.browse_age_limit
        } else {
            100
        };
        quantity = Some(quantity.unwrap_or(l).min(l));
    }
    let tc = |from: &mut String| {
        if !from.contains("JOIN track_artists ") {
            from.push_str("JOIN track_artists ON track_artists.artist_id = artists.id ");
        }
    };
    if let Some(t) = track_id {
        tc(&mut from);
        w.push("track_artists.track_id = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else if let Some(a) = artist_id {
        w.push("artists.id = ?".into());
        p.push(SqlValue::Text(a.into()));
    } else {
        let roles: Vec<i64> = if let Some(r) = &mut role_id {
            if aa_merge {
                r.push_str(",ARTIST");
            }
            split_ids(r)
                .iter()
                .filter_map(|x| type_to_role(x))
                .collect()
        } else if cfg.use_unified_artists_list {
            cfg.active_contributor_roles(false)
                .iter()
                .filter_map(|r| type_to_role(r))
                .collect()
        } else {
            CONTRIBUTOR_ROLES.iter().map(|(_, i)| *i).collect()
        };
        let join_tracks = |from: &mut String| {
            tc(from);
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON tracks.id = track_artists.track_id ");
            }
        };
        if let Some(g) = genre_id {
            let ids = split_ids(g);
            join_tracks(&mut from);
            from.push_str("JOIN track_genres ON track_genres.track_id = tracks.id ");
            w.push(format!(
                "track_genres.genre_id IN ({})",
                vec!["?"; ids.len()].join(", ")
            ));
            p.extend(ids.into_iter().map(SqlValue::Text));
            sql_va.push_str("JOIN tracks ON tracks.album_id = albums.id JOIN track_genres ON track_genres.track_id = tracks.id ");
            w_va.push("track_genres.genre_id = ?".into());
            p_va.push(SqlValue::Text(g.into()));
        }
        if let Some(wk) = work_id {
            join_tracks(&mut from);
            if wk == "-1" {
                w.push("tracks.work_id IS NOT NULL".into());
            } else {
                w.push("tracks.work_id = ?".into());
                p.push(SqlValue::Text(wk.into()));
            }
        }
        if use_stats {
            join_tracks(&mut from);
            from.push_str("JOIN files ON files.id = tracks.file_id ");
            from.push_str("LEFT JOIN track_stats ON track_stats.url = tracks.url ");
            if sort == "popular" {
                w.push("track_stats.last_played > ?".into());
                p.push(SqlValue::Integer(
                    crate::scanner::now() - cfg.browse_popularity_max_days * 86400,
                ));
            } else if sort != "new" {
                w.push("track_stats.last_played > 0".into());
            }
        }
        if search.is_none() && !from.contains("JOIN track_artists ") {
            from.push_str("JOIN album_artists ON album_artists.artist_id = artists.id ");
        }
        let role_list = roles
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(",");
        if from.contains("JOIN track_artists ") {
            w.push(format!(
                "(track_artists.role IN ({}) {})",
                role_list,
                if search.is_some() {
                    "OR artists.id = ? "
                } else {
                    ""
                }
            ));
        } else {
            if !from.contains("JOIN album_artists ") {
                from.push_str(&format!(
                    "{}JOIN album_artists ON album_artists.artist_id = artists.id ",
                    if search.is_some() { "LEFT " } else { "" }
                ));
            }
            w.push(format!(
                "(album_artists.role IN ({}) {})",
                role_list,
                if search.is_some() {
                    "OR artists.id = ? "
                } else {
                    ""
                }
            ));
        }
        if search.is_some() {
            p.push(va.map(SqlValue::Integer).unwrap_or(SqlValue::Integer(-1)));
        }
        if va_pref || aa_merge {
            if from.contains("JOIN tracks ") {
                from.push_str("JOIN albums ON albums.id = tracks.album_id ");
            } else {
                if !from.contains("JOIN album_artists ") {
                    from.push_str("JOIN album_artists ON album_artists.artist_id = artists.id ");
                }
                from.push_str("JOIN albums ON album_artists.album_id = albums.id ");
            }
            let extra = if va_pref {
                String::new()
            } else {
                format!(" OR artists.id = {}", va.unwrap_or(-1))
            };
            w.push(format!(
                "(albums.compilation IS NULL OR albums.compilation = 0{})",
                extra
            ));
        }
        if album_id.is_some() || year.is_some() {
            if !from.contains("JOIN album_artists ") {
                from.push_str("JOIN album_artists ON album_artists.artist_id = artists.id ");
            }
            if !from.contains("JOIN albums ") {
                from.push_str("JOIN albums ON album_artists.album_id = albums.id ");
            }
            if let Some(a) = album_id {
                w.push("albums.id = ?".into());
                p.push(SqlValue::Text(a.into()));
                w_va.push("albums.id = ?".into());
                p_va.push(SqlValue::Text(a.into()));
            }
            if let Some(y) = year {
                w.push("albums.year = ?".into());
                p.push(SqlValue::Text(y.into()));
                w_va.push("albums.year = ?".into());
                p_va.push(SqlValue::Text(y.into()));
            }
        }
        if let Some(s) = search {
            let (clause, params, order) =
                search_clause(&rules, cfg, s, "artists.name_search", "artists.name_search");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
            if tags != "CC" {
                order_by = format!("{}, LENGTH(artists.name), {}", order, order_by);
            }
        }
    }
    let mut where_sql = String::new();
    if !w.is_empty() {
        where_sql = format!("WHERE {} ", w.join(" AND "));
    }
    let mut count_va = 0i64;
    if va_pref
        && search.is_none()
        && track_id.is_none()
        && artist_id.is_none()
        && role_id.is_none()
        && !use_stats
    {
        let s = format!("{}WHERE {} ", sql_va, w_va.join(" AND "));
        count_va = c.query_row(&s, params_from_iter(p_va.iter()), |r| r.get(0))?;
    }
    let mut out = Row::new();
    let mut index_list: Option<Value> = None;
    if tags.contains('Z') && !use_stats {
        let isql = format!("SELECT SUBSTR(artists.name_sort,1,1) {}{}GROUP BY artists.id ORDER BY artists.name_sort {}", from, where_sql, COLLATE);
        let mut il = create_index_list(c, &isql, &p)?;
        if count_va > 0 {
            if let Value::Array(a) = &mut il {
                a.insert(0, json!(["#", 1]));
            }
        }
        if tags.contains("ZZ") {
            out.insert("indexList".into(), il);
            return Ok(out);
        }
        index_list = Some(il);
    }
    let mut cols = vec!["artists.id", "artists.name", "artists.name_sort"];
    if tags.contains('4') {
        cols.push("artists.portrait_id");
    }
    let mut sql = format!(
        "SELECT {} {}{}GROUP BY artists.id ",
        cols.join(", "),
        from,
        where_sql
    );
    if tags != "CC" {
        sql.push_str(&format!("ORDER BY {} ", order_by));
    }
    let mut count = count_sql(c, &sql, &p)? as i64;
    let mut total_count = count;
    let mut add_va = false;
    if count_va > 0 {
        add_va = index.unwrap_or(0) == 0;
        // _fixCount
        total_count = count + 1;
        if index.unwrap_or(0) == 0 {
            quantity = quantity.map(|q| q.saturating_sub(1));
        } else {
            index = index.map(|i| i - 1);
        }
    }
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    if add_va && count == 0 {
        count = 1;
    }
    let (mut valid, _, _) = normalize(index, quantity, count as usize);
    if add_va && index.unwrap_or(0) == 0 && quantity == Some(0) {
        valid = true;
    }
    if valid && tags != "CC" {
        if let (Some(i), Some(qq)) = (index, quantity) {
            sql.push_str(&format!("LIMIT {}, {} ", i, qq));
        }
        let mut emit =
            |id: i64, name: String, name_sort: String, portrait: Option<String>, first: bool| {
                let mut item = Row::new();
                item.insert("id".into(), json!(id));
                item.insert("artist".into(), json!(name));
                if !use_stats && tags.contains('s') {
                    let tk = if add_va && first {
                        " ".to_string()
                    } else {
                        text::first_char(&name_sort)
                    };
                    item.insert("textkey".into(), json!(tk));
                }
                if tags.contains('4') {
                    if let Some(pid) = portrait.filter(|e| !e.is_empty()) {
                        item.insert("portraitid".into(), json!(pid));
                    }
                }
                item.insert(
                    "favorites_url".into(),
                    json!(format!(
                        "db:contributor.name={}",
                        text::uri_escape_utf8(&name)
                    )),
                );
                add_loop(&mut out, "artists_loop", item);
            };
        let mut first = true;
        if add_va {
            if let Some(vid) = va {
                let (name, ns): (String, String) = c.query_row(
                    "SELECT name, name_sort FROM artists WHERE id = ?",
                    [vid],
                    |r| {
                        Ok((
                            get_text(r, 0)?.unwrap_or_default(),
                            get_text(r, 1)?.unwrap_or_default(),
                        ))
                    },
                )?;
                emit(vid, name, ns, None, true);
                first = false;
            }
        }
        if quantity != Some(0) {
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query(params_from_iter(p.iter()))?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let name = get_text(r, 1)?.unwrap_or_default();
                let ns = get_text(r, 2)?.unwrap_or_default();
                let portrait = if tags.contains('4') {
                    get_text(r, 3)?
                } else {
                    None
                };
                emit(id, name, ns, portrait, first && !add_va);
                first = false;
            }
        }
    }
    if let Some(il) = index_list {
        out.insert("indexList".into(), il);
    }
    out.insert("count".into(), json!(total_count));
    let _ = &mut sort;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// genres / years / roles
// ---------------------------------------------------------------------------------------------

fn genres(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").unwrap_or("").to_string();
    let mut from = String::from("FROM genres ");
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = get(q, "search").filter(|s| !s.is_empty() && specified(s)) {
        let (clause, params, _) =
            search_clause(&rules, cfg, s, "genres.name_search", "genres.name_search");
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
    }
    let va = va_id_cfg(c, cfg);
    if let Some(t) = get(q, "track_id") {
        from.push_str("JOIN track_genres ON genres.id = track_genres.genre_id ");
        w.push("track_genres.track_id = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else if let Some(g) = get(q, "genre_id") {
        let ids = split_ids(g);
        w.push(format!(
            "genres.id IN ({})",
            vec!["?"; ids.len()].join(", ")
        ));
        p.extend(ids.into_iter().map(SqlValue::Text));
    } else {
        if let Some(cid) = get(q, "artist_id") {
            if cid.parse::<i64>().ok() == va && va.is_some() {
                from.push_str("JOIN track_genres ON genres.id = track_genres.genre_id JOIN tracks ON track_genres.track_id = tracks.id JOIN albums ON tracks.album_id = albums.id ");
                w.push("albums.compilation = ?".into());
                p.push(SqlValue::Integer(1));
            } else {
                from.push_str("JOIN track_genres ON genres.id = track_genres.genre_id JOIN track_artists ON track_genres.track_id = track_artists.track_id ");
                w.push("track_artists.artist_id = ?".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
        let album = get(q, "album_id");
        let year = get(q, "year");
        let work = get(q, "work_id");
        if album.is_some() || year.is_some() || work.is_some() {
            if !from.contains("JOIN track_genres ") {
                from.push_str("JOIN track_genres ON genres.id = track_genres.genre_id ");
            }
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON track_genres.track_id = tracks.id ");
            }
            if let Some(a) = album {
                w.push("tracks.album_id = ?".into());
                p.push(SqlValue::Text(a.into()));
            }
            if let Some(y) = year {
                w.push("tracks.year = ?".into());
                p.push(SqlValue::Text(y.into()));
            }
            if let Some(wk) = work {
                if wk == "-1" {
                    w.push("tracks.work_id IS NOT NULL".into());
                } else {
                    w.push("tracks.work_id = ?".into());
                    p.push(SqlValue::Text(wk.into()));
                }
            }
        }
    }
    let where_sql = if w.is_empty() {
        String::new()
    } else {
        format!("WHERE {} ", w.join(" AND "))
    };
    let mut out = Row::new();
    if tags.contains('Z') {
        let isql = format!(
            "SELECT SUBSTR(genres.name_sort,1,1) {}{}ORDER BY genres.name_sort {}",
            from, where_sql, COLLATE
        );
        out.insert("indexList".into(), create_index_list(c, &isql, &p)?);
        if tags.contains("ZZ") {
            return Ok(out);
        }
    }
    let mut sql = format!(
        "SELECT DISTINCT(genres.id), genres.name, genres.name_sort {}{}",
        from, where_sql
    );
    if tags != "CC" {
        sql.push_str(&format!("ORDER BY genres.name_sort {}", COLLATE));
    }
    let count = count_sql(c, &sql, &p)?;
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, _, _) = normalize(q.start, q.quantity, count);
    if valid && tags != "CC" {
        if let (Some(i), Some(qq)) = (q.start, q.quantity) {
            sql.push_str(&format!(" LIMIT {}, {} ", i, qq));
        }
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query(params_from_iter(p.iter()))?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let name = get_text(r, 1)?.unwrap_or_default();
            let ns = get_text(r, 2)?.unwrap_or_default();
            let mut item = Row::new();
            item.insert("id".into(), json!(id));
            item.insert("genre".into(), json!(name));
            item.insert(
                "favorites_url".into(),
                json!(format!("db:genre.name={}", text::uri_escape_utf8(&name))),
            );
            if tags.contains('s') {
                item.insert("textkey".into(), json!(text::first_char(&ns)));
            }
            add_loop(&mut out, "genres_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

fn years(c: &Connection, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let has_albums = get(q, "hasAlbums")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false);
    let (key, table) = if has_albums {
        ("albums.year", "albums")
    } else {
        ("tracks.year", "tracks")
    };
    let mut sql = format!("SELECT DISTINCT {} FROM {} ", key, table);
    let mut w = vec![format!("{} != 0", key)];
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(y) = get(q, "year") {
        w.push(format!("{} = ?", key));
        p.push(SqlValue::Text(y.into()));
    }
    sql.push_str(&format!(
        "WHERE {} AND {} IS NOT NULL ",
        w.join(" AND "),
        key
    ));
    let count = count_sql(c, &sql, &p)?;
    sql.push_str(&format!("ORDER BY {} DESC", key));
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, _, _) = normalize(q.start, q.quantity, count);
    if valid {
        if let (Some(i), Some(qq)) = (q.start, q.quantity) {
            sql.push_str(&format!(" LIMIT {}, {} ", i, qq));
        }
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query(params_from_iter(p.iter()))?;
        while let Some(r) = rows.next()? {
            let y: i64 = r.get::<_, Option<i64>>(0)?.unwrap_or(0);
            let mut item = Row::new();
            item.insert("year".into(), json!(y));
            item.insert("favorites_url".into(), json!(format!("db:year.id={}", y)));
            add_loop(&mut out, "years_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

fn roles(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let tags = get(q, "tags").unwrap_or("").to_string();
    let mut from = String::from("FROM artists ");
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    let track = get(q, "track_id");
    if let Some(t) = track {
        from.push_str("JOIN track_artists ON artists.id = track_artists.artist_id ");
        w.push("track_artists.track_id = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else {
        from.push_str("JOIN album_artists ON artists.id = album_artists.artist_id ");
        if let Some(cid) = get(q, "artist_id") {
            let va: Option<i64> = va_id_cfg(c, cfg);
            if cid.parse::<i64>().ok() == va && va.is_some() {
                from.push_str("JOIN albums ON album_artists.album_id = albums.id ");
                w.push("albums.compilation = ?".into());
                p.push(SqlValue::Integer(1));
            } else {
                w.push("album_artists.artist_id = ?".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
        if let Some(a) = get(q, "album_id") {
            w.push("album_artists.album_id = ?".into());
            p.push(SqlValue::Text(a.into()));
        }
        let year = get(q, "year");
        let work = get(q, "work_id");
        if year.is_some() || work.is_some() {
            from.push_str("JOIN track_artists ON artists.id = track_artists.artist_id JOIN tracks ON tracks.id = track_artists.track_id ");
            if let Some(y) = year {
                w.push("tracks.year = ?".into());
                p.push(SqlValue::Text(y.into()));
            }
            if let Some(wk) = work {
                if wk == "-1" {
                    w.push("tracks.work_id IS NOT NULL".into());
                } else {
                    w.push("tracks.work_id = ?".into());
                    p.push(SqlValue::Text(wk.into()));
                }
            }
        }
    }
    let col = if track.is_some() {
        "DISTINCT track_artists.role"
    } else {
        "DISTINCT album_artists.role"
    };
    let mut sql = format!(
        "SELECT {} {}{}",
        col,
        from,
        if w.is_empty() {
            String::new()
        } else {
            format!("WHERE {} ", w.join(" AND "))
        }
    );
    let count = count_sql(c, &sql, &p)?;
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, _, _) = normalize(q.start, q.quantity, count);
    if valid && tags != "CC" {
        if let (Some(i), Some(qq)) = (q.start, q.quantity) {
            sql.push_str(&format!("LIMIT {}, {} ", i, qq));
        }
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query(params_from_iter(p.iter()))?;
        while let Some(r) = rows.next()? {
            let role: i64 = r.get(0)?;
            let mut item = Row::new();
            item.insert("role_id".into(), json!(role));
            if tags.contains('t') {
                item.insert(
                    "role_name".into(),
                    role_to_type(role).map(|s| json!(s)).unwrap_or(Value::Null),
                );
            }
            add_loop(&mut out, "roles_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// works
// ---------------------------------------------------------------------------------------------

fn works(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").unwrap_or("").to_string();
    let artist_id = get(q, "artist_id");
    let from = "FROM tracks
        JOIN track_artists composer_track ON composer_track.track_id = tracks.id AND composer_track.role = 2
        JOIN artists composer ON composer.id = composer_track.artist_id
        JOIN track_artists ON track_artists.track_id = tracks.id
        JOIN artists ON artists.id = track_artists.artist_id
        JOIN works ON works.id = tracks.work_id AND works.composer_id = composer.id
        JOIN albums ON tracks.album_id = albums.id ";
    let mut from = from.to_string();
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = get(q, "search").filter(|s| !s.is_empty() && specified(s)) {
        let (c1, p1, _) = search_clause(&rules, cfg, s, "works.title_search", "works.title_search");
        let (c2, p2, _) =
            search_clause(&rules, cfg, s, "albums.search_text", "albums.title_search");
        w.push(format!("({} OR {})", c1, c2));
        p.extend(p1.into_iter().map(SqlValue::Text));
        p.extend(p2.into_iter().map(SqlValue::Text));
    } else {
        w.push("tracks.work_id IS NOT NULL".into());
        if let Some(wk) = get(q, "work_id").filter(|s| *s != "-1") {
            let ids = split_ids(wk);
            if !ids.is_empty() {
                w.push(format!("works.id IN ({})", vec!["?"; ids.len()].join(", ")));
                p.extend(ids.into_iter().map(SqlValue::Text));
            }
        }
        if let Some(a) = get(q, "album_id") {
            let ids: Vec<String> = split_ids(a)
                .into_iter()
                .filter(|x| x.chars().all(|c| c.is_ascii_digit()))
                .collect();
            w.push(format!(
                "albums.id IN ({})",
                if ids.is_empty() {
                    "-1".into()
                } else {
                    ids.join(",")
                }
            ));
        }
        if let Some(y) = get(q, "year") {
            w.push("tracks.year = ?".into());
            p.push(SqlValue::Text(y.into()));
        }
        if let Some(r) = get(q, "role_id") {
            let ids: Vec<i64> = split_ids(r)
                .iter()
                .filter_map(|x| type_to_role(x))
                .collect();
            if !ids.is_empty() {
                w.push(format!(
                    "track_artists.role IN ({})",
                    vec!["?"; ids.len()].join(", ")
                ));
                p.extend(ids.into_iter().map(SqlValue::Integer));
            }
        }
        if let Some(a) = artist_id {
            w.push("artists.id = ?".into());
            p.push(SqlValue::Text(a.into()));
        }
        if let Some(g) = get(q, "genre_id") {
            let ids = split_ids(g);
            from.push_str("JOIN track_genres ON track_genres.track_id = tracks.id ");
            w.push(format!(
                "track_genres.genre_id IN ({})",
                vec!["?"; ids.len()].join(", ")
            ));
            p.extend(ids.into_iter().map(SqlValue::Text));
        }
    }
    w.push(
        "NOT EXISTS (SELECT 1 FROM tracks t2 WHERE tracks.album_id=t2.album_id AND tracks.work_id=t2.work_id
         AND (tracks.performance IS NULL AND t2.performance IS NULL OR tracks.performance=t2.performance) AND tracks.id > t2.id)"
            .into(),
    );
    let base = format!("{}WHERE {} ", from, w.join(" AND "));
    let composer_count = count_sql(c, &format!("SELECT 'c' {} GROUP BY composer.id", base), &p)?;
    let grouped = format!("{} GROUP BY tracks.work_id, composer.id ", base);
    let count = count_sql(c, &format!("SELECT 'c' {}", grouped), &p)?;
    let order_by = format!(
        "ORDER BY composer.name_sort {c} , works.title_sort {c}",
        c = COLLATE
    );
    let mut out = Row::new();
    if tags.contains('Z') {
        let pk = if composer_count == 1 {
            "SUBSTR(works.title_sort,1,1)"
        } else {
            "SUBSTR(composer.name_sort,1,1)"
        };
        out.insert(
            "indexList".into(),
            create_index_list(
                c,
                &format!("SELECT {} AS n {} {}", pk, grouped, order_by),
                &p,
            )?,
        );
        if tags.contains("ZZ") {
            return Ok(out);
        }
    }
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, _, _) = normalize(q.start, q.quantity, count);
    if valid && tags != "CC" {
        let mut sql = format!(
            "SELECT works.title, works.id, composer.name, composer.id, composer.name_sort, works.title_sort, GROUP_CONCAT(DISTINCT tracks.cover_id), GROUP_CONCAT(DISTINCT albums.id) {} {}",
            grouped, order_by
        );
        if let (Some(i), Some(qq)) = (q.start, q.quantity) {
            sql.push_str(&format!(" LIMIT {}, {} ", i, qq));
        }
        let mut st = c.prepare(&sql)?;
        let mut rows = st.query(params_from_iter(p.iter()))?;
        while let Some(r) = rows.next()? {
            let work = get_text(r, 0)?.unwrap_or_default();
            let work_id: i64 = r.get(1)?;
            let composer = get_text(r, 2)?.unwrap_or_default();
            let composer_id: i64 = r.get(3)?;
            let ns = get_text(r, 4)?.unwrap_or_default();
            let ts = get_text(r, 5)?.unwrap_or_default();
            let images = get_text(r, 6)?;
            let album_ids = get_text(r, 7)?.unwrap_or_default();
            let mut item = Row::new();
            item.insert("work_id".into(), json!(work_id));
            if composer_count == 1
                && artist_id
                    .map(|a| a == composer_id.to_string())
                    .unwrap_or(false)
            {
                item.insert("single_composer".into(), json!(1));
            }
            item.insert("composer".into(), json!(composer));
            item.insert("work".into(), json!(work));
            item.insert("composer_id".into(), json!(composer_id));
            if let Some(img) = &images {
                if let Some(first) = img.split(',').next() {
                    item.insert("artwork_track_id".into(), json!(first));
                }
                item.insert("artwork_track_ids".into(), json!(img));
            }
            item.insert("album_id".into(), json!(album_ids));
            item.insert(
                "textkey".into(),
                json!(text::first_char(if composer_count == 1 {
                    &ts
                } else {
                    &ns
                })),
            );
            item.insert(
                "favorites_url".into(),
                json!(format!(
                    "db:work.title={}&composer.name={}",
                    text::uri_escape_utf8(&work),
                    text::uri_escape_utf8(&composer)
                )),
            );
            item.insert(
                "favorites_title".into(),
                json!(format!("{}: {}", composer, work)),
            );
            add_loop(&mut out, "works_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// titles / songinfo / playlists tracks
// ---------------------------------------------------------------------------------------------

fn titles(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let mut tags = get(q, "tags")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "gald".into());
    let sort = get(q, "sort")
        .filter(|s| !s.is_empty())
        .unwrap_or("title")
        .to_string();
    let search = get(q, "search").map(|s| s.to_string());
    let ignore_work = get(q, "ignore_work_tracks")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false);
    let where_base = ignore_work.then(|| "tracks.work_id IS NULL".to_string());
    let mut order_by = format!("tracks.title_sort {}", COLLATE);
    if sort == "tracknum" {
        tags.push('t');
        order_by = format!(
            "tracks.disc, tracks.track_number, tracks.title_sort {}",
            COLLATE
        );
    } else if sort == "albumtrack" {
        tags.push_str("tl");
        order_by = format!(
            "albums.title_sort, tracks.disc, tracks.track_number, tracks.title_sort {}",
            COLLATE
        );
    } else if sort == "title"
        && (tags.contains('a') || tags.contains('s'))
        && (tags.contains('e') || tags.contains('l'))
    {
        order_by = format!(
            "tracks.title_sort, artists.name_sort, albums.title_sort, tracks.year {}",
            COLLATE
        );
    }
    let args = TagDataArgs {
        where_base,
        sort: Some(order_by),
        search,
        album_id: get(q, "album_id").map(|s| s.to_string()),
        year: get(q, "year").map(|s| s.to_string()),
        genre_id: get(q, "genre_id").map(|s| s.to_string()),
        contributor_id: get(q, "artist_id").map(|s| s.to_string()),
        track_id: get(q, "track_id").map(|s| s.to_string()),
        role_id: get(q, "role_id").map(|s| s.to_string()),
        release_type: get(q, "release_type").map(|s| s.to_string()),
        work_id: get(q, "work_id").map(|s| s.to_string()),
        only_album_years: get(q, "only_album_years")
            .map(|s| !s.is_empty() && s != "0")
            .unwrap_or(false),
        performance: get(q, "performance").map(|s| s.to_string()),
        paged: true,
        index: q.start,
        quantity: q.quantity,
        track_ids: None,
    };
    let res = songdata::tag_data(c, cfg, &tags, &args)?;
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    for mut h in res.rows {
        let row = songdata::song_data_from_hash(&mut h, &tags, cfg);
        add_loop(&mut out, "titles_loop", row);
    }
    out.insert("count".into(), json!(res.total));
    if let Some(h) = res.album_header {
        out.insert("album_header".into(), h);
    }
    Ok(out)
}

fn songinfo(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let tags = get(q, "tags")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "abcdefghijJklmnopqrstvwxyzBCDEFHIJKLMNOQRTUVWXY".into());
    let track_id = get(q, "track_id");
    let u = get(q, "url");
    if track_id.is_none() && u.is_none() {
        return Err(bad("url or track_id required"));
    }
    let track = if let Some(id) = track_id {
        id.parse::<i64>()
            .ok()
            .and_then(|id| crate::track::load_by_id(c, id))
    } else {
        crate::track::load_by_url(c, u.unwrap())
    };
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    if let Some(t) = track {
        let h = songdata::object_row(c, cfg, &t, &tags);
        let count = h.len();
        let (valid, start, end) = normalize(q.start, q.quantity, count);
        if valid {
            let mut lp = Vec::new();
            for (i, (k, v)) in h.into_iter().enumerate() {
                if i >= start && i <= end {
                    let mut item = Row::new();
                    item.insert(k, v);
                    lp.push(Value::Object(item));
                }
            }
            out.insert("songinfo_loop".into(), Value::Array(lp));
        }
    }
    Ok(out)
}

/// Tracks of a playlist (resolved to DB tracks where possible).
fn playlist_track_list(
    c: &Connection,
    playlist_id: i64,
) -> anyhow::Result<Option<(String, Vec<Track>)>> {
    let title: Option<String> = c
        .query_row(
            "SELECT name FROM playlists WHERE id = ?",
            [playlist_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(title) = title else {
        return Ok(None);
    };
    let urls = crate::playlist_urls(c, playlist_id)?;
    let mut tracks = Vec::new();
    for u in urls {
        if let Some(t) = crate::track::load_by_url(c, &u) {
            tracks.push(t);
        } else if crate::url::is_remote_url(&u) {
            // remote entries: a pseudo track (negative ids are reserved for the server's RemoteTrack)
            tracks.push(Track {
                id: 0,
                url: u.clone(),
                title: crate::url::file_name(&u),
                remote: true,
                content_type: crate::types::type_from_path(&u).to_string(),
                ..Default::default()
            });
        }
    }
    Ok(Some((title, tracks)))
}

fn playlist_tracks(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let tags = get(q, "tags")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "gald".into());
    let Some(pid) = get(q, "playlist_id") else {
        return Err(bad("playlist_id required"));
    };
    let pid: i64 = pid.parse().unwrap_or(-1);
    let mut out = Row::new();
    let list = playlist_track_list(c, pid)?;
    if let Some((title, _)) = &list {
        if !title.is_empty() {
            out.insert("__playlistTitle".into(), json!(title));
        }
    }
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    match list {
        Some((_, tracks)) => {
            let count = tracks.len();
            let (valid, start, end) = normalize(q.start, q.quantity, count);
            if valid {
                for (i, t) in tracks.iter().enumerate().take(end + 1).skip(start) {
                    let mut row = Row::new();
                    row.insert("playlist index".into(), json!(i));
                    let data = if t.id > 0 {
                        songdata::object_row(c, cfg, t, &tags)
                    } else {
                        songdata::minimal_row(t)
                    };
                    for (k, v) in data {
                        row.insert(k, v);
                    }
                    add_loop(&mut out, "playlisttracks_loop", row);
                }
            }
            out.insert("count".into(), json!(count));
        }
        None => {
            out.insert("count".into(), json!(0));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// playlists
// ---------------------------------------------------------------------------------------------

fn playlists(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").unwrap_or("").to_string();
    let search = get(q, "search").filter(|s| !s.is_empty());
    let folder = get(q, "folder_id").unwrap_or("");
    let mut folders: Vec<(String, String)> = Vec::new();
    let mut w: Vec<String> = vec!["playlists.kind = 'saved'".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = search {
        let (clause, params, _) = search_clause(
            &rules,
            cfg,
            s,
            "playlists.name_search",
            "playlists.name_search",
        );
        w.insert(0, clause);
        p.extend(params.into_iter().map(SqlValue::Text));
    } else if !folder.is_empty() {
        let mut f = folder.to_string();
        if !f.starts_with("file:/") && crate::re!(r"%[0-9A-Fa-f]{2}").is_match(&f) {
            f = String::from_utf8_lossy(&crate::url::percent_decode_bytes(&f)).into_owned();
        }
        if f.starts_with("file:/") {
            f = crate::url::path_from_file_url(&f)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        if f.is_empty() || f == "/" || f == "file:" {
            f = cfg
                .playlist_dir
                .as_ref()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        let mut furl = crate::url::file_url_from_path(Path::new(&f));
        furl.push('/');
        // sub folders
        let mut st = c.prepare(
            "SELECT url FROM playlists WHERE url LIKE ? ESCAPE '\\' AND kind = 'saved' AND instr(substr(url, ?), '/') > 0",
        )?;
        let urls: Vec<String> = st
            .query_map(
                rusqlite::params![
                    format!("{}%", crate::scanner::like_escape(&furl)),
                    furl.len() as i64 + 1
                ],
                |r| r.get::<_, String>(0),
            )?
            .filter_map(|r| r.ok())
            .collect();
        let mut names: Vec<String> = Vec::new();
        for u in urls {
            let rel = &u[furl.len()..];
            if let Some(i) = rel.find('/') {
                let n = rel[..i].to_string();
                if !names.contains(&n) {
                    names.push(n);
                }
            }
        }
        for n in names {
            let name = String::from_utf8_lossy(&crate::url::percent_decode_bytes(&n)).into_owned();
            folders.push((format!("{}{}", furl, n), name));
        }
        folders.sort_by(|a, b| a.1.cmp(&b.1));
        w.push(
            "playlists.url LIKE ? ESCAPE '\\' AND instr(substr(playlists.url, ?), '/') = 0".into(),
        );
        p.push(SqlValue::Text(format!(
            "{}%",
            crate::scanner::like_escape(&furl)
        )));
        p.push(SqlValue::Integer(furl.len() as i64 + 1));
    }
    let sql = format!(
        "SELECT playlists.id, playlists.name, playlists.url, playlists.name_sort FROM playlists WHERE {} ORDER BY playlists.name_sort {}",
        w.join(" AND "),
        COLLATE
    );
    let pl_count = count_sql(c, &sql, &p)?;
    let folder_count = folders.len();
    let count = folder_count + pl_count;
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let (valid, start, end) = normalize(q.start, q.quantity, count);
    if valid {
        let mut chunk = 0usize;
        if start < folder_count {
            while chunk + start <= end && start + chunk < folder_count {
                let (u, name) = &folders[start + chunk];
                let mut item = Row::new();
                item.insert("id".into(), json!(u));
                item.insert("playlist".into(), json!(name));
                if tags.contains('u') {
                    item.insert("url".into(), json!(u));
                }
                if tags.contains('s') {
                    item.insert("textkey".into(), json!(text::first_char(name)));
                }
                add_loop(&mut out, "playlists_loop", item);
                chunk += 1;
            }
        }
        let (ps, pe): (i64, i64) = if end as i64 >= folder_count as i64 {
            (
                (start as i64 - folder_count as i64).max(0),
                end as i64 - folder_count as i64,
            )
        } else {
            (0, -1)
        };
        if pe >= 0 {
            let lsql = format!("{} LIMIT {}, {}", sql, ps, pe - ps + 1);
            let mut st = c.prepare(&lsql)?;
            let mut rows = st.query(params_from_iter(p.iter()))?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let title = get_text(r, 1)?.unwrap_or_default();
                let u: String = r.get(2)?;
                let ts = get_text(r, 3)?.unwrap_or_default();
                let mut item = Row::new();
                item.insert("id".into(), json!(id));
                item.insert("playlist".into(), json!(title));
                if tags.contains('u') {
                    item.insert("url".into(), json!(u));
                    item.insert("favorites_url".into(), json!(u));
                }
                if tags.contains('s') {
                    item.insert("textkey".into(), json!(text::first_char(&ts)));
                }
                if tags.contains('x') {
                    item.insert("remote".into(), json!(0));
                }
                add_loop(&mut out, "playlists_loop", item);
            }
        }
        out.insert("count".into(), json!(count));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------------------------

fn search(
    c: &Connection,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let term = get(q, "term").map(text::match_case).unwrap_or_default();
    if term.is_empty() {
        return Err(bad("term required"));
    }
    let extended = get(q, "extended")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false);
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let mut total = 0usize;
    // (result type, table, name column, search column, title search column, extended extras
    // as (expression, result key))
    #[allow(clippy::type_complexity)]
    let types: [(&str, &str, &str, &str, &str, Vec<(&str, &str)>); 5] = [
        (
            "contributor",
            "artists",
            "name",
            "me.name_search",
            "me.name_search",
            vec![],
        ),
        (
            "album",
            "albums",
            "title",
            "me.search_text",
            "me.title_search",
            vec![("me.artwork", "artwork")],
        ),
        (
            "work",
            "works",
            "title",
            "me.title_search",
            "me.title_search",
            vec![],
        ),
        (
            "genre",
            "genres",
            "name",
            "me.name_search",
            "me.name_search",
            vec![],
        ),
        (
            "track",
            "tracks",
            "title",
            "me.search_text",
            "me.title_search",
            vec![("me.cover_id", "coverid"), ("1", "audio")],
        ),
    ];
    for (ty, table, name, col, title_col, extra) in types.iter() {
        let mut cols = format!("me.id, me.{}", name);
        if extended && !extra.is_empty() {
            let e: Vec<&str> = extra.iter().map(|(x, _)| *x).collect();
            cols = format!("{}, {}", cols, e.join(", "));
        }
        let from = format!("FROM {} me ", table);
        let mut w: Vec<String> = Vec::new();
        let mut p: Vec<SqlValue> = Vec::new();
        let (clause, params, order) = search_clause(&rules, cfg, &term, col, title_col);
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
        let mut sql = format!("SELECT {} {}WHERE {} ", cols, from, w.join(" AND "));
        let count = count_sql(c, &sql, &p)?;
        total += count;
        let (valid, _, _) = normalize(q.start, q.quantity, count);
        if valid {
            out.insert(format!("{}s_count", ty), json!(count));
            let qty = q.quantity.unwrap_or(count);
            sql.push_str(&format!(
                "ORDER BY {}, me.id LIMIT {}, {}",
                order,
                q.start.unwrap_or(0),
                qty
            ));
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query(params_from_iter(p.iter()))?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let title = get_text(r, 1)?.unwrap_or_default();
                let mut item = Row::new();
                item.insert(format!("{}_id", ty), json!(id));
                item.insert(ty.to_string(), json!(title));
                if extended {
                    for (i, (_, key)) in extra.iter().enumerate() {
                        let v: SqlValue = r.get(2 + i)?;
                        item.insert(key.to_string(), sql_to_json(&v));
                    }
                }
                add_loop(&mut out, &format!("{}s_loop", ty), item);
            }
        }
    }
    out.insert("count".into(), json!(total));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// musicfolder
// ---------------------------------------------------------------------------------------------

fn list_dir(dir: &Path, cfg: &LibraryConfig, recursive: bool) -> Vec<PathBuf> {
    let ignore_re = cfg
        .ignore_dir_re
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| regex::Regex::new(s).ok());
    let valid = |p: &Path| -> bool {
        if p.is_dir() {
            return true;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        crate::types::is_audio_ext(&ext, &cfg.disabled_extensions_audio)
            || crate::types::is_playlist_ext(&ext, &cfg.disabled_extensions_playlist)
    };
    let mut out = Vec::new();
    if recursive {
        let mut it = walkdir::WalkDir::new(dir)
            .follow_links(true)
            .min_depth(1)
            .sort_by(|a, b| {
                a.file_name()
                    .to_ascii_lowercase()
                    .cmp(&b.file_name().to_ascii_lowercase())
            })
            .into_iter();
        while let Some(e) = it.next() {
            let Ok(e) = e else { continue };
            let name = e.file_name().to_string_lossy().to_string();
            if !crate::scanner::name_allowed(&name, ignore_re.as_ref()) {
                if e.file_type().is_dir() {
                    it.skip_current_dir();
                }
                continue;
            }
            if e.file_type().is_file() && valid(e.path()) {
                out.push(e.path().to_path_buf());
            }
        }
        return out;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !crate::scanner::name_allowed(&name, ignore_re.as_ref()) {
            continue;
        }
        let p = e.path();
        if let Some(s) = cfg.skip_sentinel.as_ref().filter(|s| !s.is_empty()) {
            if p.is_dir() && p.join(s).exists() {
                continue;
            }
        }
        if valid(&p) {
            out.push(p);
        }
    }
    out.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    out
}

fn musicfolder(
    lib: &Library,
    cfg: &LibraryConfig,
    q: &LibQuery,
    scanning: bool,
) -> anyhow::Result<Row> {
    let mut tags = get(q, "tags").unwrap_or("").to_string();
    let ty = get(q, "type").unwrap_or("");
    if !ty.is_empty() && ty != "audio" {
        tags = tags.replace('d', "");
    }
    let folder_id = get(q, "folder_id").filter(|s| !s.is_empty());
    let want_top = get(q, "return_top")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false);
    let url_param = get(q, "url").filter(|s| !s.is_empty());
    let recursive = get(q, "recursive")
        .map(|s| !s.is_empty() && s != "0")
        .unwrap_or(false);
    let media_dirs: Vec<PathBuf> = if ty.is_empty() || ty == "audio" {
        cfg.media_dirs.clone()
    } else {
        Vec::new()
    };

    let mut items: Vec<FolderItem> = Vec::new();
    if url_param.is_none() && folder_id.is_none() && media_dirs.len() > 1 {
        for d in &media_dirs {
            if let Some(t) = lib.ensure_item(&crate::url::file_url_from_path(d)) {
                items.push(t);
            }
        }
    } else {
        let top: Option<FolderItem> = if let Some(u) = url_param {
            let u = u.replacen("tmp://", "file://", 1);
            let u = if crate::url::is_file_url(&u) {
                u
            } else {
                crate::url::file_url_from_path(Path::new(&u))
            };
            lib.ensure_item(&u)
        } else if let Some(fid) = folder_id {
            fid.parse::<i64>().ok().and_then(|id| lib.folder_item(id))
        } else if let Some(d) = media_dirs.first() {
            lib.ensure_item(&crate::url::file_url_from_path(d))
        } else {
            None
        };
        if let Some(top) = top {
            if want_top {
                items.push(top);
            } else if let FolderItem::Dir { id, url, mtime } = &top {
                if let Some(path) = crate::url::path_from_file_url(url).filter(|p| p.is_dir()) {
                    if !scanning {
                        lib.refresh_dir(*id, *mtime, &path);
                    }
                    for p in list_dir(&path, cfg, recursive) {
                        let u = crate::url::file_url_from_path(&p);
                        if let Some(t) = lib.ensure_item(&u) {
                            items.push(t);
                        }
                    }
                }
            }
        }
    }
    let count = items.len();
    let mut out = Row::new();
    let (valid, start, end) = normalize(q.start, q.quantity, count);
    if valid {
        for it in items.iter().take(end + 1).skip(start) {
            let mut item = Row::new();
            let u = it.url();
            let filename = crate::url::file_name(u);
            item.insert("id".into(), json!(it.id()));
            item.insert("filename".into(), json!(filename));
            let (ty, ct) = match it {
                FolderItem::Dir { .. } => ("folder", "dir".to_string()),
                FolderItem::Playlist { .. } => {
                    if tags.contains('u') {
                        item.insert("favorites_url".into(), json!(u));
                    }
                    ("playlist", crate::types::type_from_path(u).to_string())
                }
                FolderItem::Track(t) => ("track", t.content_type.clone()),
                // split into tracks by a cue sheet, or unreadable
                FolderItem::File { .. } => ("unknown", crate::types::type_from_path(u).to_string()),
            };
            item.insert("type".into(), json!(ty));
            let track = match it {
                FolderItem::Track(t) => Some(t),
                _ => None,
            };
            if tags.contains('c') {
                item.insert(
                    "coverid".into(),
                    track
                        .and_then(|t| t.coverid.clone())
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
            if tags.contains('d') {
                item.insert(
                    "duration".into(),
                    track
                        .and_then(|t| t.duration_str())
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
            if tags.contains('s') {
                item.insert(
                    "textkey".into(),
                    json!(text::first_char(&filename).to_uppercase()),
                );
            }
            if tags.contains('u') {
                item.insert("url".into(), json!(u));
            }
            if tags.contains('t') {
                item.insert("title".into(), json!(filename));
            }
            if tags.contains('o') {
                item.insert("ct".into(), json!(ct));
            }
            add_loop(&mut out, "folder_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

/// An entry of a music folder.
#[derive(Debug, Clone)]
pub enum FolderItem {
    /// a directory (`files` id)
    Dir { id: i64, url: String, mtime: i64 },
    /// a playlist or cue sheet file (`playlists` id)
    Playlist { id: i64, url: String },
    /// an audio file that is one track
    Track(Box<Track>),
    /// an audio file split into tracks by a cue sheet, or one that could not be read
    /// (`files` id)
    File { id: i64, url: String },
}

impl FolderItem {
    pub fn id(&self) -> i64 {
        match self {
            FolderItem::Dir { id, .. }
            | FolderItem::Playlist { id, .. }
            | FolderItem::File { id, .. } => *id,
            FolderItem::Track(t) => t.id,
        }
    }

    pub fn url(&self) -> &str {
        match self {
            FolderItem::Dir { url, .. }
            | FolderItem::Playlist { url, .. }
            | FolderItem::File { url, .. } => url,
            FolderItem::Track(t) => &t.url,
        }
    }
}

impl Library {
    /// What is at `u` (a directory, playlist or audio file), reading it into the library
    /// first if it is not there yet (`Slim::Schema->objectForUrl({create => 1, readTags => 1})`).
    pub fn ensure_item(&self, u: &str) -> Option<FolderItem> {
        let path = crate::url::path_from_file_url(u)?;
        let u = crate::url::file_url_from_path(&path);
        let c = self.conn().ok()?;
        if let Some(item) = item_at(&c, &u) {
            return Some(item);
        }
        let cfg = self.config();
        if path.is_dir() {
            let (mt, _, _) = crate::scanner::file_stat(&path).unwrap_or((0, 0, true));
            let id = crate::scanner::upsert_file(&c, &u, "dir", None, mt, 0).ok()?;
            return Some(FolderItem::Dir {
                id,
                url: u,
                mtime: mt,
            });
        }
        if !path.is_file() {
            return None;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_audio = crate::types::is_audio_ext(&ext, &cfg.disabled_extensions_audio);
        let is_list = crate::types::is_playlist_ext(&ext, &cfg.disabled_extensions_playlist);
        if !is_audio && !is_list {
            return None;
        }
        let abort = std::sync::atomic::AtomicBool::new(false);
        let mut sc = crate::scanner::Scanner::new(&c, &cfg, &abort, self.inner.progress.clone());
        if sc.begin().is_err() {
            return None;
        }
        let res = if ext.eq_ignore_ascii_case("cue") {
            sc.scan_cue_file(&u, &path)
        } else if is_list {
            sc.scan_playlist_file(&u, &path)
        } else {
            sc.scan_audio_file(&u, &path, None).map(|_| ())
        };
        if res.is_ok() {
            let _ = sc.post_process(false, false);
        }
        let _ = sc.commit();
        drop(sc);
        item_at(&c, &u)
    }

    /// The music folder item with the given `files` id.
    pub fn folder_item(&self, file_id: i64) -> Option<FolderItem> {
        let c = self.conn().ok()?;
        let u: String = c
            .query_row("SELECT url FROM files WHERE id = ?", [file_id], |r| {
                r.get(0)
            })
            .ok()?;
        item_at(&c, &u)
    }

    /// `findAndScanDirectoryTree`: quick non-recursive rescan when the directory mtime changed.
    pub(crate) fn refresh_dir(&self, id: i64, known_mtime: i64, path: &Path) {
        let Some((mt, _, _)) = crate::scanner::file_stat(path) else {
            return;
        };
        if known_mtime == mt {
            return;
        }
        let Ok(c) = self.conn() else { return };
        let _ = c.execute(
            "UPDATE files SET mtime = ? WHERE id = ?",
            rusqlite::params![mt, id],
        );
        let Some(_guard) = self.inner.scan_lock.try_lock() else {
            return;
        };
        let cfg = self.config();
        let abort = std::sync::atomic::AtomicBool::new(false);
        let progress = std::sync::Arc::new(parking_lot::Mutex::new(crate::ScanProgress::default()));
        let mut sc = crate::scanner::Scanner::new(&c, &cfg, &abort, progress);
        if sc.scan_audio_root(path, false, false).is_ok() && sc.changes > 0 {
            let _ = sc.post_process(false, false);
        }
        let _ = sc.commit();
    }
}

/// The item stored for the file URL `u`, if any.
fn item_at(c: &Connection, u: &str) -> Option<FolderItem> {
    let file = crate::scanner::file_row(c, u).ok().flatten()?;
    match file.kind.as_str() {
        "dir" => {
            let mtime: i64 = c
                .query_row("SELECT mtime FROM files WHERE id = ?", [file.id], |r| {
                    r.get(0)
                })
                .ok()?;
            Some(FolderItem::Dir {
                id: file.id,
                url: u.to_string(),
                mtime,
            })
        }
        "cue" | "playlist" => {
            let id: i64 = c
                .query_row(
                    "SELECT id FROM playlists WHERE file_id = ?",
                    [file.id],
                    |r| r.get(0),
                )
                .ok()?;
            Some(FolderItem::Playlist {
                id,
                url: u.to_string(),
            })
        }
        _ => match file.cue {
            None => match crate::track::load_by_url(c, u) {
                Some(t) => Some(FolderItem::Track(Box::new(t))),
                None => Some(FolderItem::File {
                    id: file.id,
                    url: u.to_string(),
                }),
            },
            Some(_) => Some(FolderItem::File {
                id: file.id,
                url: u.to_string(),
            }),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// totals / tracks_for
// ---------------------------------------------------------------------------------------------

pub(crate) fn totals(lib: &Library) -> anyhow::Result<Totals> {
    let c = lib.conn()?;
    let cfg = lib.config();
    let q = |name: LibQueryName| LibQuery::new(name, 0, 1).param("tags", "CC");
    let cnt = |r: &Row| r.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
    let albums = cnt(&albums(&c, &cfg, &q(LibQueryName::Albums), false)?);
    let artists = cnt(&artists(&c, &cfg, &q(LibQueryName::Artists), false)?);
    let genres = cnt(&genres(&c, &cfg, &q(LibQueryName::Genres), false)?);
    let songs = cnt(&titles(&c, &cfg, &q(LibQueryName::Titles), false)?);
    let playlists = cnt(&playlists(&c, &cfg, &q(LibQueryName::Playlists), false)?);
    let duration: f64 = c.query_row("SELECT COALESCE(SUM(duration), 0) FROM tracks", [], |r| {
        r.get(0)
    })?;
    Ok(Totals {
        albums,
        artists,
        genres,
        songs,
        duration,
        playlists,
    })
}

pub(crate) fn tracks_for(lib: &Library, f: &TrackFilter) -> anyhow::Result<Vec<Track>> {
    let c = lib.conn()?;
    let cfg = lib.config();
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    if !f.track_ids.is_empty() {
        return Ok(f
            .track_ids
            .iter()
            .filter_map(|id| crate::track::load_by_id(&c, *id))
            .collect());
    }
    if let Some(pid) = f.playlist_id {
        return Ok(playlist_track_list(&c, pid)?
            .map(|(_, t)| t)
            .unwrap_or_default());
    }
    if let Some(fid) = f.folder_id {
        drop(c);
        let Some(top) = lib.folder_item(fid) else {
            return Ok(Vec::new());
        };
        let c = lib.conn()?;
        let path = match &top {
            FolderItem::Dir { url, .. } => crate::url::path_from_file_url(url),
            FolderItem::Track(t) => return Ok(vec![(**t).clone()]),
            FolderItem::File { id, .. } => return file_tracks(&c, *id),
            FolderItem::Playlist { .. } => return Ok(Vec::new()),
        };
        let Some(path) = path.filter(|p| p.is_dir()) else {
            return Ok(Vec::new());
        };
        drop(c);
        let mut out = Vec::new();
        for p in list_dir(&path, &cfg, true) {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "cue" || !crate::types::is_audio_ext(&ext, &cfg.disabled_extensions_audio) {
                continue;
            }
            match lib.ensure_item(&crate::url::file_url_from_path(&p)) {
                Some(FolderItem::Track(t)) => out.push(*t),
                // split by a cue sheet: its tracks
                Some(FolderItem::File { id, .. }) => {
                    let c = lib.conn()?;
                    out.extend(file_tracks(&c, id)?)
                }
                _ => {}
            }
        }
        return Ok(out);
    }
    let album_sort = format!(
        "albums.title_sort {c}, tracks.disc, tracks.track_number, tracks.title_sort {c}",
        c = COLLATE
    );
    let album_year_sort = format!("albums.year, {}", album_sort);
    let track_sort = format!(
        "tracks.disc, tracks.track_number, tracks.title_sort {}",
        COLLATE
    );
    let mut sort: Option<String> = None;
    let mut from = String::from("FROM tracks LEFT JOIN albums ON albums.id = tracks.album_id ");
    let mut w: Vec<String> = vec!["1".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    let va = va_id_cfg(&c, &cfg);
    let mut single_album = false;
    let mut compilation = false;
    if let Some(a) = &f.artist_id {
        if Some(*a) == va {
            compilation = true;
        } else {
            from.push_str("JOIN track_artists ON track_artists.track_id = tracks.id ");
            w.push("track_artists.artist_id = ?".into());
            p.push(SqlValue::Integer(*a));
            if let Some(r) = &f.role_id {
                let mut roles = split_ids(r);
                if (r == "ALBUMARTIST" || r == "5") && !cfg.use_unified_artists_list {
                    roles.push("ARTIST".into());
                }
                let ids: Vec<i64> = roles.iter().filter_map(|x| type_to_role(x)).collect();
                w.push(format!(
                    "track_artists.role IN ({})",
                    ids.iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
        }
        sort = Some(album_sort.clone());
    } else if let Some(r) = &f.role_id {
        from.push_str("JOIN track_artists ON track_artists.track_id = tracks.id ");
        let ids: Vec<i64> = split_ids(r)
            .iter()
            .filter_map(|x| type_to_role(x))
            .collect();
        w.push(format!(
            "track_artists.role IN ({})",
            ids.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if compilation {
        w.push("albums.compilation = 1".into());
    }
    if let Some(g) = &f.genre_id {
        let ids = split_ids(g);
        from.push_str("JOIN track_genres ON track_genres.track_id = tracks.id ");
        w.push(format!(
            "track_genres.genre_id IN ({})",
            vec!["?"; ids.len()].join(",")
        ));
        p.extend(ids.into_iter().map(SqlValue::Text));
        sort = Some(album_sort.clone());
    }
    if let Some(a) = &f.album_id {
        let ids = split_ids(a);
        single_album = ids.len() == 1;
        w.push(format!(
            "tracks.album_id IN ({})",
            vec!["?"; ids.len()].join(",")
        ));
        p.extend(ids.into_iter().map(SqlValue::Text));
        sort = Some(album_sort.clone());
    }
    if let Some(y) = f.year {
        w.push("tracks.year = ?".into());
        p.push(SqlValue::Integer(y));
        sort = Some(album_sort.clone());
    }
    if let Some(wk) = f.work_id {
        w.push("tracks.work_id = ?".into());
        p.push(SqlValue::Integer(wk));
        sort = Some(album_sort.clone());
    }
    if let Some(rt) = &f.release_type {
        let rts: Vec<String> = split_ids(rt)
            .into_iter()
            .map(|s| s.to_uppercase())
            .collect();
        w.push(format!(
            "albums.release_type IN ({})",
            vec!["?"; rts.len()].join(",")
        ));
        p.extend(rts.into_iter().map(SqlValue::Text));
    }
    if let Some(s) = f.search.as_ref().filter(|s| !s.is_empty()) {
        let (clause, params, order) =
            search_clause(&rules, &cfg, s, "tracks.search_text", "tracks.title_search");
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
        if sort.is_none() {
            sort = Some(format!("{}, tracks.title_sort {}", order, COLLATE));
        }
    }
    if let Some(s) = &f.sort {
        match s.as_str() {
            "artflow" | "yearartistalbum" | "yearalbum" => {
                if sort.as_deref() == Some(album_sort.as_str()) {
                    sort = Some(album_year_sort.clone());
                }
            }
            "album" => sort = Some(album_sort.clone()),
            "artistalbum" | "albumtrack" | "new" | "random" => {}
            other => {
                if other == "tracknum" {
                    sort = Some(track_sort.clone());
                } else if other == "title" {
                    sort = Some(format!("tracks.title_sort {}", COLLATE));
                }
            }
        }
    }
    if single_album && sort.as_deref() == Some(album_sort.as_str()) {
        sort = None;
    }
    let order = sort.unwrap_or(track_sort);
    let sql = format!(
        "SELECT tracks.id {}WHERE {} GROUP BY tracks.id ORDER BY {}",
        from,
        w.join(" AND "),
        order
    );
    let mut st = c.prepare(&sql)?;
    let ids: Vec<i64> = st
        .query_map(params_from_iter(p.iter()), |r| r.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(st);
    Ok(ids
        .into_iter()
        .filter_map(|id| crate::track::load_by_id(&c, id))
        .collect())
}

/// The tracks of one audio file (several when a cue sheet splits it), in order.
fn file_tracks(c: &Connection, file_id: i64) -> anyhow::Result<Vec<Track>> {
    let mut st =
        c.prepare("SELECT id FROM tracks WHERE file_id = ? ORDER BY cue_start, track_number, id")?;
    let ids: Vec<i64> = st
        .query_map([file_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(crate::track::load_many(c, &ids))
}
