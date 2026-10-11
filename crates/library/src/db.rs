//! The library database: schema, migrations and the connection pool.
//!
//! The schema is RMS's own (documented in `docs/library-schema.md`); the LMS queries in
//! [`crate::queries`] translate it into what Squeezebox clients expect.  `PRAGMA user_version`
//! holds the schema version and [`MIGRATIONS`] brings an older database up to date.
//!
//! A database left by an earlier RMS release, which used Lyrion's schema, is converted on
//! first open: the file is copied to `<name>.lms-backup` first, then play counts, ratings and
//! "added" dates move to `track_stats`, and sonic analysis / loudness results are kept aside
//! (`legacy_analysis`) until the next scan matches them to tracks again.  The rest of the
//! old library is rebuilt by that scan.  The conversion runs in one transaction: if anything
//! fails, the old database is left as it was.

use std::path::{Path, PathBuf};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension};

pub type DbPool = Pool<SqliteConnectionManager>;
pub type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

/// Schema version written to `PRAGMA user_version`.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// `MIGRATIONS[n]` takes a database from version `n` to `n + 1`.
pub const MIGRATIONS: &[&str] = &[SCHEMA_V1];

const SCHEMA_V1: &str = r#"
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- Everything the scanner found on disk: directories, audio files, cue sheets, playlist files.
CREATE TABLE files (
  id    INTEGER PRIMARY KEY,
  url   TEXT NOT NULL UNIQUE,
  kind  TEXT NOT NULL CHECK (kind IN ('dir', 'audio', 'cue', 'playlist')),
  -- how an audio file is split into tracks: whole (NULL), by its own cue sheet
  -- ('embedded') or by a separate .cue file ('external')
  cue   TEXT CHECK (cue IN ('embedded', 'external')),
  mtime INTEGER NOT NULL DEFAULT 0,
  size  INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX files_kind ON files (kind);

CREATE TABLE artists (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  name_sort   TEXT NOT NULL,
  name_search TEXT NOT NULL,
  mbid        TEXT,
  portrait    TEXT,  -- file URL of the artist's picture
  portrait_id TEXT   -- served at /contributor/<portrait_id>/image
) STRICT;
CREATE INDEX artists_name ON artists (name);
CREATE INDEX artists_name_sort ON artists (name_sort);
CREATE INDEX artists_name_search ON artists (name_search);
CREATE INDEX artists_mbid ON artists (mbid);
CREATE INDEX artists_portrait_id ON artists (portrait_id);

CREATE TABLE albums (
  id           INTEGER PRIMARY KEY,
  title        TEXT NOT NULL,
  title_sort   TEXT NOT NULL,
  title_search TEXT NOT NULL,
  -- title and artists' search keys, for multi-word search
  search_text  TEXT,
  -- album artist; Various Artists for a compilation
  artist_id    INTEGER REFERENCES artists (id) ON DELETE SET NULL,
  -- NULL while not yet decided by the scanner
  compilation  INTEGER CHECK (compilation IN (0, 1)),
  year         INTEGER NOT NULL DEFAULT 0,  -- 0: unknown
  disc         INTEGER,
  disc_count   INTEGER,
  release_type TEXT NOT NULL DEFAULT 'ALBUM',
  replay_gain  REAL,
  replay_peak  REAL,
  mbid         TEXT,
  artwork      TEXT  -- cover id of the track whose picture stands for the album
) STRICT;
CREATE INDEX albums_title ON albums (title);
CREATE INDEX albums_title_sort ON albums (title_sort);
CREATE INDEX albums_title_search ON albums (title_search);
CREATE INDEX albums_artist ON albums (artist_id);
CREATE INDEX albums_year ON albums (year);
CREATE INDEX albums_mbid ON albums (mbid);

CREATE TABLE works (
  id           INTEGER PRIMARY KEY,
  composer_id  INTEGER NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
  title        TEXT NOT NULL,
  title_sort   TEXT NOT NULL,
  title_search TEXT NOT NULL,
  UNIQUE (composer_id, title_search)
) STRICT;

CREATE TABLE genres (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL,
  name_sort   TEXT NOT NULL,
  name_search TEXT NOT NULL UNIQUE
) STRICT;
CREATE INDEX genres_name_sort ON genres (name_sort);

-- A playable track: a whole audio file, or a segment of one (cue sheets).
CREATE TABLE tracks (
  id            INTEGER PRIMARY KEY,
  file_id       INTEGER NOT NULL REFERENCES files (id) ON DELETE CASCADE,
  url           TEXT NOT NULL UNIQUE,  -- file URL, plus #start-end for a segment
  cue_start     REAL,
  cue_end       REAL,
  -- identifies the audio across moves and renames (see scanner::fingerprint)
  fingerprint   TEXT,
  title         TEXT NOT NULL,
  title_sort    TEXT NOT NULL,
  title_search  TEXT NOT NULL,
  -- title, album and artists' search keys, for multi-word search
  search_text   TEXT,
  album_id      INTEGER REFERENCES albums (id) ON DELETE SET NULL,
  artist_id     INTEGER REFERENCES artists (id) ON DELETE SET NULL,  -- primary artist
  work_id       INTEGER REFERENCES works (id) ON DELETE SET NULL,
  track_number  INTEGER,
  disc          INTEGER,
  disc_subtitle TEXT,
  year          INTEGER NOT NULL DEFAULT 0,  -- 0: unknown
  duration      REAL,                        -- seconds
  format        TEXT NOT NULL,               -- flc, mp3, mp4, alc, ogg, ops, wav, aif, …
  bitrate       INTEGER,
  vbr           INTEGER NOT NULL DEFAULT 0,
  sample_rate   INTEGER,
  sample_size   INTEGER,
  channels      INTEGER,
  lossless      INTEGER NOT NULL DEFAULT 0,
  big_endian    INTEGER,
  block_align   INTEGER,
  audio_offset  INTEGER,
  audio_size    INTEGER,
  bpm           INTEGER,
  tag_version   TEXT,
  replay_gain   REAL,
  replay_peak   REAL,
  mbid          TEXT,
  lyrics        TEXT,
  subtitle      TEXT,
  grouping      TEXT,
  performance   TEXT,
  -- embedded picture length, picture file path, or '0' for none
  cover         TEXT,
  cover_id      TEXT,
  added_at      INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) STRICT;
CREATE INDEX tracks_file ON tracks (file_id);
CREATE INDEX tracks_album ON tracks (album_id);
CREATE INDEX tracks_artist ON tracks (artist_id);
CREATE INDEX tracks_work ON tracks (work_id);
CREATE INDEX tracks_title_sort ON tracks (title_sort);
CREATE INDEX tracks_year ON tracks (year);
CREATE INDEX tracks_cover_id ON tracks (cover_id);
CREATE INDEX tracks_fingerprint ON tracks (fingerprint);

-- `role`: 1 artist, 2 composer, 3 conductor, 4 band, 5 album artist, 6 track artist,
-- 7 lyricist, … (config::CONTRIBUTOR_ROLES)
CREATE TABLE track_artists (
  track_id  INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
  artist_id INTEGER NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
  role      INTEGER NOT NULL,
  position  INTEGER NOT NULL,  -- order of appearance in the tags
  PRIMARY KEY (track_id, role, artist_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX track_artists_artist ON track_artists (artist_id, role);

CREATE TABLE album_artists (
  album_id  INTEGER NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
  artist_id INTEGER NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
  role      INTEGER NOT NULL,
  PRIMARY KEY (album_id, role, artist_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX album_artists_artist ON album_artists (artist_id, role);

CREATE TABLE track_genres (
  track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
  genre_id INTEGER NOT NULL REFERENCES genres (id) ON DELETE CASCADE,
  PRIMARY KEY (track_id, genre_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX track_genres_genre ON track_genres (genre_id);

CREATE TABLE track_comments (
  track_id INTEGER NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  text     TEXT NOT NULL,
  PRIMARY KEY (track_id, position)
) STRICT, WITHOUT ROWID;

-- 'saved': in the playlist folder (listed as playlists); 'file': a playlist file elsewhere
-- (reached through folders); 'cue': a cue sheet.
CREATE TABLE playlists (
  id          INTEGER PRIMARY KEY,
  file_id     INTEGER UNIQUE REFERENCES files (id) ON DELETE CASCADE,
  url         TEXT NOT NULL UNIQUE,
  kind        TEXT NOT NULL CHECK (kind IN ('saved', 'file', 'cue')),
  name        TEXT NOT NULL,
  name_sort   TEXT NOT NULL,
  name_search TEXT NOT NULL
) STRICT;
CREATE INDEX playlists_kind ON playlists (kind, name_sort);

CREATE TABLE playlist_items (
  playlist_id INTEGER NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
  position    INTEGER NOT NULL,
  url         TEXT NOT NULL,
  PRIMARY KEY (playlist_id, position)
) STRICT, WITHOUT ROWID;
CREATE INDEX playlist_items_url ON playlist_items (url);

-- What listeners did.  Kept when tracks are rescanned or the library is wiped, and found
-- again for a moved file by MusicBrainz id or fingerprint.
CREATE TABLE track_stats (
  id          INTEGER PRIMARY KEY,
  url         TEXT NOT NULL UNIQUE,
  mbid        TEXT,
  fingerprint TEXT,
  added_at    INTEGER NOT NULL,
  rating      INTEGER,
  play_count  INTEGER NOT NULL DEFAULT 0,
  last_played INTEGER
) STRICT;
CREATE INDEX track_stats_mbid ON track_stats (mbid);
CREATE INDEX track_stats_fingerprint ON track_stats (fingerprint);

-- Sonic analysis (bliss features) and loudness per audio content.  Kept across wipes; a
-- changed or moved file finds its analysis by fingerprint.  NULL features / loudness: could
-- not be measured.  `loudness` in LUFS, `peak` linear true peak.
CREATE TABLE track_analysis (
  fingerprint TEXT PRIMARY KEY,
  version     INTEGER NOT NULL,
  features    BLOB,
  loudness    REAL,
  peak        REAL,
  analyzed_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
"#;

/// Library tables a wipe empties (user data, `track_stats` and `track_analysis`, stays).
pub const CLEAR: &str = r#"
DELETE FROM playlist_items;
DELETE FROM playlists;
DELETE FROM track_comments;
DELETE FROM track_genres;
DELETE FROM album_artists;
DELETE FROM track_artists;
DELETE FROM tracks;
DELETE FROM works;
DELETE FROM albums;
DELETE FROM genres;
DELETE FROM artists;
DELETE FROM files;
DELETE FROM meta WHERE key = 'last_scan';
"#;

pub fn init_connection(c: &mut Connection) -> rusqlite::Result<()> {
    c.busy_timeout(std::time::Duration::from_secs(30))?;
    c.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -20000;",
    )?;
    // journal_mode returns a row
    let _: String = c.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    c.create_collation("perllocale", crate::text::collate_cmp)?;
    Ok(())
}

pub fn open_pool(path: &Path) -> anyhow::Result<DbPool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        let mut c = Connection::open(path)?;
        init_connection(&mut c)?;
        if is_lms_schema(&c)? {
            let backup = backup_path(path);
            tracing::info!(
                "converting the library database to the RMS schema (backup: {})",
                backup.display()
            );
            // VACUUM INTO writes a consistent copy, WAL included
            c.execute("VACUUM INTO ?", [backup.to_string_lossy().as_ref()])?;
            convert_lms(&mut c)?;
        }
        migrate(&mut c)?;
    }
    let manager = SqliteConnectionManager::file(path).with_init(init_connection);
    let pool = Pool::builder().max_size(8).build(manager)?;
    Ok(pool)
}

/// Bring the schema up to [`SCHEMA_VERSION`], one transaction per step.
pub fn migrate(c: &mut Connection) -> rusqlite::Result<()> {
    let mut version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    while (version as usize) < MIGRATIONS.len() {
        let tx = c.transaction()?;
        tx.execute_batch(MIGRATIONS[version as usize])?;
        version += 1;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// A database written with Lyrion's schema (RMS before its own schema).
fn is_lms_schema(c: &Connection) -> rusqlite::Result<bool> {
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(version == 0 && table_exists(c, "metainformation")?)
}

pub fn table_exists(c: &Connection, name: &str) -> rusqlite::Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?",
        [name],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// `<db>.lms-backup`, or `<db>.lms-backup.<n>` when that exists already.
fn backup_path(db: &Path) -> PathBuf {
    let base = format!("{}.lms-backup", db.display());
    let mut p = PathBuf::from(&base);
    let mut n = 1;
    while p.exists() {
        p = PathBuf::from(format!("{}.{}", base, n));
        n += 1;
    }
    p
}

/// Turn a Lyrion-schema database into an empty RMS one that keeps the listening history and
/// the analysis results.  All or nothing.
fn convert_lms(c: &mut Connection) -> rusqlite::Result<()> {
    // dropping tables in any order: no foreign key checks while converting
    c.execute_batch("PRAGMA foreign_keys = OFF")?;
    let res = (|| {
        let tx = c.transaction()?;
        let keep_stats = table_exists(&tx, "tracks_persistent")?;
        if keep_stats {
            tx.execute_batch("ALTER TABLE tracks_persistent RENAME TO legacy_stats")?;
        }
        let keep_analysis = table_exists(&tx, "track_analysis")?
            && tx
                .prepare("SELECT urlmd5, timestamp, filesize, loudness, peak FROM track_analysis LIMIT 0")
                .is_ok();
        if keep_analysis {
            tx.execute_batch("ALTER TABLE track_analysis RENAME TO legacy_analysis")?;
        }
        let old: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                   AND name NOT IN ('legacy_stats', 'legacy_analysis')",
            )?;
            let names = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            names
        };
        for t in old {
            tx.execute_batch(&format!("DROP TABLE \"{}\"", t.replace('"', "\"\"")))?;
        }
        tx.execute_batch(SCHEMA_V1)?;
        if keep_stats {
            // one row per URL; the most played wins when LMS kept duplicates
            tx.execute_batch(
                "INSERT OR IGNORE INTO track_stats (url, mbid, added_at, rating, play_count, last_played)
                 SELECT url, NULLIF(musicbrainz_id, ''), COALESCE(added, CAST(strftime('%s', 'now') AS INTEGER)),
                        NULLIF(rating, 0), COALESCE(playCount, 0), lastPlayed
                 FROM legacy_stats WHERE url IS NOT NULL AND url != ''
                 ORDER BY COALESCE(playCount, 0) DESC, lastPlayed DESC;
                 DROP TABLE legacy_stats;",
            )?;
        }
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()
    })();
    c.execute_batch("PRAGMA foreign_keys = ON")?;
    res
}

pub fn get_meta(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

pub fn set_meta(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?, ?)",
        [key, value],
    )?;
    Ok(())
}
