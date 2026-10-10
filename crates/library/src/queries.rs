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

/// Id of the Various Artists contributor (`variousArtistsObject`), looked up by namesearch.
pub fn va_id_cfg(c: &Connection, cfg: &LibraryConfig) -> Option<i64> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let search = rules.search_key(&cfg.various_artists_string);
    c.query_row("SELECT id FROM contributors WHERE namesearch = ? LIMIT 1", [search], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

/// Multi-word search (each word must match at a word start). Returns (where clause, params, order expr).
pub fn search_clause(rules: &TextRules, cfg: &LibraryConfig, search: &str, col: &str, title_col: &str) -> (String, Vec<String>, String) {
    let norm = rules.normalize_search(search);
    let words: Vec<String> = norm.split_whitespace().map(|s| s.to_string()).collect();
    let words = if words.is_empty() { vec![norm.clone()] } else { words };
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
            title_clauses.push(format!("({t} LIKE '{e}%' OR {t} LIKE '% {e}%')", t = title_col, e = e));
        }
    }
    let order = format!("(CASE WHEN {} THEN 0 ELSE 1 END)", title_clauses.join(" AND "));
    (format!("({})", clauses.join(" AND ")), params, order)
}

fn get<'a>(q: &'a LibQuery, k: &str) -> Option<&'a str> {
    q.params.get(k).map(|s| s.as_str())
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

pub(crate) fn run(lib: &Library, q: &LibQuery) -> anyhow::Result<Row> {
    let cfg = lib.config();
    let c = lib.conn()?;
    let scanning = lib.is_scanning();
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
                if is_upper && list.iter().any(|(k, _)| k.len() == 1 && k.chars().all(|x| x.is_ascii_uppercase()) && ch.as_str() < k.as_str()) {
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
    Ok(Value::Array(list.into_iter().map(|(k, n)| json!([k, n])).collect()))
}

fn add_loop(out: &mut Row, name: &str, item: Row) {
    match out.get_mut(name) {
        Some(Value::Array(a)) => a.push(Value::Object(item)),
        _ => {
            out.insert(name.to_string(), Value::Array(vec![Value::Object(item)]));
        }
    }
}

fn fetch_map(r: &rusqlite::Row, names: &[String]) -> rusqlite::Result<std::collections::HashMap<String, SqlValue>> {
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
pub fn add_disc_number(title: &str, disc: Option<i64>, discc: Option<i64>, cfg: &LibraryConfig) -> String {
    let Some(d) = disc.filter(|d| *d > 0) else { return title.to_string() };
    let mut dc = discc;
    if let Some(n) = dc {
        if n == 1 {
            return title.to_string();
        }
        if n < 1 {
            dc = None;
        }
    }
    let re = regex::Regex::new(&format!(r"(?i)\b(({})|(Disc))\s+\d+", regex::escape(&cfg.string_disc))).unwrap();
    if re.is_match(title) {
        return title.to_string();
    }
    match dc {
        Some(n) => {
            let w = n.to_string().len();
            format!("{} ({} {:>w$} {} {})", title, cfg.string_disc, d, cfg.string_of, n, w = w)
        }
        None => format!("{} ({} {})", title, cfg.string_disc, d),
    }
}

// ---------------------------------------------------------------------------------------------
// albums
// ---------------------------------------------------------------------------------------------

fn albums(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").filter(|t| !t.is_empty()).unwrap_or("l").to_string();
    let search = get(q, "search");
    let mut compilation = get(q, "compilation").map(|s| s.to_string());
    let contributor_id = get(q, "artist_id");
    let genre_id = get(q, "genre_id");
    let track_id = get(q, "track_id");
    let album_id = get(q, "album_id");
    let role_id = get(q, "role_id").filter(|s| !s.is_empty());
    let release_type = get(q, "release_type");
    let library_id = get(q, "library_id").filter(|s| !s.is_empty());
    let year = get(q, "year");
    let work = get(q, "work_id").filter(|s| !s.is_empty() && *s != "0");
    let composer_id = get(q, "composer_id");
    let from_search = get(q, "from_search");
    let mut sort = get(q, "sort").filter(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| {
        if role_id.is_some() {
            "artistalbum".into()
        } else {
            "album".into()
        }
    });
    let valid_sorts = ["new", "playcount", "popular", "recentlyplayed", "changed", "album", "artflow", "artistalbum", "yearalbum", "yearartistalbum", "random"];
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
        ("albums.titlesearch".into(), "albums.titlesearch".into()),
        ("albums.titlesort".into(), "albums.titlesort".into()),
    ];
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    let mut order_by = format!("albums.titlesort {}, albums.disc", COLLATE);
    let mut limit: Option<usize> = None;
    let mut page_key: Option<String> = Some("SUBSTR(albums.titlesort,1,1)".into());
    let va = va_id_cfg(c, cfg);

    if let Some(t) = track_id {
        from.push_str("JOIN tracks ON tracks.album = albums.id ");
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
            let (clause, params, order) = search_clause(&rules, cfg, s, "albums.customsearch", "albums.titlesearch");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
            if !cc {
                order_by = format!("{}, LENGTH(albums.titlesearch)", order);
                sort = order_by.clone();
            }
        }
        let mut roles: Vec<String> = Vec::new();
        if let Some(cid) = contributor_id {
            if cid.parse::<i64>().ok() == va && va.is_some() {
                compilation = Some("1".into());
            } else {
                from.push_str("JOIN contributor_album ON contributor_album.album = albums.id ");
                w.push("contributor_album.contributor = ?".into());
                p.push(SqlValue::Text(cid.into()));
                if let Some(r) = role_id {
                    roles = split_ids(r);
                    if r == "ALBUMARTIST" && !cfg.use_unified_artists_list {
                        roles.push("ARTIST".into());
                    }
                } else if cfg.use_unified_artists_list {
                    let mut v: Vec<String> = cfg.active_contributor_roles(true).iter().map(|s| s.to_string()).collect();
                    for r in cfg.all_album_link_roles() {
                        if !v.iter().any(|x| x == r) {
                            v.push(r.to_string());
                        }
                    }
                    roles = v;
                } else {
                    roles = CONTRIBUTOR_ROLES.iter().map(|(n, _)| n.to_string()).collect();
                }
            }
        } else if let Some(r) = role_id {
            from.push_str("JOIN contributor_album ON contributor_album.album = albums.id ");
            roles = split_ids(r);
            if r == "ALBUMARTIST" && !cfg.use_unified_artists_list {
                roles.push("ARTIST".into());
            }
        }
        if !roles.is_empty() {
            let ids: Vec<i64> = roles.iter().filter_map(|r| type_to_role(r)).collect();
            w.push(format!("contributor_album.role IN ({})", vec!["?"; ids.len()].join(", ")));
            p.extend(ids.into_iter().map(SqlValue::Integer));
        }
        if !cc {
            if ["new", "changed", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str()) {
                from.push_str("JOIN tracks ON tracks.album = albums.id ");
                let l = if cfg.browse_age_limit > 0 { cfg.browse_age_limit } else { 100 };
                limit = Some(l);
                order_by = "MAX(tracks.timestamp) DESC".into();
                if let Some(qq) = quantity {
                    if qq > l {
                        quantity = Some(l);
                    }
                }
                if sort != "changed" {
                    from.push_str("LEFT JOIN tracks_persistent ON tracks_persistent.urlmd5 = tracks.urlmd5 ");
                    match sort.as_str() {
                        "new" => order_by = "MIN(tracks_persistent.added) DESC".into(),
                        "playcount" | "popular" => order_by = "albums_playcount DESC".into(),
                        "recentlyplayed" => order_by = "MAX(tracks_persistent.lastplayed) DESC".into(),
                        _ => {}
                    }
                    if sort != "new" {
                        w.push("tracks_persistent.lastplayed > 0".into());
                    }
                }
                page_key = None;
            } else if sort == "artflow" {
                order_by = format!("contributors.namesort {c}, albums.year, albums.titlesort {c}", c = COLLATE);
                cols.push(("contributors.namesort".into(), "contributors.namesort".into()));
                page_key = Some("SUBSTR(contributors.namesort,1,1)".into());
            } else if sort == "artistalbum" {
                order_by = format!("contributors.namesort {c}, albums.titlesort {c}", c = COLLATE);
                cols.push(("contributors.namesort".into(), "contributors.namesort".into()));
                page_key = Some("SUBSTR(contributors.namesort,1,1)".into());
            } else if sort == "yearartistalbum" {
                order_by = format!("albums.year, contributors.namesort {c}, albums.titlesort {c}", c = COLLATE);
                page_key = Some("albums.year".into());
            } else if sort == "yearalbum" {
                order_by = format!("albums.year, albums.titlesort {c}", c = COLLATE);
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
        if let Some(l) = library_id {
            w.push("albums.id IN (SELECT library_album.album FROM library_album WHERE library_album.library = ?)".into());
            p.push(SqlValue::Text(l.into()));
        }
        if let Some(rt) = release_type {
            let rts: Vec<String> = split_ids(rt).into_iter().map(|s| s.to_uppercase()).collect();
            w.push(format!("albums.release_type IN ({})", vec!["?"; rts.len()].join(", ")));
            p.extend(rts.into_iter().map(SqlValue::Text));
        }
        if let Some(y) = year {
            w.push("albums.year = ?".into());
            p.push(SqlValue::Text(y.into()));
        }
        if let (Some(fs), None) = (from_search, search) {
            let (clause, params, _) = search_clause(&rules, cfg, fs, "albums.customsearch", "albums.titlesearch");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
        }
        if let Some(g) = genre_id {
            let ids = split_ids(g);
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON tracks.album = albums.id ");
            }
            from.push_str("JOIN genre_track ON genre_track.track = tracks.id ");
            w.push(format!("genre_track.genre IN ({})", vec!["?"; ids.len()].join(", ")));
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
            from.push_str("JOIN tracks ON tracks.album = albums.id ");
        }
        from.push_str("JOIN works ON tracks.work = works.id ");
        from.push_str("JOIN contributors AS composer ON works.composer = composer.id ");
        if !from.contains("JOIN contributor_track ") {
            from.push_str("JOIN contributor_track ON contributor_track.track = tracks.id ");
        }
        for k in ["tracks.work", "works.title", "composer.name", "tracks.performance"] {
            cols.push((k.into(), k.into()));
        }
        order_by.push_str(", tracks.tracknum");
        if wk != "-1" {
            w.push("tracks.work = ?".into());
            p.push(SqlValue::Text(wk.into()));
            if let Some(cid) = composer_id {
                w.push("contributor_track.contributor = ? AND contributor_track.role = 2".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
    }
    if has("l") {
        for k in ["albums.title", "albums.disc", "albums.discc"] {
            cols.push((k.into(), k.into()));
        }
    }
    if has("y") {
        cols.push(("albums.year".into(), "albums.year".into()));
    }
    if has("j") {
        if work.is_some() {
            cols.push(("tracks.coverid".into(), "tracks.coverid".into()));
        } else {
            cols.push(("albums.artwork".into(), "albums.artwork".into()));
        }
    }
    cols.push(("albums.artwork".into(), "albums.artwork".into()));
    for (t, k) in [("t", "albums.title"), ("i", "albums.disc"), ("q", "albums.discc"), ("w", "albums.compilation"), ("E", "albums.extid"), ("X", "albums.replay_gain")] {
        if has(t) {
            cols.push((k.into(), k.into()));
        }
    }
    cols.push(("albums.extid".into(), "albums.extid".into()));
    cols.push(("albums.title".into(), "albums.title".into()));
    cols.push(("albums.compilation".into(), "albums.compilation".into()));
    if has("W") && !cfg.ignore_release_types {
        cols.push(("albums.release_type".into(), "albums.release_type".into()));
    }
    cols.push(("albums.contributor".into(), "albums.contributor".into()));
    if !cc {
        from.push_str("JOIN contributors ON contributors.id = albums.contributor ");
        cols.push(("contributors.name".into(), "contributors.name".into()));
        if has("4") && work.is_none() {
            cols.push(("contributors.portraitid".into(), "contributors.portraitid".into()));
        }
    }
    if has("2") {
        cols.push((
            "(SELECT COUNT(1) FROM (SELECT 1 FROM tracks WHERE tracks.album=albums.id GROUP BY work,grouping,performance))".into(),
            "group_count".into(),
        ));
        cols.push((
            "(SELECT GROUP_CONCAT(COALESCE(SUBSTR('00000'||disc,-5),'00001') || SUBSTR('00000'||tracknum,-5) || COALESCE(work,'') || '##' || COALESCE(performance,'') || '##' || COALESCE(grouping,''),',,') FROM tracks WHERE tracks.album = albums.id)".into(),
            "group_structure".into(),
        ));
    }
    if ["playcount", "popular"].contains(&sort.as_str()) {
        cols.push(("AVG(tracks_persistent.playcount)".into(), "albums_playcount".into()));
        if sort == "popular" {
            w.push("tracks_persistent.lastplayed > ?".into());
            p.push(SqlValue::Integer(crate::scanner::now() - cfg.browse_popularity_max_days * 86400));
        }
    }
    // dedupe columns
    let mut seen = std::collections::HashSet::new();
    cols.retain(|(_, k)| seen.insert(k.clone()));

    let mut where_sql = String::new();
    if !w.is_empty() {
        where_sql = format!("WHERE {} ", w.join(" AND "));
    }
    let group = if work.is_some() { "GROUP BY tracks.performance, tracks.work, albums.id " } else { "GROUP BY albums.id " };
    let mut out = Row::new();
    if let (Some(pk), true) = (&page_key, has("Z")) {
        let isql = format!("SELECT {} AS n {}{}{}ORDER BY {}", pk, from, where_sql, group, order_by);
        out.insert("indexList".into(), create_index_list(c, &isql, &p)?);
        if has("ZZ") {
            return Ok(out);
        }
    }
    let col_sql: Vec<String> = cols.iter().map(|(e, _)| e.clone()).collect();
    let names: Vec<String> = cols.iter().map(|(_, k)| k.clone()).collect();
    let mut sql = format!("SELECT {} {}{}{}", col_sql.join(", "), from, where_sql, group);
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
                let mut lr: Vec<String> = cfg.all_album_link_roles().iter().map(|s| s.to_string()).collect();
                if let (Some(r), None) = (role_id, contributor_id) {
                    let mut pre: Vec<String> = split_ids(r).into_iter().map(|x| x.parse::<i64>().ok().and_then(role_to_type).map(|s| s.to_string()).unwrap_or(x)).collect();
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
            let cname = sv_str(m.get("contributors.name")).unwrap_or_default();
            let work_title = sv_str(m.get("works.title"));
            let composer_name = sv_str(m.get("composer.name"));
            let perf = sv_str(m.get("tracks.performance"));
            item.insert("id".into(), json!(album_id_v));
            put_defined(&mut item, "work_id", sv_json(m.get("tracks.work")));
            put_defined(&mut item, "work_name", work_title.clone().map(Value::String).unwrap_or(Value::Null));
            put_defined(&mut item, "composer", composer_name.clone().map(Value::String).unwrap_or(Value::Null));
            item.insert("performance".into(), json!(perf.clone().unwrap_or_default()));
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
                format!("db:album.title={}&contributor.name={}", text::uri_escape_utf8(&title), text::uri_escape_utf8(&cname))
            };
            let extid = sv_str(m.get("albums.extid")).filter(|s| !s.is_empty());
            let fav = match (&extid, sv_str(m.get("tracks.work"))) {
                (Some(e), None) => e.clone(),
                _ => fav_url,
            };
            item.insert("favorites_url".into(), json!(fav));
            let fav_title = if work.is_some() {
                let mut t = composer_name.as_ref().map(|c| format!("{}: ", c)).unwrap_or_default();
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
                    add_disc_number(&title, sv_i64(m.get("albums.disc")), sv_i64(m.get("albums.discc")), cfg)
                };
                item.insert("album".into(), json!(t));
            }
            if has("y") {
                put_defined(&mut item, "year", sv_json(m.get("albums.year")));
            }
            let artwork = sv_str(m.get("albums.artwork"));
            if has("j") {
                let cover = if work.is_some() { sv_str(m.get("tracks.coverid")).filter(|s| !s.is_empty()).or(artwork.clone()) } else { artwork.clone() };
                if !cover.as_deref().unwrap_or("").starts_with("http") {
                    put_defined(&mut item, "artwork_track_id", cover.map(Value::String).unwrap_or(Value::Null));
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
                put_defined(&mut item, "disccount", sv_json(m.get("albums.discc")));
            }
            if has("w") {
                put_defined(&mut item, "compilation", sv_json(m.get("albums.compilation")));
            }
            if has("W") {
                let v = if cfg.ignore_release_types { json!("ALBUM") } else { sv_json(m.get("albums.release_type")) };
                put_defined(&mut item, "release_type", v);
            }
            if has("E") {
                put_defined(&mut item, "extid", sv_json(m.get("albums.extid")));
            }
            if has("X") {
                put_defined(&mut item, "album_replay_gain", sv_json(m.get("albums.replay_gain")));
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
                item.insert("contiguous_groups".into(), json!(if non_contig { 0 } else { 1 }));
            }
            let album_contrib = sv_i64(m.get("albums.contributor"));
            if work.is_none() {
                if has("S") {
                    put_defined(&mut item, "artist_id", album_contrib.map(|x| json!(x)).unwrap_or(Value::Null));
                }
                if has("a") {
                    put_defined(&mut item, "artist", json!(cname));
                }
                if has("4") {
                    put_defined(&mut item, "portraitid", sv_json(m.get("contributors.portraitid")));
                }
            }
            if has("s") {
                let tk = if sort == "artflow" || sort == "artistalbum" {
                    Some(text::first_char(&sv_str(m.get("contributors.namesort")).unwrap_or_default()))
                } else if sort == "album" {
                    Some(text::first_char(&sv_str(m.get("albums.titlesort")).unwrap_or_default()))
                } else {
                    None
                };
                if let Some(t) = tk {
                    item.insert("textkey".into(), json!(t));
                }
            }
            let is_comp = sv_i64(m.get("albums.compilation")).unwrap_or(0) != 0;
            if !link_roles.is_empty() && album_contrib != va && !is_comp {
                let (names_v, ids_v) = album_link_contributors(c, album_id_v, &link_roles, work, perf.as_deref(), if work.is_none() { Some((cname.clone(), album_contrib)) } else { None })?;
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
                    item.insert("artist_ids".into(), json!(ids_v.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")));
                }
            }
            if has("R") {
                let mut st2 = if contributor_id.is_some() {
                    c.prepare_cached("SELECT role FROM contributor_album WHERE album = ? AND contributor = ?")?
                } else {
                    c.prepare_cached("SELECT role FROM contributor_album WHERE album = ?")?
                };
                let roles: Vec<String> = if let Some(cid) = contributor_id {
                    st2.query_map(rusqlite::params![album_id_v, cid], |r| r.get::<_, i64>(0))?.filter_map(|r| r.ok()).map(|x| x.to_string()).collect()
                } else {
                    st2.query_map([album_id_v], |r| r.get::<_, i64>(0))?.filter_map(|r| r.ok()).map(|x| x.to_string()).collect()
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
    let rl = roles.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",");
    let rows: Vec<(i64, String, i64)> = if let Some(wk) = work {
        let sql = format!(
            "SELECT contributor_track.role, contributors.name, contributors.id FROM tracks
             JOIN contributor_track ON tracks.id = contributor_track.track
             JOIN contributors ON contributors.id = contributor_track.contributor
             WHERE tracks.album = ?1 AND tracks.work = ?2 AND ((?3 IS NULL AND tracks.performance IS NULL) OR tracks.performance = ?3)
               AND contributor_track.role IN ({})
             GROUP BY contributor_track.role, contributors.name, contributors.id
             ORDER BY contributor_track.role, contributors.namesort",
            rl
        );
        let mut st = c.prepare_cached(&sql)?;
        let perf: Option<String> = performance.filter(|s| !s.is_empty()).map(|s| s.to_string());
        let v = st
            .query_map(rusqlite::params![album, wk, perf], |r| Ok((r.get(0)?, get_text(r, 1)?.unwrap_or_default(), r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        v
    } else {
        let sql = format!(
            "SELECT contributor_album.role, contributors.name, contributors.id FROM contributor_album
             JOIN contributors ON contributors.id = contributor_album.contributor
             WHERE contributor_album.album = ? AND contributor_album.role IN ({})
             ORDER BY contributor_album.role, contributors.namesort {}",
            rl, COLLATE
        );
        let mut st = c.prepare_cached(&sql)?;
        let v = st
            .query_map([album], |r| Ok((r.get(0)?, get_text(r, 1)?.unwrap_or_default(), r.get(2)?)))?
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

fn artists(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let mut index = q.start;
    let mut quantity = q.quantity;
    let mut sort = get(q, "sort").filter(|s| !s.is_empty()).unwrap_or("artist").to_string();
    let search = get(q, "search").filter(|s| !s.is_empty());
    let year = get(q, "year");
    let genre_id = get(q, "genre_id");
    let genre_string = get(q, "genre_string");
    let track_id = get(q, "track_id");
    let album_id = get(q, "album_id");
    let artist_id = get(q, "artist_id");
    let mut role_id = get(q, "role_id").filter(|s| !s.is_empty()).map(|s| s.to_string());
    let work_id = get(q, "work_id");
    let include_online = get(q, "include_online_only_artists").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let library_id = get(q, "library_id").filter(|s| !s.is_empty());
    let tags = get(q, "tags").unwrap_or("").to_string();
    if !["artist", "new", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str()) {
        return Err(bad("sort"));
    }
    let aa_merge = role_id.as_deref() == Some("ALBUMARTIST") && !cfg.use_unified_artists_list;
    let va_pref = cfg.various_artist_auto_identification && cfg.use_unified_artists_list;
    let mut want_external = include_online && year.is_none() && genre_id.is_none() && genre_string.is_none() && track_id.is_none() && album_id.is_none() && artist_id.is_none();
    let va = va_id_cfg(c, cfg);

    let mut from = String::from("FROM contributors ");
    let mut sql_va = String::from("SELECT COUNT(*) FROM albums ");
    let mut w: Vec<String> = Vec::new();
    let mut w_va: Vec<String> = vec!["albums.compilation = 1".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    let mut p_va: Vec<SqlValue> = Vec::new();
    let mut order_by = format!("contributors.namesort {}, contributors.musicbrainz_id", COLLATE);
    let use_stats = ["new", "playcount", "popular", "recentlyplayed"].contains(&sort.as_str());
    if use_stats {
        match sort.as_str() {
            "new" => order_by = format!("MIN(tracks_persistent.added) DESC, {}", order_by),
            "playcount" | "popular" => order_by = format!("SUM(tracks_persistent.playcount) DESC, MIN(tracks.timestamp), {}", order_by),
            _ => order_by = format!("MAX(tracks_persistent.lastplayed) DESC, {}", order_by),
        }
        let l = if cfg.browse_age_limit > 0 { cfg.browse_age_limit } else { 100 };
        quantity = Some(quantity.unwrap_or(l).min(l));
        want_external = false;
    }
    let tc = |from: &mut String| {
        if !from.contains("JOIN contributor_track ") {
            from.push_str("JOIN contributor_track ON contributor_track.contributor = contributors.id ");
        }
    };
    if let Some(t) = track_id {
        tc(&mut from);
        w.push("contributor_track.track = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else if let Some(a) = artist_id {
        w.push("contributors.id = ?".into());
        p.push(SqlValue::Text(a.into()));
    } else {
        let roles: Vec<i64> = if let Some(r) = &mut role_id {
            if aa_merge {
                r.push_str(",ARTIST");
            }
            split_ids(r).iter().filter_map(|x| type_to_role(x)).collect()
        } else if cfg.use_unified_artists_list {
            cfg.active_contributor_roles(false).iter().filter_map(|r| type_to_role(r)).collect()
        } else {
            CONTRIBUTOR_ROLES.iter().map(|(_, i)| *i).collect()
        };
        let join_tracks = |from: &mut String| {
            tc(from);
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON tracks.id = contributor_track.track ");
            }
        };
        if let Some(g) = genre_id {
            let ids = split_ids(g);
            join_tracks(&mut from);
            from.push_str("JOIN genre_track ON genre_track.track = tracks.id ");
            w.push(format!("genre_track.genre IN ({})", vec!["?"; ids.len()].join(", ")));
            p.extend(ids.into_iter().map(SqlValue::Text));
            sql_va.push_str("JOIN tracks ON tracks.album = albums.id JOIN genre_track ON genre_track.track = tracks.id ");
            w_va.push("genre_track.genre = ?".into());
            p_va.push(SqlValue::Text(g.into()));
        }
        if let Some(wk) = work_id {
            join_tracks(&mut from);
            if wk == "-1" {
                w.push("tracks.work IS NOT NULL".into());
            } else {
                w.push("tracks.work = ?".into());
                p.push(SqlValue::Text(wk.into()));
            }
        }
        if use_stats {
            join_tracks(&mut from);
            from.push_str("LEFT JOIN tracks_persistent ON tracks_persistent.urlmd5 = tracks.urlmd5 ");
            if sort == "popular" {
                w.push("tracks_persistent.lastplayed > ?".into());
                p.push(SqlValue::Integer(crate::scanner::now() - cfg.browse_popularity_max_days * 86400));
            } else if sort != "new" {
                w.push("tracks_persistent.lastplayed > 0".into());
            }
        }
        if search.is_none() && !from.contains("JOIN contributor_track ") {
            from.push_str(&format!("{}JOIN contributor_album ON contributor_album.contributor = contributors.id ", if want_external { "LEFT " } else { "" }));
        }
        let role_list = roles.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(",");
        if from.contains("JOIN contributor_track ") {
            w.push(format!("(contributor_track.role IN ({}) {})", role_list, if search.is_some() { "OR contributors.id = ? " } else { "" }));
        } else {
            if !from.contains("JOIN contributor_album ") {
                from.push_str(&format!(
                    "{}JOIN contributor_album ON contributor_album.contributor = contributors.id ",
                    if search.is_some() || want_external { "LEFT " } else { "" }
                ));
            }
            w.push(format!("(contributor_album.role IN ({}) {})", role_list, if search.is_some() { "OR contributors.id = ? " } else { "" }));
        }
        if search.is_some() {
            p.push(va.map(SqlValue::Integer).unwrap_or(SqlValue::Integer(-1)));
        }
        if va_pref || aa_merge {
            if from.contains("JOIN tracks ") {
                from.push_str("JOIN albums ON albums.id = tracks.album ");
            } else {
                if !from.contains("JOIN contributor_album ") {
                    from.push_str(&format!("{}JOIN contributor_album ON contributor_album.contributor = contributors.id ", if want_external { "LEFT " } else { "" }));
                }
                from.push_str(&format!("{}JOIN albums ON contributor_album.album = albums.id ", if want_external { "LEFT " } else { "" }));
            }
            let extra = if va_pref { String::new() } else { format!(" OR contributors.id = {}", va.unwrap_or(-1)) };
            w.push(format!("(albums.compilation IS NULL OR albums.compilation = 0{})", extra));
        }
        if album_id.is_some() || year.is_some() {
            if !from.contains("JOIN contributor_album ") {
                from.push_str("JOIN contributor_album ON contributor_album.contributor = contributors.id ");
            }
            if !from.contains("JOIN albums ") {
                from.push_str("JOIN albums ON contributor_album.album = albums.id ");
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
            let (clause, params, order) = search_clause(&rules, cfg, s, "contributors.namesearch", "contributors.namesearch");
            w.push(clause);
            p.extend(params.into_iter().map(SqlValue::Text));
            if tags != "CC" {
                order_by = format!("{}, LENGTH(contributors.name), {}", order, order_by);
            }
        }
        if let Some(l) = library_id {
            from.push_str(&format!("{}JOIN library_contributor ON library_contributor.contributor = contributors.id ", if want_external { "LEFT " } else { "" }));
            w.push("library_contributor.library = ?".into());
            p.push(SqlValue::Text(l.into()));
            if from.contains("JOIN albums ") {
                from.push_str(&format!("{}JOIN library_album ON library_album.album = albums.id ", if want_external { "LEFT " } else { "" }));
                w.push("library_album.library = ?".into());
                p.push(SqlValue::Text(l.into()));
            }
            sql_va.push_str("JOIN library_album ON library_album.album = albums.id ");
            w_va.push("library_album.library = ?".into());
            p_va.push(SqlValue::Text(l.into()));
        }
    }
    let mut where_sql = String::new();
    if !w.is_empty() {
        let s = w.join(" AND ");
        where_sql = if want_external && library_id.is_none() {
            format!("WHERE ({}) OR (contributors.extid IS NOT NULL AND contributors.extid != '') ", s)
        } else {
            format!("WHERE {} ", s)
        };
    }
    let mut count_va = 0i64;
    if va_pref && search.is_none() && track_id.is_none() && artist_id.is_none() && role_id.is_none() && !use_stats {
        let s = format!("{}WHERE {} ", sql_va, w_va.join(" AND "));
        count_va = c.query_row(&s, params_from_iter(p_va.iter()), |r| r.get(0))?;
    }
    let mut out = Row::new();
    let mut index_list: Option<Value> = None;
    if tags.contains('Z') && !use_stats {
        let isql = format!("SELECT SUBSTR(contributors.namesort,1,1) {}{}GROUP BY contributors.id ORDER BY contributors.namesort {}", from, where_sql, COLLATE);
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
    let mut cols = vec!["contributors.id", "contributors.name", "contributors.namesort"];
    if tags.contains('E') {
        cols.push("contributors.extid");
    }
    if tags.contains('4') {
        cols.push("contributors.portraitid");
    }
    let mut sql = format!("SELECT {} {}{}GROUP BY contributors.id ", cols.join(", "), from, where_sql);
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
        let mut emit = |id: i64, name: String, namesort: String, extid: Option<String>, portrait: Option<String>, first: bool| {
            let mut item = Row::new();
            item.insert("id".into(), json!(id));
            item.insert("artist".into(), json!(name));
            if !use_stats && tags.contains('s') {
                let tk = if add_va && first { " ".to_string() } else { text::first_char(&namesort) };
                item.insert("textkey".into(), json!(tk));
            }
            if tags.contains('E') {
                if let Some(e) = extid.filter(|e| !e.is_empty()) {
                    item.insert("extid".into(), json!(e));
                }
            }
            if tags.contains('4') {
                if let Some(pid) = portrait.filter(|e| !e.is_empty()) {
                    item.insert("portraitid".into(), json!(pid));
                }
            }
            item.insert("favorites_url".into(), json!(format!("db:contributor.name={}", text::uri_escape_utf8(&name))));
            add_loop(&mut out, "artists_loop", item);
        };
        let mut first = true;
        if add_va {
            if let Some(vid) = va {
                let (name, ns): (String, String) = c.query_row("SELECT name, namesort FROM contributors WHERE id = ?", [vid], |r| {
                    Ok((get_text(r, 0)?.unwrap_or_default(), get_text(r, 1)?.unwrap_or_default()))
                })?;
                emit(vid, name, ns, None, None, true);
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
                let mut i = 3;
                let extid = if tags.contains('E') {
                    i += 1;
                    get_text(r, i - 1)?
                } else {
                    None
                };
                let portrait = if tags.contains('4') { get_text(r, i)? } else { None };
                emit(id, name, ns, extid, portrait, first && !add_va);
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

fn genres(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").unwrap_or("").to_string();
    let mut from = String::from("FROM genres ");
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = get(q, "search").filter(|s| !s.is_empty() && specified(s)) {
        let (clause, params, _) = search_clause(&rules, cfg, s, "genres.namesearch", "genres.namesearch");
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
    }
    let va = va_id_cfg(c, cfg);
    if let Some(t) = get(q, "track_id") {
        from.push_str("JOIN genre_track ON genres.id = genre_track.genre ");
        w.push("genre_track.track = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else if let Some(g) = get(q, "genre_id") {
        let ids = split_ids(g);
        w.push(format!("genres.id IN ({})", vec!["?"; ids.len()].join(", ")));
        p.extend(ids.into_iter().map(SqlValue::Text));
    } else {
        if let Some(cid) = get(q, "artist_id") {
            if cid.parse::<i64>().ok() == va && va.is_some() {
                from.push_str("JOIN genre_track ON genres.id = genre_track.genre JOIN tracks ON genre_track.track = tracks.id JOIN albums ON tracks.album = albums.id ");
                w.push("albums.compilation = ?".into());
                p.push(SqlValue::Integer(1));
            } else {
                from.push_str("JOIN genre_track ON genres.id = genre_track.genre JOIN contributor_track ON genre_track.track = contributor_track.track ");
                w.push("contributor_track.contributor = ?".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
        if let Some(l) = get(q, "library_id").filter(|s| !s.is_empty()) {
            from.push_str("JOIN library_genre ON library_genre.genre = genres.id ");
            w.push("library_genre.library = ?".into());
            p.push(SqlValue::Text(l.into()));
        }
        let album = get(q, "album_id");
        let year = get(q, "year");
        let work = get(q, "work_id");
        if album.is_some() || year.is_some() || work.is_some() {
            if !from.contains("JOIN genre_track ") {
                from.push_str("JOIN genre_track ON genres.id = genre_track.genre ");
            }
            if !from.contains("JOIN tracks ") {
                from.push_str("JOIN tracks ON genre_track.track = tracks.id ");
            }
            if let Some(a) = album {
                w.push("tracks.album = ?".into());
                p.push(SqlValue::Text(a.into()));
            }
            if let Some(y) = year {
                w.push("tracks.year = ?".into());
                p.push(SqlValue::Text(y.into()));
            }
            if let Some(wk) = work {
                if wk == "-1" {
                    w.push("tracks.work IS NOT NULL".into());
                } else {
                    w.push("tracks.work = ?".into());
                    p.push(SqlValue::Text(wk.into()));
                }
            }
        }
    }
    let where_sql = if w.is_empty() { String::new() } else { format!("WHERE {} ", w.join(" AND ")) };
    let mut out = Row::new();
    if tags.contains('Z') {
        let isql = format!("SELECT SUBSTR(genres.namesort,1,1) {}{}ORDER BY genres.namesort {}", from, where_sql, COLLATE);
        out.insert("indexList".into(), create_index_list(c, &isql, &p)?);
        if tags.contains("ZZ") {
            return Ok(out);
        }
    }
    let mut sql = format!("SELECT DISTINCT(genres.id), genres.name, genres.namesort {}{}", from, where_sql);
    if tags != "CC" {
        sql.push_str(&format!("ORDER BY genres.namesort {}", COLLATE));
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
            item.insert("favorites_url".into(), json!(format!("db:genre.name={}", text::uri_escape_utf8(&name))));
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
    let has_albums = get(q, "hasAlbums").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let (key, table) = if has_albums { ("albums.year", "albums") } else { ("tracks.year", "tracks") };
    let mut sql = format!("SELECT DISTINCT {} FROM {} ", key, table);
    let mut w = vec![format!("{} != '0'", key)];
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(y) = get(q, "year") {
        w.push(format!("{} = ?", key));
        p.push(SqlValue::Text(y.into()));
    }
    if let Some(l) = get(q, "library_id").filter(|s| !s.is_empty()) {
        if has_albums {
            sql.push_str("JOIN tracks ON tracks.album = albums.id ");
        }
        sql.push_str("JOIN library_track ON library_track.track = tracks.id ");
        w.push("library_track.library = ?".into());
        p.push(SqlValue::Text(l.into()));
    }
    sql.push_str(&format!("WHERE {} AND {} IS NOT NULL ", w.join(" AND "), key));
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
    let mut from = String::from("FROM contributors ");
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    let track = get(q, "track_id");
    if let Some(t) = track {
        from.push_str("JOIN contributor_track ON contributors.id = contributor_track.contributor ");
        w.push("contributor_track.track = ?".into());
        p.push(SqlValue::Text(t.into()));
    } else {
        from.push_str("JOIN contributor_album ON contributors.id = contributor_album.contributor ");
        if let Some(cid) = get(q, "artist_id") {
            let va: Option<i64> = va_id_cfg(c, cfg);
            if cid.parse::<i64>().ok() == va && va.is_some() {
                from.push_str("JOIN albums ON contributor_album.album = albums.id ");
                w.push("albums.compilation = ?".into());
                p.push(SqlValue::Integer(1));
            } else {
                w.push("contributor_album.contributor = ?".into());
                p.push(SqlValue::Text(cid.into()));
            }
        }
        if let Some(l) = get(q, "library_id").filter(|s| !s.is_empty()) {
            from.push_str("JOIN library_contributor ON library_contributor.contributor = contributors.id ");
            w.push("library_contributor.library = ?".into());
            p.push(SqlValue::Text(l.into()));
        }
        if let Some(a) = get(q, "album_id") {
            w.push("contributor_album.album = ?".into());
            p.push(SqlValue::Text(a.into()));
        }
        let year = get(q, "year");
        let work = get(q, "work_id");
        if year.is_some() || work.is_some() {
            from.push_str("JOIN contributor_track ON contributors.id = contributor_track.contributor JOIN tracks ON tracks.id = contributor_track.track ");
            if let Some(y) = year {
                w.push("tracks.year = ?".into());
                p.push(SqlValue::Text(y.into()));
            }
            if let Some(wk) = work {
                if wk == "-1" {
                    w.push("tracks.work IS NOT NULL".into());
                } else {
                    w.push("tracks.work = ?".into());
                    p.push(SqlValue::Text(wk.into()));
                }
            }
        }
    }
    let col = if track.is_some() { "DISTINCT contributor_track.role" } else { "DISTINCT contributor_album.role" };
    let mut sql = format!("SELECT {} {}{}", col, from, if w.is_empty() { String::new() } else { format!("WHERE {} ", w.join(" AND ")) });
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
                item.insert("role_name".into(), role_to_type(role).map(|s| json!(s)).unwrap_or(Value::Null));
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
        JOIN contributor_track composer_track ON composer_track.track = tracks.id AND composer_track.role = 2
        JOIN contributors composer ON composer.id = composer_track.contributor
        JOIN contributor_track ON contributor_track.track = tracks.id
        JOIN contributors ON contributors.id = contributor_track.contributor
        JOIN works ON works.id = tracks.work AND works.composer = composer.id
        JOIN albums ON tracks.album = albums.id ";
    let mut from = from.to_string();
    let mut w: Vec<String> = Vec::new();
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = get(q, "search").filter(|s| !s.is_empty() && specified(s)) {
        let (c1, p1, _) = search_clause(&rules, cfg, s, "works.titlesearch", "works.titlesearch");
        let (c2, p2, _) = search_clause(&rules, cfg, s, "albums.customsearch", "albums.titlesearch");
        w.push(format!("({} OR {})", c1, c2));
        p.extend(p1.into_iter().map(SqlValue::Text));
        p.extend(p2.into_iter().map(SqlValue::Text));
    } else {
        w.push("tracks.work IS NOT NULL".into());
        if let Some(wk) = get(q, "work_id").filter(|s| *s != "-1") {
            let ids = split_ids(wk);
            if !ids.is_empty() {
                w.push(format!("works.id IN ({})", vec!["?"; ids.len()].join(", ")));
                p.extend(ids.into_iter().map(SqlValue::Text));
            }
        }
        if let Some(a) = get(q, "album_id") {
            let ids: Vec<String> = split_ids(a).into_iter().filter(|x| x.chars().all(|c| c.is_ascii_digit())).collect();
            w.push(format!("albums.id IN ({})", if ids.is_empty() { "-1".into() } else { ids.join(",") }));
        }
        if let Some(y) = get(q, "year") {
            w.push("tracks.year = ?".into());
            p.push(SqlValue::Text(y.into()));
        }
        if let Some(r) = get(q, "role_id") {
            let ids: Vec<i64> = split_ids(r).iter().filter_map(|x| type_to_role(x)).collect();
            if !ids.is_empty() {
                w.push(format!("contributor_track.role IN ({})", vec!["?"; ids.len()].join(", ")));
                p.extend(ids.into_iter().map(SqlValue::Integer));
            }
        }
        if let Some(a) = artist_id {
            w.push("contributors.id = ?".into());
            p.push(SqlValue::Text(a.into()));
        }
        if let Some(g) = get(q, "genre_id") {
            let ids = split_ids(g);
            from.push_str("JOIN genre_track ON genre_track.track = tracks.id ");
            w.push(format!("genre_track.genre IN ({})", vec!["?"; ids.len()].join(", ")));
            p.extend(ids.into_iter().map(SqlValue::Text));
        }
    }
    w.push(
        "NOT EXISTS (SELECT 1 FROM tracks t2 WHERE tracks.album=t2.album AND tracks.work=t2.work
         AND (tracks.performance IS NULL AND t2.performance IS NULL OR tracks.performance=t2.performance) AND tracks.id > t2.id)"
            .into(),
    );
    if let Some(l) = get(q, "library_id").filter(|s| !s.is_empty()) {
        w.push("EXISTS (SELECT 1 FROM library_album WHERE library_album.album = albums.id AND library_album.library = ?)".into());
        p.push(SqlValue::Text(l.into()));
    }
    let base = format!("{}WHERE {} ", from, w.join(" AND "));
    let composer_count = count_sql(c, &format!("SELECT 'c' {} GROUP BY composer.id", base), &p)?;
    let grouped = format!("{} GROUP BY tracks.work, composer.id ", base);
    let count = count_sql(c, &format!("SELECT 'c' {}", grouped), &p)?;
    let order_by = format!("ORDER BY composer.namesort {c} , works.titlesort {c}", c = COLLATE);
    let mut out = Row::new();
    if tags.contains('Z') {
        let pk = if composer_count == 1 { "SUBSTR(works.titlesort,1,1)" } else { "SUBSTR(composer.namesort,1,1)" };
        out.insert("indexList".into(), create_index_list(c, &format!("SELECT {} AS n {} {}", pk, grouped, order_by), &p)?);
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
            "SELECT works.title, works.id, composer.name, composer.id, composer.namesort, works.titlesort, GROUP_CONCAT(DISTINCT tracks.coverid), GROUP_CONCAT(DISTINCT albums.id) {} {}",
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
            if composer_count == 1 && artist_id.map(|a| a == composer_id.to_string()).unwrap_or(false) {
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
            item.insert("textkey".into(), json!(text::first_char(if composer_count == 1 { &ts } else { &ns })));
            item.insert(
                "favorites_url".into(),
                json!(format!("db:work.title={}&composer.name={}", text::uri_escape_utf8(&work), text::uri_escape_utf8(&composer))),
            );
            item.insert("favorites_title".into(), json!(format!("{}: {}", composer, work)));
            add_loop(&mut out, "works_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// titles / songinfo / playlists tracks
// ---------------------------------------------------------------------------------------------

fn titles(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let mut tags = get(q, "tags").map(|s| s.to_string()).unwrap_or_else(|| "gald".into());
    let sort = get(q, "sort").filter(|s| !s.is_empty()).unwrap_or("title").to_string();
    let search = get(q, "search").map(|s| s.to_string());
    let ignore_work = get(q, "ignore_work_tracks").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let mut where_base = String::from("(tracks.audio = 1 AND tracks.content_type NOT IN (\"cpl\", \"src\", \"ssp\", \"dir\") ");
    where_base.push_str(if ignore_work { "AND tracks.work IS NULL)" } else { ")" });
    let mut order_by = format!("tracks.titlesort {}", COLLATE);
    if sort == "tracknum" {
        tags.push('t');
        order_by = format!("tracks.disc, tracks.tracknum, tracks.titlesort {}", COLLATE);
    } else if let Some(s) = sort.strip_prefix("sql=") {
        order_by = s.replace(';', "");
    } else if sort == "albumtrack" {
        tags.push_str("tl");
        order_by = format!("albums.titlesort, tracks.disc, tracks.tracknum, tracks.titlesort {}", COLLATE);
    } else if sort == "title" && (tags.contains('a') || tags.contains('s')) && (tags.contains('e') || tags.contains('l')) {
        order_by = format!("tracks.titlesort, contributors.namesort, albums.titlesort, tracks.year {}", COLLATE);
    }
    if let Some(s) = &search {
        if s.contains("tracks_persistent.rating") && !tags.contains('R') {
            tags.push('R');
        }
        if s.contains("tracks_persistent.playcount") && !tags.contains('O') {
            tags.push('O');
        }
    }
    let args = TagDataArgs {
        where_base: Some(where_base),
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
        library_id: get(q, "library_id").map(|s| s.to_string()),
        only_album_years: get(q, "only_album_years").map(|s| !s.is_empty() && s != "0").unwrap_or(false),
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

fn songinfo(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let tags = get(q, "tags").map(|s| s.to_string()).unwrap_or_else(|| "abcdefghijJklmnopqrstvwxyzBCDEFHIJKLMNOQRTUVWXY".into());
    let track_id = get(q, "track_id");
    let u = get(q, "url");
    if track_id.is_none() && u.is_none() {
        return Err(bad("url or track_id required"));
    }
    let track = if let Some(id) = track_id {
        id.parse::<i64>().ok().and_then(|id| crate::track::load_by_id(c, id))
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
fn playlist_track_list(c: &Connection, playlist_id: i64) -> anyhow::Result<Option<(String, Vec<Track>)>> {
    let row: Option<(Option<String>, Option<String>)> = c
        .query_row("SELECT title, content_type FROM tracks WHERE id = ?", [playlist_id], |r| Ok((get_text(r, 0)?, r.get(1)?)))
        .optional()?;
    let Some((title, ct)) = row else { return Ok(None) };
    if !crate::types::is_playlist_type(ct.as_deref().unwrap_or("")) {
        return Ok(None);
    }
    let mut st = c.prepare("SELECT track FROM playlist_track WHERE playlist = ? ORDER BY position")?;
    let urls: Vec<String> = st.query_map([playlist_id], |r| r.get::<_, String>(0))?.filter_map(|r| r.ok()).collect();
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
    Ok(Some((title.unwrap_or_default(), tracks)))
}

fn playlist_tracks(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let tags = get(q, "tags").map(|s| s.to_string()).unwrap_or_else(|| "gald".into());
    let Some(pid) = get(q, "playlist_id") else { return Err(bad("playlist_id required")) };
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
                    let data = if t.id > 0 { songdata::object_row(c, cfg, t, &tags) } else { songdata::minimal_row(t) };
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

fn playlists(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let tags = get(q, "tags").unwrap_or("").to_string();
    let search = get(q, "search").filter(|s| !s.is_empty());
    let folder = get(q, "folder_id").unwrap_or("");
    let mut folders: Vec<(String, String)> = Vec::new();
    let mut w: Vec<String> = vec!["tracks.content_type IN ('ssp')".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    if let Some(s) = search {
        let (clause, params, _) = search_clause(&rules, cfg, s, "tracks.titlesearch", "tracks.titlesearch");
        w.insert(0, clause);
        p.extend(params.into_iter().map(SqlValue::Text));
    } else if !folder.is_empty() {
        let mut f = folder.to_string();
        if !f.starts_with("file:/") && regex::Regex::new(r"%[0-9A-Fa-f]{2}").unwrap().is_match(&f) {
            f = String::from_utf8_lossy(&crate::url::percent_decode_bytes(&f)).into_owned();
        }
        if f.starts_with("file:/") {
            f = crate::url::path_from_file_url(&f).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        }
        if f.is_empty() || f == "/" || f == "file:" {
            f = cfg.playlist_dir.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        }
        let mut furl = crate::url::file_url_from_path(Path::new(&f));
        furl.push('/');
        // sub folders
        let mut st = c.prepare(
            "SELECT url FROM tracks WHERE url LIKE ? ESCAPE '\\' AND content_type IN ('ssp') AND instr(substr(url, ?), '/') > 0",
        )?;
        let urls: Vec<String> = st
            .query_map(rusqlite::params![format!("{}%", crate::scanner::like_escape(&furl)), furl.len() as i64 + 1], |r| r.get::<_, String>(0))?
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
        w.push("tracks.url LIKE ? ESCAPE '\\' AND instr(substr(tracks.url, ?), '/') = 0".into());
        p.push(SqlValue::Text(format!("{}%", crate::scanner::like_escape(&furl))));
        p.push(SqlValue::Integer(furl.len() as i64 + 1));
    }
    if let Some(l) = get(q, "library_id").filter(|s| !s.is_empty()) {
        w.push(
            "tracks.id IN (SELECT DISTINCT playlist_track.playlist FROM playlist_track LEFT OUTER JOIN tracks t2 ON t2.url = playlist_track.track
              LEFT OUTER JOIN library_track ON library_track.track = t2.id
              WHERE playlist_track.track NOT LIKE 'file:/%' OR (library_track.track = t2.id AND library_track.library = ?))"
                .into(),
        );
        p.push(SqlValue::Text(l.into()));
    }
    let sql = format!(
        "SELECT tracks.id, tracks.title, tracks.url, tracks.titlesort, tracks.extid, tracks.remote FROM tracks WHERE {} ORDER BY tracks.titlesort {}",
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
            ((start as i64 - folder_count as i64).max(0), end as i64 - folder_count as i64)
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
                let extid = get_text(r, 4)?;
                let remote: Option<i64> = r.get(5)?;
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
                if tags.contains('E') {
                    item.insert("extid".into(), extid.map(Value::String).unwrap_or(Value::Null));
                }
                if tags.contains('x') {
                    item.insert("remote".into(), json!(if remote.unwrap_or(0) != 0 { 1 } else { 0 }));
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

fn search(c: &Connection, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    let term = get(q, "term").map(text::match_case).unwrap_or_default();
    if term.is_empty() {
        return Err(bad("term required"));
    }
    let extended = get(q, "extended").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let library_id = get(q, "library_id").filter(|s| !s.is_empty());
    let mut out = Row::new();
    if scanning {
        out.insert("rescan".into(), json!(1));
    }
    let mut total = 0usize;
    let types: [(&str, &str, &str, &str, Vec<&str>); 5] = [
        ("contributor", "name", "me.namesearch", "me.namesearch", vec![]),
        ("album", "title", "me.customsearch", "me.titlesearch", vec!["me.artwork"]),
        ("work", "title", "me.titlesearch", "me.titlesearch", vec![]),
        ("genre", "name", "me.namesearch", "me.namesearch", vec![]),
        ("track", "title", "me.customsearch", "me.titlesearch", vec!["me.coverid", "me.audio"]),
    ];
    for (ty, name, col, title_col, extra) in types.iter() {
        let mut cols = format!("me.id, me.{}", name);
        if extended && !extra.is_empty() {
            cols = format!("{}, {}", cols, extra.join(", "));
        }
        let mut from = format!("FROM {}s me ", ty);
        let mut w: Vec<String> = Vec::new();
        let mut p: Vec<SqlValue> = Vec::new();
        if *ty == "track" {
            w.push("me.audio = ?".into());
            p.push(SqlValue::Text("1".into()));
        }
        if let Some(l) = library_id {
            match *ty {
                "contributor" => from.push_str("JOIN contributor_track ON contributor_track.contributor = me.id JOIN library_track ON library_track.track = contributor_track.track "),
                "work" => from.push_str("JOIN tracks ON tracks.work = me.id JOIN library_track ON library_track.track = tracks.id "),
                "album" => from.push_str("JOIN tracks ON tracks.album = me.id JOIN library_track ON library_track.track = tracks.id "),
                "genre" => from.push_str("JOIN genre_track ON genre_track.genre = me.id JOIN library_track ON library_track.track = genre_track.track "),
                _ => from.push_str("JOIN library_track ON library_track.track = me.id "),
            }
            w.push("library_track.library = ?".into());
            p.push(SqlValue::Text(l.into()));
        }
        let (clause, params, order) = search_clause(&rules, cfg, &term, col, title_col);
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
        let mut sql = format!("SELECT {} {}WHERE {} ", cols, from, w.join(" AND "));
        if library_id.is_some() {
            sql.push_str("GROUP BY me.id ");
        }
        let count = count_sql(c, &sql, &p)?;
        total += count;
        let (valid, _, _) = normalize(q.start, q.quantity, count);
        if valid {
            out.insert(format!("{}s_count", ty), json!(count));
            let qty = q.quantity.unwrap_or(count);
            sql.push_str(&format!("ORDER BY {}, me.id LIMIT {}, {}", order, q.start.unwrap_or(0), qty));
            let mut st = c.prepare(&sql)?;
            let mut rows = st.query(params_from_iter(p.iter()))?;
            while let Some(r) = rows.next()? {
                let id: i64 = r.get(0)?;
                let title = get_text(r, 1)?.unwrap_or_default();
                let mut item = Row::new();
                item.insert(format!("{}_id", ty), json!(id));
                item.insert(ty.to_string(), json!(title));
                if extended {
                    for (i, e) in extra.iter().enumerate() {
                        let v: SqlValue = r.get(2 + i)?;
                        item.insert(e.trim_start_matches("me.").to_string(), sql_to_json(&v));
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
    let ignore_re = cfg.ignore_dir_re.as_deref().filter(|s| !s.is_empty()).and_then(|s| regex::Regex::new(s).ok());
    let valid = |p: &Path| -> bool {
        if p.is_dir() {
            return true;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        crate::types::is_audio_ext(&ext, &cfg.disabled_extensions_audio) || crate::types::is_playlist_ext(&ext, &cfg.disabled_extensions_playlist)
    };
    let mut out = Vec::new();
    if recursive {
        let mut it = walkdir::WalkDir::new(dir).follow_links(true).min_depth(1).sort_by(|a, b| a.file_name().to_ascii_lowercase().cmp(&b.file_name().to_ascii_lowercase())).into_iter();
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
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
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
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    out
}

fn musicfolder(lib: &Library, cfg: &LibraryConfig, q: &LibQuery, scanning: bool) -> anyhow::Result<Row> {
    let mut tags = get(q, "tags").unwrap_or("").to_string();
    let ty = get(q, "type").unwrap_or("");
    if !ty.is_empty() && ty != "audio" {
        tags = tags.replace('d', "");
    }
    let folder_id = get(q, "folder_id").filter(|s| !s.is_empty());
    let want_top = get(q, "return_top").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let url_param = get(q, "url").filter(|s| !s.is_empty());
    let recursive = get(q, "recursive").map(|s| !s.is_empty() && s != "0").unwrap_or(false);
    let media_dirs: Vec<PathBuf> = if ty.is_empty() || ty == "audio" { cfg.media_dirs.clone() } else { Vec::new() };

    let mut items: Vec<Track> = Vec::new();
    if url_param.is_none() && folder_id.is_none() && media_dirs.len() > 1 {
        for d in &media_dirs {
            if let Some(t) = lib.ensure_object(&crate::url::file_url_from_path(d)) {
                items.push(t);
            }
        }
    } else {
        let top: Option<Track> = if let Some(u) = url_param {
            let u = u.replacen("tmp://", "file://", 1);
            let u = if crate::url::is_file_url(&u) { u } else { crate::url::file_url_from_path(Path::new(&u)) };
            lib.ensure_object(&u)
        } else if let Some(fid) = folder_id {
            fid.parse::<i64>().ok().and_then(|id| lib.track(id))
        } else if let Some(d) = media_dirs.first() {
            lib.ensure_object(&crate::url::file_url_from_path(d))
        } else {
            None
        };
        if let Some(top) = top {
            if want_top {
                items.push(top);
            } else if let Some(path) = top.path() {
                if path.is_dir() {
                    if !scanning {
                        lib.refresh_dir(&top, &path);
                    }
                    for p in list_dir(&path, cfg, recursive) {
                        let u = crate::url::file_url_from_path(&p);
                        if let Some(t) = lib.ensure_object(&u) {
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
        for t in items.iter().take(end + 1).skip(start) {
            let mut item = Row::new();
            let filename = crate::url::file_name(&t.url);
            item.insert("id".into(), json!(t.id));
            item.insert("filename".into(), json!(filename));
            let ct = t.content_type.as_str();
            if ct == "dir" {
                item.insert("type".into(), json!("folder"));
            } else if crate::types::is_playlist_type(ct) {
                item.insert("type".into(), json!("playlist"));
                if tags.contains('u') {
                    item.insert("favorites_url".into(), json!(t.url));
                }
            } else if crate::types::is_song_type(ct) {
                item.insert("type".into(), json!("track"));
            } else if t.path().map(|p| p.is_dir()).unwrap_or(false) {
                item.insert("type".into(), json!("folder"));
            } else {
                item.insert("type".into(), json!("unknown"));
            }
            if tags.contains('c') {
                item.insert("coverid".into(), t.coverid.clone().map(Value::String).unwrap_or(Value::Null));
            }
            if tags.contains('d') {
                item.insert("duration".into(), t.duration_str().map(Value::String).unwrap_or(Value::Null));
            }
            if tags.contains('s') {
                item.insert("textkey".into(), json!(text::first_char(&filename).to_uppercase()));
            }
            if tags.contains('u') {
                item.insert("url".into(), json!(t.url));
            }
            if tags.contains('t') {
                item.insert("title".into(), json!(filename));
            }
            if tags.contains('o') {
                item.insert("ct".into(), json!(t.content_type));
            }
            add_loop(&mut out, "folder_loop", item);
        }
    }
    out.insert("count".into(), json!(count));
    Ok(out)
}

impl Library {
    /// `Slim::Schema->objectForUrl({create => 1, readTags => 1})` for local files and directories.
    pub fn ensure_object(&self, u: &str) -> Option<Track> {
        if let Some(t) = self.track_by_url(u) {
            return Some(t);
        }
        let path = crate::url::path_from_file_url(u)?;
        let u = crate::url::file_url_from_path(&path);
        if let Some(t) = self.track_by_url(&u) {
            return Some(t);
        }
        let cfg = self.config();
        let c = self.conn().ok()?;
        if path.is_dir() {
            let (mt, _, _) = crate::scanner::file_stat(&path).unwrap_or((0, 0, true));
            let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
            let title = crate::url::plain_title(&u, true);
            let now = crate::scanner::now();
            c.execute(
                "INSERT INTO tracks (url, urlmd5, title, titlesort, titlesearch, content_type, timestamp, filesize, remote, added_time, updated_time)
                 VALUES (?, ?, ?, ?, ?, 'dir', ?, 0, 0, ?, ?)",
                rusqlite::params![u, crate::url::md5_hex(&u), title, rules.sort_key(&title), rules.search_key(&title), mt, now, now],
            )
            .ok()?;
            return crate::track::load_by_url(&c, &u);
        }
        if !path.is_file() {
            return None;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
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
            sc.scan_cue_file(&u, &path).map(|_| ())
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
        crate::track::load_by_url(&c, &u)
    }

    /// `findAndScanDirectoryTree`: quick non-recursive rescan when the directory mtime changed.
    pub(crate) fn refresh_dir(&self, top: &Track, path: &Path) {
        let Some((mt, _, _)) = crate::scanner::file_stat(path) else { return };
        if top.timestamp == Some(mt) {
            return;
        }
        let Ok(c) = self.conn() else { return };
        let _ = c.execute("UPDATE tracks SET timestamp = ? WHERE id = ?", rusqlite::params![mt, top.id]);
        let Some(_guard) = self.inner.scan_lock.try_lock() else { return };
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
    let duration: f64 = c.query_row("SELECT COALESCE(SUM(secs), 0) FROM tracks WHERE audio = 1", [], |r| r.get(0))?;
    Ok(Totals { albums, artists, genres, songs, duration, playlists })
}

pub(crate) fn tracks_for(lib: &Library, f: &TrackFilter) -> anyhow::Result<Vec<Track>> {
    let c = lib.conn()?;
    let cfg = lib.config();
    let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
    if !f.track_ids.is_empty() {
        return Ok(f.track_ids.iter().filter_map(|id| crate::track::load_by_id(&c, *id)).collect());
    }
    if let Some(pid) = f.playlist_id {
        return Ok(playlist_track_list(&c, pid)?.map(|(_, t)| t).unwrap_or_default());
    }
    if let Some(fid) = f.folder_id {
        drop(c);
        let Some(top) = lib.track(fid) else { return Ok(Vec::new()) };
        let Some(path) = top.path() else { return Ok(Vec::new()) };
        if !path.is_dir() {
            return Ok(if top.audio { vec![top] } else { Vec::new() });
        }
        let mut out = Vec::new();
        for p in list_dir(&path, &cfg, true) {
            let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if ext == "cue" || !crate::types::is_audio_ext(&ext, &cfg.disabled_extensions_audio) {
                continue;
            }
            let u = crate::url::file_url_from_path(&p);
            let Some(t) = lib.ensure_object(&u) else { continue };
            if t.audio {
                out.push(t);
            } else if t.content_type == "cur" || t.content_type == "fec" {
                // container: its virtual cue tracks
                let c = lib.conn()?;
                let mut st = c.prepare("SELECT id FROM tracks WHERE url LIKE ? ESCAPE '\\' AND virtual = 1 ORDER BY tracknum, id")?;
                let ids: Vec<i64> = st
                    .query_map([format!("{}#%", crate::scanner::like_escape(&t.url))], |r| r.get::<_, i64>(0))?
                    .filter_map(|r| r.ok())
                    .collect();
                out.extend(ids.into_iter().filter_map(|id| crate::track::load_by_id(&c, id)));
            }
        }
        return Ok(out);
    }
    let album_sort = format!("albums.titlesort {c}, tracks.disc, tracks.tracknum, tracks.titlesort {c}", c = COLLATE);
    let album_year_sort = format!("albums.year, {}", album_sort);
    let track_sort = format!("tracks.disc, tracks.tracknum, tracks.titlesort {}", COLLATE);
    let mut sort: Option<String> = None;
    let mut from = String::from("FROM tracks LEFT JOIN albums ON albums.id = tracks.album ");
    let mut w: Vec<String> = vec!["tracks.audio = 1".into()];
    let mut p: Vec<SqlValue> = Vec::new();
    let va = va_id_cfg(&c, &cfg);
    let mut single_album = false;
    let mut compilation = false;
    if let Some(a) = &f.artist_id {
        if Some(*a) == va {
            compilation = true;
        } else {
            from.push_str("JOIN contributor_track ON contributor_track.track = tracks.id ");
            w.push("contributor_track.contributor = ?".into());
            p.push(SqlValue::Integer(*a));
            if let Some(r) = &f.role_id {
                let mut roles = split_ids(r);
                if (r == "ALBUMARTIST" || r == "5") && !cfg.use_unified_artists_list {
                    roles.push("ARTIST".into());
                }
                let ids: Vec<i64> = roles.iter().filter_map(|x| type_to_role(x)).collect();
                w.push(format!("contributor_track.role IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")));
            }
        }
        sort = Some(album_sort.clone());
    } else if let Some(r) = &f.role_id {
        from.push_str("JOIN contributor_track ON contributor_track.track = tracks.id ");
        let ids: Vec<i64> = split_ids(r).iter().filter_map(|x| type_to_role(x)).collect();
        w.push(format!("contributor_track.role IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")));
    }
    if compilation {
        w.push("albums.compilation = 1".into());
    }
    if let Some(g) = &f.genre_id {
        let ids = split_ids(g);
        from.push_str("JOIN genre_track ON genre_track.track = tracks.id ");
        w.push(format!("genre_track.genre IN ({})", vec!["?"; ids.len()].join(",")));
        p.extend(ids.into_iter().map(SqlValue::Text));
        sort = Some(album_sort.clone());
    }
    if let Some(a) = &f.album_id {
        let ids = split_ids(a);
        single_album = ids.len() == 1;
        w.push(format!("tracks.album IN ({})", vec!["?"; ids.len()].join(",")));
        p.extend(ids.into_iter().map(SqlValue::Text));
        sort = Some(album_sort.clone());
    }
    if let Some(y) = f.year {
        w.push("tracks.year = ?".into());
        p.push(SqlValue::Integer(y));
        sort = Some(album_sort.clone());
    }
    if let Some(wk) = f.work_id {
        w.push("tracks.work = ?".into());
        p.push(SqlValue::Integer(wk));
        sort = Some(album_sort.clone());
    }
    if let Some(rt) = &f.release_type {
        let rts: Vec<String> = split_ids(rt).into_iter().map(|s| s.to_uppercase()).collect();
        w.push(format!("albums.release_type IN ({})", vec!["?"; rts.len()].join(",")));
        p.extend(rts.into_iter().map(SqlValue::Text));
    }
    if let Some(l) = f.library_id.as_ref().filter(|s| !s.is_empty()) {
        from.push_str("JOIN library_track ON library_track.track = tracks.id ");
        w.push("library_track.library = ?".into());
        p.push(SqlValue::Text(l.clone()));
    }
    if let Some(s) = f.search.as_ref().filter(|s| !s.is_empty()) {
        let (clause, params, order) = search_clause(&rules, &cfg, s, "tracks.customsearch", "tracks.titlesearch");
        w.push(clause);
        p.extend(params.into_iter().map(SqlValue::Text));
        if sort.is_none() {
            sort = Some(format!("{}, tracks.titlesort {}", order, COLLATE));
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
                if let Some(raw) = other.strip_prefix("sql=") {
                    sort = Some(raw.replace(';', ""));
                } else if other == "tracknum" {
                    sort = Some(track_sort.clone());
                } else if other == "title" {
                    sort = Some(format!("tracks.titlesort {}", COLLATE));
                }
            }
        }
    }
    if single_album && sort.as_deref() == Some(album_sort.as_str()) {
        sort = None;
    }
    let order = sort.unwrap_or(track_sort);
    let sql = format!("SELECT tracks.id {}WHERE {} GROUP BY tracks.id ORDER BY {}", from, w.join(" AND "), order);
    let mut st = c.prepare(&sql)?;
    let ids: Vec<i64> = st.query_map(params_from_iter(p.iter()), |r| r.get::<_, i64>(0))?.filter_map(|r| r.ok()).collect();
    drop(st);
    Ok(ids.into_iter().filter_map(|id| crate::track::load_by_id(&c, id)).collect())
}
