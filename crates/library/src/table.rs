//! The web UI's Table: the library as rows of tracks, or of albums, artists, genres, years, …
//! with the columns asked for, from a `field:value` query.  Not part of LMS.
//!
//! * [`FIELDS`] is every column: how to read it, filter on it, sort by it, group by it, and
//!   what a grouped row shows of it (the one value its tracks share, *varies*, a total, a
//!   range).
//! * [`parse`] reads a query: words are searched as everywhere, `field:value` filters
//!   (`year:1990..1999`, `bpm:>120`, `added:<30d`, `genre:jazz,soul`, `-format:mp3`,
//!   `missing:label`, `varies:album_artist`).
//! * [`Library::table`] runs one: a page of rows, how many there are, and how many tracks
//!   and how long they are in all.  The first page works the whole result out (ordered track
//!   ids, or every grouped row) and keeps it, so scrolling through the rest is quick.

use crate::text::TextRules;
use crate::Library;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a field holds, for parsing its values and showing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Int,
    Real,
    /// Seconds; written `3:30`, `90s`, `5m`.
    Duration,
    /// Unix time; written as an age (`30d`, `6mo`, `2y`) or a date (`2024-05-01`).
    Time,
    Bool,
    /// Bytes; written `700mb`, `1.5gb`.
    Size,
    /// Bits a second; written in kbit/s (`320`, `320k`).
    Bitrate,
}

/// What a grouped row shows of a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Agg {
    /// The value its tracks share, else `{"varies": n}`.
    Same,
    Sum,
    Avg,
    Max,
    /// `[min, max]`, or the one value.
    Range,
}

/// Grouping by a field: one row per distinct value.
#[derive(Debug)]
struct Group {
    /// What a row stands for.
    key: &'static str,
    /// Its name.
    label: &'static str,
    /// What to sort its rows by.
    sort: &'static str,
    /// Tables to join (genres: one row per genre of each track).
    join: &'static str,
    /// The filter that finds a row's tracks: `{}` is replaced by the key (`album_id:{}`), or
    /// `None` to filter on the field's own value.
    drill: Option<&'static str>,
    /// The grouping to show a row's tracks in.
    next: &'static str,
}

/// A column of the Table.
#[derive(Debug)]
pub struct Field {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// The value, for a track (`t` tracks, `al` albums, `ar` the track's artist, `aa` the
    /// album's artist, `f` files, `s` track_stats, `r` track_rhythm, `a` track_analysis).
    expr: &'static str,
    /// For text filters: a column of search keys (`title_search`); `None`: the value lowercased.
    search: Option<&'static str>,
    /// For sorting, when not the value (`title_sort`).
    sort: Option<&'static str>,
    /// Several values to a track: the filter (`?` is the search-key pattern).
    multi: Option<&'static str>,
    group: Option<Group>,
    pub agg: Agg,
    /// Not a column, only for filters (`album_id:12`).
    pub hidden: bool,
}

const fn field(name: &'static str, label: &'static str, kind: Kind, expr: &'static str) -> Field {
    Field {
        name,
        label,
        kind,
        expr,
        search: None,
        sort: None,
        multi: None,
        group: None,
        agg: Agg::Same,
        hidden: false,
    }
}

const fn by_value(next: &'static str) -> Option<Group> {
    Some(Group {
        key: "",
        label: "",
        sort: "",
        join: "",
        drill: None,
        next,
    })
}

/// Effective tempo: measured, else tagged.
const BPM: &str = "COALESCE(r.bpm, NULLIF(t.bpm, 0))";
const GENRES: &str = "(SELECT group_concat(g.name, '; ') FROM (SELECT g.name FROM track_genres tg \
     JOIN genres g ON g.id = tg.genre_id WHERE tg.track_id = t.id ORDER BY g.name_sort) g)";

/// Every field, in the order the column chooser lists them.
pub static FIELDS: &[Field] = &[
    Field {
        search: Some("t.title_search"),
        sort: Some("t.title_sort"),
        ..field("title", "Title", Kind::Text, "t.title")
    },
    Field {
        search: Some("ar.name_search"),
        sort: Some("ar.name_sort"),
        // any of the track's artists
        multi: Some(
            "t.id IN (SELECT ta.track_id FROM track_artists ta JOIN artists x ON x.id = ta.artist_id \
             WHERE x.name_search LIKE ?)",
        ),
        group: Some(Group {
            key: "t.artist_id",
            label: "ar.name",
            sort: "ar.name_sort",
            join: "",
            drill: Some("artist_id:{}"),
            next: "album",
        }),
        ..field("artist", "Artist", Kind::Text, "ar.name")
    },
    Field {
        search: Some("aa.name_search"),
        sort: Some("aa.name_sort"),
        group: Some(Group {
            key: "al.artist_id",
            label: "aa.name",
            sort: "aa.name_sort",
            join: "",
            drill: Some("album_artist_id:{}"),
            next: "album",
        }),
        ..field("album_artist", "Album artist", Kind::Text, "aa.name")
    },
    Field {
        search: Some("al.title_search"),
        sort: Some("al.title_sort"),
        group: Some(Group {
            key: "t.album_id",
            label: "al.title",
            sort: "al.title_sort",
            join: "",
            drill: Some("album_id:{}"),
            next: "track",
        }),
        ..field("album", "Album", Kind::Text, "al.title")
    },
    Field {
        sort: Some("COALESCE(t.disc, 1) * 10000 + t.track_number"),
        ..field("track", "#", Kind::Int, "t.track_number")
    },
    field("disc", "Disc", Kind::Int, "t.disc"),
    Field {
        multi: Some(
            "t.id IN (SELECT tg.track_id FROM track_genres tg JOIN genres g ON g.id = tg.genre_id \
             WHERE g.name_search LIKE ?)",
        ),
        group: Some(Group {
            key: "gx.id",
            label: "gx.name",
            sort: "gx.name_sort",
            join: "LEFT JOIN track_genres tgx ON tgx.track_id = t.id \
                   LEFT JOIN genres gx ON gx.id = tgx.genre_id",
            drill: Some("genre_id:{}"),
            next: "album",
        }),
        ..field("genre", "Genre", Kind::Text, GENRES)
    },
    Field {
        group: by_value("album"),
        agg: Agg::Range,
        ..field("year", "Year", Kind::Int, "NULLIF(t.year, 0)")
    },
    Field {
        group: by_value("album"),
        agg: Agg::Range,
        ..field("decade", "Decade", Kind::Int, "(NULLIF(t.year, 0) / 10 * 10)")
    },
    Field {
        agg: Agg::Sum,
        ..field("duration", "Length", Kind::Duration, "t.duration")
    },
    Field {
        group: by_value("album"),
        ..field("format", "Format", Kind::Text, "t.format")
    },
    field("bitrate", "Bitrate", Kind::Bitrate, "t.bitrate"),
    Field {
        group: by_value("album"),
        ..field("sample_rate", "Sample rate", Kind::Int, "t.sample_rate")
    },
    Field {
        group: by_value("album"),
        ..field("bits", "Bits", Kind::Int, "t.sample_size")
    },
    field("channels", "Channels", Kind::Int, "t.channels"),
    Field {
        group: by_value("album"),
        ..field("lossless", "Lossless", Kind::Bool, "t.lossless")
    },
    Field {
        agg: Agg::Range,
        ..field("bpm", "BPM", Kind::Real, BPM)
    },
    Field {
        group: by_value("track"),
        ..field("key", "Key", Kind::Text, "r.camelot")
    },
    Field {
        agg: Agg::Avg,
        group: by_value("track"),
        ..field("rating", "Rating", Kind::Int, "s.rating")
    },
    Field {
        agg: Agg::Sum,
        ..field("plays", "Plays", Kind::Int, "COALESCE(s.play_count, 0)")
    },
    Field {
        agg: Agg::Sum,
        ..field("skips", "Skips", Kind::Int, "COALESCE(s.skip_count, 0)")
    },
    Field {
        agg: Agg::Max,
        ..field("last_played", "Last played", Kind::Time, "s.last_played")
    },
    Field {
        agg: Agg::Max,
        ..field("added", "Added", Kind::Time, "COALESCE(s.added_at, t.added_at)")
    },
    Field {
        agg: Agg::Avg,
        ..field("loudness", "Loudness", Kind::Real, "a.loudness")
    },
    field("replay_gain", "ReplayGain", Kind::Real, "t.replay_gain"),
    Field {
        group: by_value("track"),
        ..field("analyzed", "Analysed", Kind::Bool, "(a.features IS NOT NULL)")
    },
    Field {
        group: by_value("album"),
        ..field("release_type", "Release type", Kind::Text, "lower(al.release_type)")
    },
    Field {
        group: by_value("album"),
        ..field("compilation", "Compilation", Kind::Bool, "al.compilation")
    },
    field("mbid", "MusicBrainz id", Kind::Text, "t.mbid"),
    field("isrc", "ISRC", Kind::Text, "t.isrc"),
    Field {
        agg: Agg::Sum,
        ..field("size", "Size", Kind::Size, "f.size")
    },
    Field {
        group: by_value("track"),
        ..field("folder", "Folder", Kind::Text, "rms_dir(t.url)")
    },
    field("path", "Path", Kind::Text, "rms_path(t.url)"),
    // filters only: what a grouped row's tracks are found by
    Field {
        hidden: true,
        ..field("album_id", "Album id", Kind::Int, "t.album_id")
    },
    Field {
        hidden: true,
        ..field("artist_id", "Artist id", Kind::Int, "t.artist_id")
    },
    Field {
        hidden: true,
        ..field("album_artist_id", "Album artist id", Kind::Int, "al.artist_id")
    },
    Field {
        hidden: true,
        multi: Some("t.id IN (SELECT track_id FROM track_genres WHERE genre_id = ?)"),
        ..field("genre_id", "Genre id", Kind::Int, "NULL")
    },
    Field {
        hidden: true,
        ..field("id", "Track id", Kind::Int, "t.id")
    },
];

/// The column of how many tracks a grouped row has.
pub const COUNT: &str = "count";

pub fn field_named(name: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.name == name)
}

impl Field {
    pub fn groupable(&self) -> bool {
        self.group.is_some()
    }

    fn sort_expr(&self) -> &'static str {
        self.sort.unwrap_or(self.expr)
    }

    fn group_key(&self) -> &'static str {
        self.group
            .as_ref()
            .map(|g| g.key)
            .filter(|k| !k.is_empty())
            .unwrap_or(self.expr)
    }

    fn group_label(&self) -> &'static str {
        self.group
            .as_ref()
            .map(|g| g.label)
            .filter(|k| !k.is_empty())
            .unwrap_or(self.expr)
    }

    fn group_sort(&self) -> &'static str {
        self.group
            .as_ref()
            .map(|g| g.sort)
            .filter(|k| !k.is_empty())
            .unwrap_or(self.expr)
    }

    /// The grouping a row's tracks are shown in.
    pub fn next_group(&self) -> Option<&'static str> {
        self.group.as_ref().map(|g| g.next)
    }
}

// -------------------------------------------------------------------------------- queries

#[derive(Debug, Clone, PartialEq)]
enum Cond {
    /// Contains (text), equals (numbers).
    Is(String),
    /// Exactly (text, ignoring case).
    Exact(String),
    /// Any of these.
    Any(Vec<String>),
    Cmp(&'static str, f64),
    /// Inclusive.
    Range(f64, f64),
}

#[derive(Debug)]
enum Term {
    Words(String, bool),
    Field(&'static Field, Cond, bool),
    Missing(&'static Field, bool),
    Varies(&'static Field),
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct QueryError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, QueryError> {
    Err(QueryError(msg.into()))
}

/// Split a query into tokens: whitespace separates them, except inside double quotes
/// (`\"` is a quote within).  A token keeps its quotes off: `artist:"Sigur Rós"` →
/// `artist:Sigur Rós`, with a flag that it was quoted (so `"re:birth"` stays a word).
fn tokens(q: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    // the token began with a quote; inside quotes; anything seen
    let (mut quoted, mut in_quotes, mut any) = (false, false, false);
    let mut chars = q.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes && c == '\\' && chars.peek() == Some(&'"') {
            cur.push('"');
            chars.next();
        } else if c == '"' {
            quoted |= !any;
            in_quotes = !in_quotes;
            any = true;
        } else if c.is_whitespace() && !in_quotes {
            if any {
                out.push((std::mem::take(&mut cur), quoted));
            }
            (quoted, any) = (false, false);
        } else {
            cur.push(c);
            any = true;
        }
    }
    if any {
        out.push((cur, quoted));
    }
    out
}

/// `30d`, `6mo`, `2y`, `12h`, `3w`: seconds.
fn age(s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (n, unit) = s.split_at(split);
    let n: f64 = n.parse().ok()?;
    let secs = match unit {
        "h" => 3600.0,
        "d" => 86_400.0,
        "w" => 7.0 * 86_400.0,
        "mo" => 30.44 * 86_400.0,
        "y" => 365.25 * 86_400.0,
        _ => return None,
    };
    Some(n * secs)
}

/// `2024`, `2024-05`, `2024-05-01`: the first and last second of that year, month or day (UTC).
fn date(s: &str) -> Option<(f64, f64)> {
    use chrono::NaiveDate;
    let parts: Vec<&str> = s.trim().split('-').collect();
    let y: i32 = parts
        .first()?
        .parse()
        .ok()
        .filter(|y| (1000..=9999).contains(y))?;
    let start = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
    let (a, b) = match parts.len() {
        1 => (start(y, 1, 1)?, start(y + 1, 1, 1)?),
        2 => {
            let m: u32 = parts[1].parse().ok()?;
            let a = start(y, m, 1)?;
            let b = if m == 12 {
                start(y + 1, 1, 1)?
            } else {
                start(y, m + 1, 1)?
            };
            (a, b)
        }
        3 => {
            let a = start(y, parts[1].parse().ok()?, parts[2].parse().ok()?)?;
            (a, a.succ_opt()?)
        }
        _ => return None,
    };
    let t = |d: NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .map(|x| x.and_utc().timestamp() as f64)
    };
    Some((t(a)?, t(b)? - 1.0))
}

/// A number in a field's units.
fn number(kind: Kind, s: &str) -> Option<f64> {
    let s = s.trim().to_ascii_lowercase();
    match kind {
        Kind::Duration => {
            if let Some((m, sec)) = s.split_once(':') {
                let m: f64 = m.parse().ok()?;
                let sec: f64 = sec.parse().ok()?;
                return Some(m * 60.0 + sec);
            }
            if let Some(m) = s.strip_suffix('m') {
                return m.parse::<f64>().ok().map(|m| m * 60.0);
            }
            if let Some(h) = s.strip_suffix('h') {
                return h.parse::<f64>().ok().map(|h| h * 3600.0);
            }
            s.strip_suffix('s').unwrap_or(&s).parse().ok()
        }
        Kind::Size => {
            for (suffix, mult) in [("kb", 1e3), ("mb", 1e6), ("gb", 1e9), ("b", 1.0)] {
                if let Some(n) = s.strip_suffix(suffix) {
                    return n.trim().parse::<f64>().ok().map(|n| n * mult);
                }
            }
            s.parse().ok()
        }
        Kind::Bitrate => s
            .strip_suffix("kbps")
            .or_else(|| s.strip_suffix('k'))
            .unwrap_or(&s)
            .parse::<f64>()
            .ok()
            .map(|k| k * 1000.0),
        Kind::Bool => match s.as_str() {
            "yes" | "true" | "1" | "y" => Some(1.0),
            "no" | "false" | "0" | "n" => Some(0.0),
            _ => None,
        },
        _ => s.parse().ok(),
    }
}

/// A field's value condition.
fn condition(f: &'static Field, v: &str, now: f64) -> Result<Cond, QueryError> {
    let bad = |what: &str| err(format!("{}: {what}", f.name));
    if f.kind == Kind::Text {
        if let Some(x) = v.strip_prefix('=') {
            return Ok(Cond::Exact(x.to_string()));
        }
        if v.contains(',') {
            let any: Vec<String> = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Ok(Cond::Any(any));
        }
        return Ok(Cond::Is(v.to_string()));
    }
    if f.kind == Kind::Time {
        // an age: `<30d` newer than 30 days, `>1y` older than a year; or dates
        for (op, newer) in [("<=", ">="), (">=", "<="), ("<", ">"), (">", "<")] {
            if let Some(rest) = v.strip_prefix(op) {
                if let Some(a) = age(rest) {
                    return Ok(Cond::Cmp(newer, now - a));
                }
                if let Some((start, end)) = date(rest) {
                    // before the period starts, or after it ends; `<=`/`>=` take it in
                    let at = match op {
                        "<" | ">=" => start,
                        _ => end,
                    };
                    let op = match op {
                        "<" => "<",
                        "<=" => "<=",
                        ">" => ">",
                        _ => ">=",
                    };
                    return Ok(Cond::Cmp(op, at));
                }
                return bad("a time is an age (30d, 6mo, 2y) or a date (2024-05-01)");
            }
        }
        if let Some((a, b)) = v.split_once("..") {
            let (Some((a, _)), Some((_, b))) = (date(a), date(b)) else {
                return bad("a range of dates is 2023..2024-06");
            };
            return Ok(Cond::Range(a, b));
        }
        if let Some((a, b)) = date(v) {
            return Ok(Cond::Range(a, b));
        }
        if let Some(a) = age(v) {
            // `30d`: within the last 30 days
            return Ok(Cond::Cmp(">=", now - a));
        }
        return bad("a time is an age (30d, 6mo, 2y) or a date (2024-05-01)");
    }
    let num = |s: &str| {
        number(f.kind, s)
            .ok_or_else(|| QueryError(format!("{}: {s:?} isn't a {}", f.name, unit(f.kind))))
    };
    for op in ["<=", ">=", "<", ">", "="] {
        if let Some(rest) = v.strip_prefix(op) {
            let n = num(rest)?;
            return Ok(if op == "=" {
                Cond::Is(n.to_string())
            } else {
                Cond::Cmp(op, n)
            });
        }
    }
    if let Some((a, b)) = v.split_once("..") {
        let a = if a.is_empty() {
            f64::NEG_INFINITY
        } else {
            num(a)?
        };
        let b = if b.is_empty() { f64::INFINITY } else { num(b)? };
        return Ok(Cond::Range(a, b));
    }
    if v.contains(',') {
        let any = v
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| num(s).map(|n| n.to_string()))
            .collect::<Result<_, _>>()?;
        return Ok(Cond::Any(any));
    }
    Ok(Cond::Is(num(v)?.to_string()))
}

fn unit(kind: Kind) -> &'static str {
    match kind {
        Kind::Duration => "length (3:30, 90s, 5m)",
        Kind::Size => "size (700mb, 1.5gb)",
        Kind::Bitrate => "bitrate in kbit/s (320)",
        Kind::Bool => "yes or no",
        _ => "number",
    }
}

/// A parsed query.
#[derive(Debug, Default)]
pub struct Query {
    terms: Vec<Term>,
}

/// Read a query (`now`: Unix time, for ages).
pub fn parse(q: &str, now: i64) -> Result<Query, QueryError> {
    let now = now as f64;
    let mut terms = Vec::new();
    for (tok, quoted) in tokens(q) {
        if tok.is_empty() {
            continue;
        }
        let (neg, body) = match tok.strip_prefix('-') {
            Some(rest) if !rest.is_empty() && !quoted => (true, rest),
            _ => (false, tok.as_str()),
        };
        let pair = (!quoted).then(|| body.split_once(':')).flatten();
        let Some((name, value)) = pair else {
            terms.push(Term::Words(body.to_string(), neg));
            continue;
        };
        let name = name.to_ascii_lowercase();
        let lookup = |n: &str| {
            field_named(n).ok_or_else(|| {
                QueryError(format!(
                    "no field called {n:?} (quote words with a colon: \"re:birth\")"
                ))
            })
        };
        match name.as_str() {
            "missing" | "has" => {
                let f = lookup(&value.to_ascii_lowercase())?;
                terms.push(Term::Missing(f, (name == "has") != neg));
            }
            "varies" => {
                if neg {
                    return err("varies: can't be negated");
                }
                terms.push(Term::Varies(lookup(&value.to_ascii_lowercase())?));
            }
            _ => {
                let f = lookup(&name)?;
                if value.is_empty() {
                    return err(format!(
                        "{name}: what value? (missing:{name} finds tracks without one)"
                    ));
                }
                terms.push(Term::Field(f, condition(f, value, now)?, neg));
            }
        }
    }
    Ok(Query { terms })
}

/// A filter written back as query text (`field:=value`), for a grouped row's tracks.
fn exact(field: &str, v: &Value) -> String {
    match v {
        Value::Null => format!("missing:{field}"),
        Value::String(s) => format!("{field}:=\"{}\"", s.replace('"', "\\\"")),
        Value::Bool(b) => format!("{field}:{}", if *b { "yes" } else { "no" }),
        other => format!("{field}:={other}"),
    }
}

// -------------------------------------------------------------------------------- SQL

/// The tables a field's SQL can read, by alias, after `tracks t`.
const JOINS: &[(&str, &str)] = &[
    ("al", "LEFT JOIN albums al ON al.id = t.album_id"),
    ("ar", "LEFT JOIN artists ar ON ar.id = t.artist_id"),
    ("aa", "LEFT JOIN artists aa ON aa.id = al.artist_id"),
    ("f", "LEFT JOIN files f ON f.id = t.file_id"),
    ("s", "LEFT JOIN track_stats s ON s.url = t.url"),
    (
        "r",
        "LEFT JOIN track_rhythm r ON r.fingerprint = t.fingerprint",
    ),
    (
        "a",
        "LEFT JOIN track_analysis a ON a.fingerprint = t.fingerprint",
    ),
];

/// `FROM tracks t` and only the joins `sql` uses (each one costs a lookup per track).
fn from(sql: &str) -> String {
    static ALIAS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?:^|[^A-Za-z0-9_.])(al|ar|aa|f|s|r|a)\.").expect("regex")
    });
    let used: std::collections::HashSet<&str> = ALIAS
        .captures_iter(sql)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .collect();
    let mut out = String::from("FROM tracks t");
    for (alias, join) in JOINS {
        // the album artist is reached through the album
        let needed = used.contains(alias) || (*alias == "al" && used.contains("aa"));
        if needed {
            out.push(' ');
            out.push_str(join);
        }
    }
    out
}

/// `LIKE` pattern for "contains".
fn contains(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// The search key of a text value for field `f` (its search column holds search keys).
fn text_key(f: &Field, rules: &TextRules, v: &str) -> String {
    if f.search.is_some() || f.multi.is_some() {
        rules.search_key(v)
    } else {
        v.to_lowercase()
    }
}

struct Sql {
    wheres: Vec<String>,
    /// `varies:` fields: groups whose tracks differ in them.
    varies: Vec<&'static Field>,
    params: Vec<SqlValue>,
}

impl Sql {
    fn wheres(&self) -> String {
        if self.wheres.is_empty() {
            "1".into()
        } else {
            self.wheres.join(" AND ")
        }
    }
}

fn cond_sql(f: &Field, c: &Cond, rules: &TextRules, p: &mut Vec<SqlValue>) -> String {
    let text = f.kind == Kind::Text;
    let col = if text {
        f.search
            .map(str::to_string)
            .unwrap_or_else(|| format!("lower({})", f.expr))
    } else {
        f.expr.to_string()
    };
    let num = |s: &str| SqlValue::Real(s.parse().unwrap_or(0.0));
    if let (Some(multi), true) = (f.multi, text || f.hidden) {
        // several values to a track: any of them
        let one = |v: &str, exact: bool, p: &mut Vec<SqlValue>| {
            let key = text_key(f, rules, v);
            p.push(if f.hidden {
                SqlValue::Integer(v.parse().unwrap_or(-1))
            } else if exact {
                SqlValue::Text(
                    key.replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_"),
                )
            } else {
                SqlValue::Text(contains(&key))
            });
            multi.replace("LIKE ?", "LIKE ? ESCAPE '\\'")
        };
        return match c {
            Cond::Is(v) => one(v, f.hidden, p),
            Cond::Exact(v) => one(v, true, p),
            Cond::Any(vs) => format!(
                "({})",
                vs.iter()
                    .map(|v| one(v, false, p))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
            _ => "0".into(),
        };
    }
    match c {
        Cond::Is(v) if text => {
            p.push(SqlValue::Text(contains(&text_key(f, rules, v))));
            format!("{col} LIKE ? ESCAPE '\\'")
        }
        Cond::Exact(v) => {
            p.push(SqlValue::Text(text_key(f, rules, v)));
            format!("{col} = ?")
        }
        Cond::Any(vs) if text => {
            let parts: Vec<String> = vs
                .iter()
                .map(|v| {
                    p.push(SqlValue::Text(contains(&text_key(f, rules, v))));
                    format!("{col} LIKE ? ESCAPE '\\'")
                })
                .collect();
            format!("({})", parts.join(" OR "))
        }
        Cond::Is(v) => {
            p.push(num(v));
            format!("{col} = ?")
        }
        Cond::Any(vs) => {
            p.extend(vs.iter().map(|v| num(v)));
            format!("{col} IN ({})", vec!["?"; vs.len()].join(","))
        }
        Cond::Cmp(op, n) => {
            p.push(SqlValue::Real(*n));
            format!("{col} {op} ?")
        }
        Cond::Range(a, b) => {
            let mut parts = Vec::new();
            if a.is_finite() {
                p.push(SqlValue::Real(*a));
                parts.push(format!("{col} >= ?"));
            }
            if b.is_finite() {
                p.push(SqlValue::Real(*b));
                parts.push(format!("{col} <= ?"));
            }
            if parts.is_empty() {
                format!("{col} IS NOT NULL")
            } else {
                parts.join(" AND ")
            }
        }
    }
}

fn to_sql(q: &Query, rules: &TextRules, grouped: bool) -> Result<Sql, QueryError> {
    let mut s = Sql {
        wheres: Vec::new(),
        varies: Vec::new(),
        params: Vec::new(),
    };
    for t in &q.terms {
        match t {
            Term::Words(w, neg) => {
                let Some(fts) = crate::queries::fts_query(rules, w) else {
                    continue;
                };
                s.params.push(SqlValue::Text(fts));
                let not = if *neg { "NOT " } else { "" };
                s.wheres.push(format!(
                    "t.id {not}IN (SELECT rowid FROM tracks_fts WHERE tracks_fts MATCH ?)"
                ));
            }
            Term::Field(f, c, neg) => {
                let sql = cond_sql(f, c, rules, &mut s.params);
                s.wheres.push(if *neg {
                    format!("NOT COALESCE(({sql}), 0)")
                } else {
                    sql
                });
            }
            Term::Missing(f, has) => {
                let missing = if f.name == "genre" {
                    "NOT EXISTS (SELECT 1 FROM track_genres WHERE track_id = t.id)".to_string()
                } else if f.kind == Kind::Text {
                    format!("COALESCE({}, '') = ''", f.expr)
                } else {
                    format!("{} IS NULL", f.expr)
                };
                s.wheres.push(if *has {
                    format!("NOT ({missing})")
                } else {
                    missing
                });
            }
            Term::Varies(f) => {
                if !grouped {
                    return err("varies: needs a grouping (albums, artists, …)");
                }
                s.varies.push(f);
            }
        }
    }
    Ok(s)
}

/// A grouped row's cell SQL for a column, over `e` (the column of the tracks' values).
fn agg_sql(f: &Field, e: &str) -> Vec<String> {
    match f.agg {
        Agg::Same => vec![format!("COUNT(DISTINCT {e})"), format!("MIN({e})")],
        Agg::Sum => vec![format!("SUM({e})")],
        Agg::Avg => vec![format!("AVG({e})")],
        Agg::Max => vec![format!("MAX({e})")],
        Agg::Range => vec![format!("MIN({e})"), format!("MAX({e})")],
    }
}

fn sql_to_json(v: SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(r) => json!(r),
        SqlValue::Text(s) => json!(s),
        SqlValue::Blob(_) => Value::Null,
    }
}

fn cell(f: &Field, kind: Kind, mut vals: Vec<Value>) -> Value {
    let fix = |v: Value| match (kind, &v) {
        (Kind::Bool, Value::Number(n)) => json!(n.as_f64().unwrap_or(0.0) != 0.0),
        _ => v,
    };
    match f.agg {
        Agg::Same => {
            let n = vals[0].as_i64().unwrap_or(0);
            if n > 1 {
                json!({ "varies": n })
            } else {
                fix(vals.remove(1))
            }
        }
        Agg::Range => {
            let (a, b) = (vals.remove(0), vals.remove(0));
            if a == b {
                fix(a)
            } else {
                json!([a, b])
            }
        }
        _ => fix(vals.remove(0)),
    }
}

// -------------------------------------------------------------------------------- running

/// One request of the Table.
#[derive(Debug, Clone, Default)]
pub struct TableQuery {
    pub q: String,
    /// A groupable field's name; `None` or `track`: one row per track.
    pub group: Option<String>,
    /// Field names, and `count` for grouped rows.
    pub columns: Vec<String>,
    /// Field names (or `count`), descending when `true`.
    pub sort: Vec<(String, bool)>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TableRow {
    /// The track (rows of tracks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// What a grouped row stands for (an album id, a year, …) and its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<Value>,
    /// The query text that finds a grouped row's tracks, and the grouping to show them in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<&'static str>,
    /// One per column: a value, `null`, `{"varies": n}` or `[min, max]`.
    pub cells: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TablePage {
    /// Rows in all.
    pub total: usize,
    pub offset: usize,
    pub rows: Vec<TableRow>,
    /// Tracks matching, and their length in all (seconds).
    pub tracks: i64,
    pub duration: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum TableError {
    #[error("{0}")]
    Query(#[from] QueryError),
    #[error("{0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// A whole result, kept for the next pages.
enum Whole {
    /// Track ids in order.
    Tracks(Vec<i64>),
    /// Every grouped row.
    Groups(Vec<TableRow>),
}

struct Cached {
    key: String,
    at: Instant,
    whole: Arc<Whole>,
    tracks: i64,
    duration: f64,
}

/// Results kept, newest first.
#[derive(Default)]
pub(crate) struct Cache(Vec<Cached>);
const CACHE_SIZE: usize = 4;
const CACHE_FOR: Duration = Duration::from_secs(600);

/// `file:///a/b%20c.flac#1-2` → `/a/b c.flac`.
fn url_path(url: &str) -> String {
    let u = url.split('#').next().unwrap_or(url);
    let p = u.strip_prefix("file://").unwrap_or(u);
    percent_encoding::percent_decode_str(p)
        .decode_utf8_lossy()
        .into_owned()
}

fn add_functions(c: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    c.create_scalar_function("rms_path", 1, flags, |ctx| {
        Ok(ctx.get::<Option<String>>(0)?.map(|u| url_path(&u)))
    })?;
    c.create_scalar_function("rms_dir", 1, flags, |ctx| {
        Ok(ctx.get::<Option<String>>(0)?.map(|u| {
            let p = url_path(&u);
            match p.rfind('/') {
                Some(i) if i > 0 => p[..i].to_string(),
                _ => p,
            }
        }))
    })
}

fn order_term(expr: &str, desc: bool) -> String {
    format!(
        "({expr}) IS NULL, {expr}{}",
        if desc { " DESC" } else { "" }
    )
}

fn columns(names: &[String]) -> Result<Vec<Option<&'static Field>>, QueryError> {
    names
        .iter()
        .map(|n| {
            if n == COUNT {
                Ok(None)
            } else {
                field_named(n)
                    .filter(|f| !f.hidden)
                    .map(Some)
                    .ok_or_else(|| QueryError(format!("no column called {n:?}")))
            }
        })
        .collect()
}

/// Track rows: ordered ids.
fn track_ids(c: &Connection, s: &Sql, sort: &[(String, bool)]) -> Result<Vec<i64>, TableError> {
    let mut order: Vec<String> = Vec::new();
    for (name, desc) in sort {
        let f = field_named(name).ok_or_else(|| QueryError(format!("can't sort by {name:?}")))?;
        order.push(order_term(f.sort_expr(), *desc));
    }
    // the library's own order last: album artist, album, disc, track
    order.push("aa.name_sort IS NULL, aa.name_sort, al.title_sort, t.disc, t.track_number, t.title_sort, t.id".into());
    let wheres = s.wheres();
    let order = order.join(", ");
    let sql = format!(
        "SELECT t.id {} WHERE {wheres} ORDER BY {order}",
        from(&format!("{wheres} {order}"))
    );
    let ids = c
        .prepare(&sql)?
        .query_map(params_from_iter(s.params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// Rows for track ids, in their order.
fn track_rows(
    c: &Connection,
    ids: &[i64],
    cols: &[Option<&'static Field>],
) -> Result<Vec<TableRow>, TableError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let exprs: Vec<String> = cols
        .iter()
        .map(|f| f.map(|f| f.expr).unwrap_or("1").to_string())
        .collect();
    let select = if exprs.is_empty() {
        String::new()
    } else {
        format!(", {}", exprs.join(", "))
    };
    let list = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT t.id{select} {} WHERE t.id IN ({list})",
        from(&select)
    );
    let mut st = c.prepare(&sql)?;
    let mut by_id: HashMap<i64, Vec<Value>> = HashMap::new();
    let mut rows = st.query(params_from_iter(ids.iter()))?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let mut cells = Vec::with_capacity(cols.len());
        for (i, f) in cols.iter().enumerate() {
            let v = sql_to_json(r.get::<_, SqlValue>(i + 1)?);
            cells.push(match f {
                Some(f) if f.kind == Kind::Bool && v.is_number() => json!(v.as_f64() != Some(0.0)),
                // a track is one track
                None => json!(1),
                _ => v,
            });
        }
        by_id.insert(id, cells);
    }
    Ok(ids
        .iter()
        .filter_map(|id| {
            by_id.remove(id).map(|cells| TableRow {
                id: Some(*id),
                key: None,
                label: None,
                drill: None,
                next: None,
                cells,
            })
        })
        .collect())
}

/// The tracks of a grouping, one row per track (per genre of a track, grouping by genre),
/// with what the grouped rows read of them: `k` the group, `l` its name, `gs` its sort key,
/// `tid`, `dur`, `c0…` the columns, `s0…` the sort fields, `h0…` the `varies:` fields.
fn group_inner(
    s: &Sql,
    g: &'static Field,
    cols: &[Option<&'static Field>],
    sort: &[&'static Field],
) -> String {
    let group = g.group.as_ref().expect("groupable");
    let mut sel = vec![
        format!("{} AS k", g.group_key()),
        format!("{} AS l", g.group_label()),
        format!("{} AS gs", g.group_sort()),
        "t.id AS tid".to_string(),
        "t.duration AS dur".to_string(),
    ];
    for (i, f) in cols.iter().enumerate() {
        sel.push(format!("{} AS c{i}", f.map(|f| f.expr).unwrap_or("NULL")));
    }
    for (i, f) in sort.iter().enumerate() {
        sel.push(format!("{} AS s{i}", f.sort_expr()));
    }
    for (i, f) in s.varies.iter().enumerate() {
        sel.push(format!("{} AS h{i}", f.expr));
    }
    let sel = sel.join(", ");
    let wheres = s.wheres();
    format!(
        "SELECT {sel} {} {} WHERE {wheres}",
        from(&format!("{sel} {wheres}")),
        group.join
    )
}

fn having(s: &Sql) -> String {
    if s.varies.is_empty() {
        return String::new();
    }
    let h: Vec<String> = (0..s.varies.len())
        .map(|i| format!("COUNT(DISTINCT h{i}) > 1"))
        .collect();
    format!("HAVING {}", h.join(" AND "))
}

/// Every grouped row, in order.
fn group_rows(
    c: &Connection,
    s: &Sql,
    g: &'static Field,
    cols: &[Option<&'static Field>],
    sort: &[(String, bool)],
) -> Result<Vec<TableRow>, TableError> {
    let group = g.group.as_ref().expect("groupable");
    let sort_fields: Vec<(Option<&'static Field>, bool)> = sort
        .iter()
        .map(|(name, desc)| {
            if name == COUNT {
                Ok((None, *desc))
            } else {
                field_named(name)
                    .map(|f| (Some(f), *desc))
                    .ok_or_else(|| QueryError(format!("can't sort by {name:?}")))
            }
        })
        .collect::<Result<_, _>>()?;
    let by: Vec<&'static Field> = sort_fields.iter().filter_map(|(f, _)| *f).collect();
    let inner = group_inner(s, g, cols, &by);
    let mut select = vec!["k".to_string(), "MIN(l)".to_string()];
    let mut spans = Vec::new();
    for (i, f) in cols.iter().enumerate() {
        let parts = match f {
            Some(f) => agg_sql(f, &format!("c{i}")),
            None => vec!["COUNT(DISTINCT tid)".to_string()],
        };
        spans.push(parts.len());
        select.extend(parts);
    }
    let mut order = Vec::new();
    let mut j = 0;
    for (f, desc) in &sort_fields {
        let expr = match f {
            None => "COUNT(DISTINCT tid)".to_string(),
            Some(f) => {
                let e = format!("s{j}");
                j += 1;
                match f.agg {
                    Agg::Sum => format!("SUM({e})"),
                    Agg::Avg => format!("AVG({e})"),
                    Agg::Max => format!("MAX({e})"),
                    Agg::Same | Agg::Range => format!("MIN({e})"),
                }
            }
        };
        order.push(order_term(&expr, *desc));
    }
    order.push(order_term("MIN(gs)", false));
    order.push("k".into());
    let sql = format!(
        "SELECT {} FROM ({inner}) GROUP BY k {} ORDER BY {}",
        select.join(", "),
        having(s),
        order.join(", ")
    );
    let mut st = c.prepare(&sql)?;
    let mut rows = st.query(params_from_iter(s.params.iter()))?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let key = sql_to_json(r.get::<_, SqlValue>(0)?);
        let label = sql_to_json(r.get::<_, SqlValue>(1)?);
        let mut i = 2;
        let mut cells = Vec::with_capacity(cols.len());
        for (f, n) in cols.iter().zip(&spans) {
            let vals: Vec<Value> = (i..i + n)
                .map(|j| r.get::<_, SqlValue>(j).map(sql_to_json))
                .collect::<rusqlite::Result<_>>()?;
            i += n;
            cells.push(match f {
                Some(f) => cell(f, f.kind, vals),
                None => vals.into_iter().next().unwrap_or(Value::Null),
            });
        }
        let drill = match (group.drill, &key) {
            (Some(_), Value::Null) => format!("missing:{}", g.name),
            (Some(pattern), k) => pattern.replace("{}", &k.to_string()),
            (None, k) => {
                let k = match (g.kind, k) {
                    (Kind::Bool, Value::Number(n)) => json!(n.as_f64() != Some(0.0)),
                    _ => k.clone(),
                };
                exact(g.name, &k)
            }
        };
        out.push(TableRow {
            id: None,
            key: Some(key),
            label: Some(label),
            drill: Some(drill),
            next: Some(group.next),
            cells,
        });
    }
    Ok(out)
}

impl Library {
    /// A page of the Table.  `now`: Unix time, for ages in the query.
    pub fn table(&self, tq: &TableQuery, now: i64) -> Result<TablePage, TableError> {
        let group = match tq.group.as_deref() {
            None | Some("") | Some("track") => None,
            Some(name) => Some(
                field_named(name)
                    .filter(|f| f.groupable())
                    .ok_or_else(|| QueryError(format!("can't group by {name:?}")))?,
            ),
        };
        let cols = columns(&tq.columns)?;
        let sort_key: Vec<String> = tq
            .sort
            .iter()
            .map(|(n, d)| format!("{n}{}", if *d { "-" } else { "+" }))
            .collect();
        let key = format!(
            "{}\u{1}{}\u{1}{}\u{1}{}",
            tq.q,
            group.map(|g| g.name).unwrap_or("track"),
            sort_key.join(","),
            if group.is_some() {
                tq.columns.join(",")
            } else {
                String::new()
            }
        );
        let cached = if tq.offset > 0 {
            let cache = self.inner.table.lock();
            cache
                .0
                .iter()
                .find(|c| c.key == key && c.at.elapsed() < CACHE_FOR)
                .map(|c| (c.whole.clone(), c.tracks, c.duration))
        } else {
            None
        };
        let c = self.conn()?;
        add_functions(&c)?;
        let (whole, tracks, duration) = match cached {
            Some(hit) => hit,
            None => {
                let cfg = self.config();
                let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
                let query = parse(&tq.q, now)?;
                let s = to_sql(&query, &rules, group.is_some())?;
                let wheres = s.wheres();
                let (mut tracks, mut duration): (i64, f64) = c.query_row(
                    &format!(
                        "SELECT COUNT(*), COALESCE(SUM(t.duration), 0) {} WHERE {wheres}",
                        from(&wheres)
                    ),
                    params_from_iter(s.params.iter()),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let whole = Arc::new(match group {
                    None => Whole::Tracks(track_ids(&c, &s, &tq.sort)?),
                    Some(g) => {
                        if !s.varies.is_empty() {
                            // only the tracks of the groups left
                            let sql = format!(
                                "WITH x AS ({}) SELECT COUNT(*), COALESCE(SUM(dur), 0) FROM (SELECT DISTINCT tid, dur FROM x \
                                 WHERE k IN (SELECT k FROM x GROUP BY k {}))",
                                group_inner(&s, g, &[], &[]),
                                having(&s)
                            );
                            (tracks, duration) =
                                c.query_row(&sql, params_from_iter(s.params.iter()), |r| {
                                    Ok((r.get(0)?, r.get(1)?))
                                })?;
                        }
                        Whole::Groups(group_rows(&c, &s, g, &cols, &tq.sort)?)
                    }
                });
                let mut cache = self.inner.table.lock();
                cache.0.retain(|c| c.key != key);
                cache.0.insert(
                    0,
                    Cached {
                        key,
                        at: Instant::now(),
                        whole: whole.clone(),
                        tracks,
                        duration,
                    },
                );
                cache.0.truncate(CACHE_SIZE);
                (whole, tracks, duration)
            }
        };
        let limit = tq.limit.clamp(1, 1000);
        let (total, rows) = match &*whole {
            Whole::Tracks(ids) => {
                let page = ids
                    .iter()
                    .skip(tq.offset)
                    .take(limit)
                    .copied()
                    .collect::<Vec<_>>();
                (ids.len(), track_rows(&c, &page, &cols)?)
            }
            Whole::Groups(rows) => (
                rows.len(),
                rows.iter().skip(tq.offset).take(limit).cloned().collect(),
            ),
        };
        Ok(TablePage {
            total,
            offset: tq.offset,
            rows,
            tracks,
            duration,
        })
    }

    /// The tracks a query finds, in the Table's order (for playing them), at most `limit`.
    pub fn table_track_ids(
        &self,
        q: &str,
        sort: &[(String, bool)],
        limit: usize,
        now: i64,
    ) -> Result<Vec<i64>, TableError> {
        let cfg = self.config();
        let rules = TextRules::new(&cfg.ignored_articles, &cfg.split_list);
        let query = parse(q, now)?;
        let s = to_sql(&query, &rules, false)?;
        let c = self.conn()?;
        add_functions(&c)?;
        let mut ids = track_ids(&c, &s, sort)?;
        ids.truncate(limit);
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn conds(q: &str) -> Vec<String> {
        parse(q, NOW)
            .unwrap()
            .terms
            .iter()
            .map(|t| format!("{t:?}"))
            .collect()
    }

    #[test]
    fn tokens_keep_quoted_words_together() {
        assert_eq!(
            tokens(r#"artist:"Sigur Rós" -live "re:birth" title:"say \"hi\"""#),
            vec![
                ("artist:Sigur Rós".to_string(), false),
                ("-live".to_string(), false),
                ("re:birth".to_string(), true),
                ("title:say \"hi\"".to_string(), false),
            ]
        );
    }

    #[test]
    fn fields_and_words() {
        let q = parse(
            "blue year:1990..1999 bpm:>120 -format:mp3 genre:jazz,soul",
            NOW,
        )
        .unwrap();
        assert_eq!(q.terms.len(), 5);
        assert!(matches!(&q.terms[0], Term::Words(w, false) if w == "blue"));
        assert!(
            matches!(&q.terms[1], Term::Field(f, Cond::Range(a, b), false) if f.name == "year" && *a == 1990.0 && *b == 1999.0)
        );
        assert!(
            matches!(&q.terms[2], Term::Field(f, Cond::Cmp(">", n), false) if f.name == "bpm" && *n == 120.0)
        );
        assert!(
            matches!(&q.terms[3], Term::Field(f, Cond::Is(v), true) if f.name == "format" && v == "mp3")
        );
        assert!(
            matches!(&q.terms[4], Term::Field(f, Cond::Any(v), false) if f.name == "genre" && v == &["jazz", "soul"])
        );
    }

    #[test]
    fn units() {
        let one = |q: &str| conds(q).remove(0);
        assert!(one("duration:>3:30").contains("Cmp(\">\", 210.0)"));
        assert!(one("duration:<5m").contains("Cmp(\"<\", 300.0)"));
        assert!(one("size:>1.5gb").contains("Cmp(\">\", 1500000000.0)"));
        assert!(one("bitrate:>=320").contains("Cmp(\">=\", 320000.0)"));
        assert!(one("lossless:yes").contains("Is(\"1\")"));
        // added in the last 30 days; played more than a year ago
        assert!(one("added:<30d").contains(&format!("Cmp(\">\", {}.0)", NOW - 30 * 86_400)));
        assert!(one("last_played:>1y").contains("Cmp(\"<\""));
        // a year of dates
        let r = one("added:2024");
        assert!(r.contains("Range(1704067200.0, 1735689599.0)"), "{r}");
        assert!(one("added:2023..2024-06").contains("Range(1672531200.0, 1719791999.0)"));
    }

    #[test]
    fn mistakes_are_explained() {
        let e = |q: &str| parse(q, NOW).unwrap_err().0;
        assert!(e("colour:red").contains("no field called \"colour\""));
        assert!(e("bpm:fast").contains("isn't a number"));
        assert!(e("year:").contains("what value"));
        assert!(e("added:soon").contains("an age"));
        // a word with a colon, quoted, is a word
        assert!(parse("\"re:birth\"", NOW).is_ok());
    }

    #[test]
    fn missing_has_varies() {
        let c = conds("missing:mbid has:genre varies:album_artist");
        assert!(c[0].starts_with("Missing(") && c[0].ends_with("false)"));
        assert!(c[1].ends_with("true)"));
        assert!(c[2].starts_with("Varies("));
    }

    #[test]
    fn drill_filters_read_back() {
        for (f, v) in [
            ("format", json!("flc")),
            ("year", json!(1995)),
            ("folder", json!("/m/a \"b\"")),
        ] {
            let q = exact(f, &v);
            assert!(parse(&q, NOW).is_ok(), "{q}");
        }
        assert_eq!(exact("year", &Value::Null), "missing:year");
        assert_eq!(exact("lossless", &json!(true)), "lossless:yes");
    }

    #[test]
    fn every_field_is_usable() {
        let rules = TextRules::new("", "");
        for f in FIELDS {
            let v = match f.kind {
                Kind::Text => "x",
                Kind::Time => "30d",
                Kind::Bool => "yes",
                _ => "1",
            };
            let q = parse(&format!("{}:{v} missing:{}", f.name, f.name), NOW).unwrap();
            to_sql(&q, &rules, false).unwrap();
            if let Some(next) = f.next_group() {
                assert!(
                    next == "track" || field_named(next).is_some_and(|n| n.groupable()),
                    "{}",
                    f.name
                );
            }
        }
    }

    #[test]
    fn paths_from_urls() {
        assert_eq!(url_path("file:///m/A%20B/01.flac#10-20"), "/m/A B/01.flac");
    }
}
