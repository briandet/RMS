//! Typed views of the library for APIs that are not LMS's (OpenSubsonic): album artists,
//! albums with their totals, album lists in the usual orders, genres and search, straight
//! from the schema in `db.rs`.

use crate::text::TextRules;
use crate::track::{self, Track};
use crate::Library;
use rusqlite::{params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Row};

/// An artist who has albums.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistInfo {
    pub id: i64,
    pub name: String,
    pub sort_name: String,
    pub album_count: i64,
    /// Served at `/contributor/<portrait_id>/image`.
    pub portrait_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    pub id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub artist_id: Option<i64>,
    pub year: Option<i64>,
    /// The first genre of its tracks.
    pub genre: Option<String>,
    pub song_count: i64,
    /// Seconds.
    pub duration: f64,
    /// Unix time its first track was added.
    pub added_at: i64,
    pub cover_id: Option<String>,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub compilation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenreInfo {
    pub name: String,
    pub song_count: i64,
    pub album_count: i64,
}

/// Orders of [`Library::album_list`].
#[derive(Debug, Clone, PartialEq)]
pub enum AlbumOrder {
    Random,
    /// Most recently added first.
    Newest,
    /// Most played first.
    Frequent,
    /// Most recently played first.
    Recent,
    /// Highest rated (by their tracks' average) first.
    Highest,
    Title,
    /// By album artist, then year and title.
    Artist,
    /// Albums of these years, in this direction (`from > to`: newest first).
    Years {
        from: i64,
        to: i64,
    },
    /// Albums with a track of this genre (by name), by title.
    Genre(String),
    /// These albums, in this order.
    Ids(Vec<i64>),
}

/// Search results of [`Library::search_all`].
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub artists: Vec<ArtistInfo>,
    pub albums: Vec<AlbumInfo>,
    pub tracks: Vec<Track>,
}

/// One page: `offset` entries skipped, at most `count` returned.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub offset: usize,
    pub count: usize,
}

impl Window {
    pub fn new(offset: usize, count: usize) -> Window {
        Window { offset, count }
    }

    fn sql(self) -> String {
        format!(
            " LIMIT {} OFFSET {}",
            self.count.min(i64::MAX as usize),
            self.offset
        )
    }
}

const ALBUM_SELECT: &str = "
SELECT albums.id, albums.title, artists.name, albums.artist_id, albums.year,
       (SELECT genres.name FROM tracks t2 JOIN track_genres tg ON tg.track_id = t2.id
          JOIN genres ON genres.id = tg.genre_id WHERE t2.album_id = albums.id
          ORDER BY genres.id LIMIT 1),
       COUNT(tracks.id), COALESCE(SUM(tracks.duration), 0), COALESCE(MIN(tracks.added_at), 0),
       albums.artwork, COALESCE(SUM(track_stats.play_count), 0), MAX(track_stats.last_played),
       COALESCE(albums.compilation, 0)
FROM albums
LEFT JOIN artists ON artists.id = albums.artist_id
LEFT JOIN tracks ON tracks.album_id = albums.id
LEFT JOIN track_stats ON track_stats.url = tracks.url
";

fn album_row(r: &Row) -> rusqlite::Result<AlbumInfo> {
    Ok(AlbumInfo {
        id: r.get(0)?,
        title: r.get(1)?,
        artist: r.get(2)?,
        artist_id: r.get(3)?,
        year: r.get::<_, Option<i64>>(4)?.filter(|&y| y > 0),
        genre: r.get(5)?,
        song_count: r.get(6)?,
        duration: r.get(7)?,
        added_at: r.get(8)?,
        cover_id: r
            .get::<_, Option<String>>(9)?
            .filter(|c| !c.is_empty() && c != "0"),
        play_count: r.get(10)?,
        last_played: r.get(11)?,
        compilation: r.get::<_, i64>(12)? != 0,
    })
}

const ARTIST_SELECT: &str = "
SELECT artists.id, artists.name, artists.name_sort, COUNT(DISTINCT albums.id), artists.portrait_id
FROM artists JOIN albums ON albums.artist_id = artists.id
";

fn artist_row(r: &Row) -> rusqlite::Result<ArtistInfo> {
    Ok(ArtistInfo {
        id: r.get(0)?,
        name: r.get(1)?,
        sort_name: r.get(2)?,
        album_count: r.get(3)?,
        portrait_id: r.get(4)?,
    })
}

fn query<T>(
    c: &Connection,
    sql: &str,
    params: &[SqlValue],
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> Vec<T> {
    let run = || -> rusqlite::Result<Vec<T>> {
        let mut st = c.prepare(sql)?;
        let rows = st.query_map(params_from_iter(params.iter()), f)?;
        rows.collect()
    };
    match run() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("catalog query failed: {e}: {sql}");
            Vec::new()
        }
    }
}

/// Rows of a full-text index matching every word of `search`, best first: a `WITH m`
/// (id, rank) prefix and the join onto it, with their params; everything (rank 0) when
/// `search` has no words.  Materialized, so that `bm25` stays in its own query.
fn ranked(
    rules: &TextRules,
    search: &str,
    index: &str,
    alias: &str,
) -> (String, String, Vec<SqlValue>) {
    match crate::queries::fts_query(rules, search) {
        Some(q) => (
            format!(
                "WITH m AS MATERIALIZED (SELECT rowid AS id, bm25({index}) AS rank FROM {index}
                   WHERE {index} MATCH ?)"
            ),
            format!("JOIN m ON m.id = {alias}.id"),
            vec![SqlValue::Text(q)],
        ),
        None => (
            "WITH m AS (SELECT 0 AS rank)".into(),
            "CROSS JOIN m".into(),
            Vec::new(),
        ),
    }
}

impl Library {
    pub(crate) fn rules(&self) -> TextRules {
        let cfg = self.config();
        TextRules::new(&cfg.ignored_articles, &cfg.split_list)
    }

    /// Artists with albums (as album artist), by sort name.
    pub fn album_artists(&self) -> Vec<ArtistInfo> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "{ARTIST_SELECT} GROUP BY artists.id ORDER BY artists.name_sort COLLATE NOCASE, artists.id"
        );
        query(&c, &sql, &[], artist_row)
    }

    /// Any artist, with the number of albums they are album artist of.
    pub fn artist_info(&self, id: i64) -> Option<ArtistInfo> {
        let c = self.conn().ok()?;
        let sql = "SELECT artists.id, artists.name, artists.name_sort,
                (SELECT COUNT(*) FROM albums WHERE albums.artist_id = artists.id), artists.portrait_id
            FROM artists WHERE artists.id = ?";
        query(&c, sql, &[SqlValue::Integer(id)], artist_row)
            .into_iter()
            .next()
    }

    /// The albums of an album artist, by year and title; for an artist without albums of
    /// their own, the albums they appear on.
    pub fn artist_albums(&self, artist_id: i64) -> Vec<AlbumInfo> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let order = " GROUP BY albums.id ORDER BY albums.year, albums.title_sort COLLATE NOCASE";
        let own = format!("{ALBUM_SELECT} WHERE albums.artist_id = ?1 {order}");
        let p = [SqlValue::Integer(artist_id)];
        let albums = query(&c, &own, &p, album_row);
        if !albums.is_empty() {
            return albums;
        }
        let appears = format!(
            "{ALBUM_SELECT} WHERE albums.id IN (SELECT tracks.album_id FROM tracks
               JOIN track_artists ta ON ta.track_id = tracks.id WHERE ta.artist_id = ?1) {order}"
        );
        query(&c, &appears, &p, album_row)
    }

    pub fn album_info(&self, id: i64) -> Option<AlbumInfo> {
        let c = self.conn().ok()?;
        let sql = format!("{ALBUM_SELECT} WHERE albums.id = ? GROUP BY albums.id");
        query(&c, &sql, &[SqlValue::Integer(id)], album_row)
            .into_iter()
            .next()
    }

    pub fn album_list(&self, order: &AlbumOrder, w: Window) -> Vec<AlbumInfo> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let by_title = "albums.title_sort COLLATE NOCASE, albums.id";
        let mut params = Vec::new();
        let (filter, having, sort) = match order {
            AlbumOrder::Random => ("", "", "RANDOM()".to_string()),
            AlbumOrder::Newest => ("", "", format!("MIN(tracks.added_at) DESC, {by_title}")),
            AlbumOrder::Frequent => (
                "",
                "HAVING SUM(track_stats.play_count) > 0",
                format!("SUM(track_stats.play_count) DESC, {by_title}"),
            ),
            AlbumOrder::Recent => (
                "",
                "HAVING MAX(track_stats.last_played) IS NOT NULL",
                format!("MAX(track_stats.last_played) DESC, {by_title}"),
            ),
            AlbumOrder::Highest => (
                "",
                "HAVING AVG(track_stats.rating) > 0",
                format!("AVG(track_stats.rating) DESC, {by_title}"),
            ),
            AlbumOrder::Title => ("", "", by_title.to_string()),
            AlbumOrder::Artist => (
                "",
                "",
                format!("artists.name_sort COLLATE NOCASE, albums.year, {by_title}"),
            ),
            AlbumOrder::Years { from, to } => {
                params.push(SqlValue::Integer(*from.min(to)));
                params.push(SqlValue::Integer(*from.max(to)));
                let dir = if from > to { "DESC" } else { "ASC" };
                (
                    "WHERE albums.year BETWEEN ? AND ?",
                    "",
                    format!("albums.year {dir}, {by_title}"),
                )
            }
            AlbumOrder::Genre(name) => {
                params.push(SqlValue::Text(name.clone()));
                (
                    "WHERE albums.id IN (SELECT t.album_id FROM tracks t
                       JOIN track_genres tg ON tg.track_id = t.id
                       JOIN genres g ON g.id = tg.genre_id WHERE g.name = ? COLLATE NOCASE)",
                    "",
                    by_title.to_string(),
                )
            }
            AlbumOrder::Ids(ids) => {
                if ids.is_empty() {
                    return Vec::new();
                }
                let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                let sql = format!("{ALBUM_SELECT} WHERE albums.id IN ({list}) GROUP BY albums.id");
                let mut found = query(&c, &sql, &[], album_row);
                found.sort_by_key(|a| ids.iter().position(|i| *i == a.id));
                return found.into_iter().skip(w.offset).take(w.count).collect();
            }
        };
        let sql = format!(
            "{ALBUM_SELECT} {filter} GROUP BY albums.id {having} ORDER BY {sort}{}",
            w.sql()
        );
        query(&c, &sql, &params, album_row)
    }

    /// The tracks of an album in disc and track order.
    pub fn album_tracks(&self, album_id: i64) -> Vec<Track> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let ids = query(
            &c,
            "SELECT id FROM tracks WHERE album_id = ?
             ORDER BY COALESCE(disc, 1), COALESCE(track_number, 0), title_sort, id",
            &[SqlValue::Integer(album_id)],
            |r| r.get(0),
        );
        track::load_many(&c, &ids)
    }

    pub fn genre_list(&self) -> Vec<GenreInfo> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        query(
            &c,
            "SELECT genres.name, COUNT(DISTINCT tracks.id), COUNT(DISTINCT tracks.album_id)
             FROM genres JOIN track_genres tg ON tg.genre_id = genres.id
             JOIN tracks ON tracks.id = tg.track_id
             GROUP BY genres.id ORDER BY genres.name_sort COLLATE NOCASE",
            &[],
            |r| {
                Ok(GenreInfo {
                    name: r.get(0)?,
                    song_count: r.get(1)?,
                    album_count: r.get(2)?,
                })
            },
        )
    }

    /// Random tracks, optionally of a genre (by name) and of years `from..=to`.
    pub fn random_track_list(
        &self,
        count: usize,
        genre: Option<&str>,
        from_year: Option<i64>,
        to_year: Option<i64>,
    ) -> Vec<Track> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let mut w = Vec::new();
        let mut p = Vec::new();
        if let Some(g) = genre {
            w.push(
                "id IN (SELECT tg.track_id FROM track_genres tg JOIN genres g ON g.id = tg.genre_id
                   WHERE g.name = ? COLLATE NOCASE)",
            );
            p.push(SqlValue::Text(g.to_string()));
        }
        if let Some(y) = from_year {
            w.push("year >= ?");
            p.push(SqlValue::Integer(y));
        }
        if let Some(y) = to_year {
            w.push("year <= ?");
            p.push(SqlValue::Integer(y));
        }
        let filter = if w.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", w.join(" AND "))
        };
        let sql = format!("SELECT id FROM tracks {filter} ORDER BY RANDOM() LIMIT {count}");
        let ids = query(&c, &sql, &p, |r| r.get(0));
        track::load_many(&c, &ids)
    }

    /// Tracks of a genre (by name), by album and track.
    pub fn genre_tracks(&self, genre: &str, w: Window) -> Vec<Track> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "SELECT tracks.id FROM tracks JOIN track_genres tg ON tg.track_id = tracks.id
             JOIN genres g ON g.id = tg.genre_id
             LEFT JOIN albums ON albums.id = tracks.album_id
             WHERE g.name = ? COLLATE NOCASE
             ORDER BY albums.title_sort COLLATE NOCASE, tracks.album_id, tracks.disc,
                      tracks.track_number, tracks.id{}",
            w.sql()
        );
        let ids = query(&c, &sql, &[SqlValue::Text(genre.to_string())], |r| r.get(0));
        track::load_many(&c, &ids)
    }

    /// Album artists, albums and tracks matching every word of `search` (all of them when
    /// it is empty), each in its own window.
    pub fn search_all(
        &self,
        search: &str,
        artists: Window,
        albums: Window,
        tracks: Window,
    ) -> Found {
        let Ok(c) = self.conn() else {
            return Found::default();
        };
        let rules = self.rules();
        let mut found = Found::default();
        // best matches first, then by name
        if artists.count > 0 {
            let (with, m, p) = ranked(&rules, search, "artists_fts", "artists");
            let sql = format!(
                "{with} {ARTIST_SELECT} {m} GROUP BY artists.id
                 ORDER BY m.rank, artists.name_sort COLLATE NOCASE, artists.id{}",
                artists.sql()
            );
            found.artists = query(&c, &sql, &p, artist_row);
        }
        if albums.count > 0 {
            let (with, m, p) = ranked(&rules, search, "albums_fts", "albums");
            let sql = format!(
                "{with} {ALBUM_SELECT} {m} GROUP BY albums.id
                 ORDER BY m.rank, albums.title_sort COLLATE NOCASE, albums.id{}",
                albums.sql()
            );
            found.albums = query(&c, &sql, &p, album_row);
        }
        if tracks.count > 0 {
            let (with, m, p) = ranked(&rules, search, "tracks_fts", "tracks");
            let sql = format!(
                "{with} SELECT tracks.id FROM tracks {m}
                 ORDER BY m.rank, tracks.title_sort COLLATE NOCASE, tracks.id{}",
                tracks.sql()
            );
            let ids = query(&c, &sql, &p, |r| r.get(0));
            found.tracks = track::load_many(&c, &ids);
        }
        found
    }

    /// Tracks by id, in the given order (unknown ids left out).
    pub fn tracks_by_id(&self, ids: &[i64]) -> Vec<Track> {
        match self.conn() {
            Ok(c) => track::load_many(&c, ids),
            Err(_) => Vec::new(),
        }
    }

    /// Ids of the albums whose (title, album artist) are among `keys`, matched without
    /// regard to case; the artist is ignored when `None`.
    pub fn albums_named(&self, keys: &[(String, Option<String>)]) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (title, artist) in keys {
            let sql =
                "SELECT albums.id FROM albums LEFT JOIN artists ON artists.id = albums.artist_id
                       WHERE albums.title = ?1 COLLATE NOCASE
                         AND (?2 IS NULL OR artists.name = ?2 COLLATE NOCASE)
                       ORDER BY albums.id LIMIT 1";
            let p = [
                SqlValue::Text(title.clone()),
                artist.clone().map(SqlValue::Text).unwrap_or(SqlValue::Null),
            ];
            out.extend(query(&c, sql, &p, |r| r.get::<_, i64>(0)));
        }
        out
    }

    /// Ids of the artists with these names (without regard to case).
    pub fn artists_named(&self, names: &[String]) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for n in names {
            out.extend(query(
                &c,
                "SELECT id FROM artists WHERE name = ? COLLATE NOCASE ORDER BY id LIMIT 1",
                &[SqlValue::Text(n.clone())],
                |r| r.get::<_, i64>(0),
            ));
        }
        out
    }
}

impl Library {
    /// The artist's most played tracks (played at least once), most played first.
    pub fn most_played_by_artist(&self, artist_id: i64, n: usize) -> Vec<i64> {
        let Ok(c) = self.conn() else {
            return Vec::new();
        };
        let Ok(mut st) = c.prepare(&format!(
            "SELECT t.id FROM tracks t
             JOIN track_artists ta ON ta.track_id = t.id
             JOIN track_stats s ON s.url = t.url
             WHERE ta.artist_id = ? AND s.play_count > 0
             GROUP BY t.id
             ORDER BY MAX(s.play_count) DESC, MAX(s.last_played) DESC
             LIMIT {n}"
        )) else {
            return Vec::new();
        };
        st.query_map([artist_id], |r| r.get(0))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The artist's MusicBrainz id: from the tags, else as its tracks were identified.
    pub fn artist_mbid(&self, artist_id: i64) -> Option<String> {
        let c = self.conn().ok()?;
        let tagged: Option<String> = c
            .query_row("SELECT mbid FROM artists WHERE id = ?", [artist_id], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
            .flatten();
        if tagged.is_some() {
            return tagged;
        }
        c.query_row(
            "SELECT m.artist_mbid FROM tracks t
             JOIN track_musicbrainz m ON m.fingerprint = t.fingerprint
             WHERE t.artist_id = ? AND m.artist_mbid IS NOT NULL
             GROUP BY m.artist_mbid ORDER BY COUNT(*) DESC LIMIT 1",
            [artist_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// The library's artist with this MusicBrainz id, else this name.
    pub fn artist_in_library(&self, mbid: Option<&str>, name: &str) -> Option<i64> {
        let c = self.conn().ok()?;
        if let Some(m) = mbid {
            let id = c
                .query_row("SELECT id FROM artists WHERE mbid = ? LIMIT 1", [m], |r| {
                    r.get(0)
                })
                .optional()
                .ok()
                .flatten();
            if id.is_some() {
                return id;
            }
        }
        c.query_row(
            "SELECT id FROM artists WHERE name_search = ? LIMIT 1",
            [self.rules().search_key(name)],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }
}
