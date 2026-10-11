//! SQLite schema (LMS schema version 26, SQLite flavour) and connection pool.

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

pub type DbPool = Pool<SqliteConnectionManager>;
pub type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

pub const SCHEMA_VERSION: i64 = 26;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS dbix_migration (name VARCHAR(255) PRIMARY KEY, value VARCHAR(255));
CREATE TABLE IF NOT EXISTS metainformation (
  name  varchar(255) NOT NULL DEFAULT '',
  value varchar(255) NOT NULL DEFAULT '',
  PRIMARY KEY (name)
);
CREATE TABLE IF NOT EXISTS tracks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  url text NOT NULL COLLATE NOCASE,
  title blob,
  titlesort text,
  titlesearch text,
  customsearch text,
  album int(10),
  tracknum int(10),
  content_type varchar(255),
  timestamp int(10),
  filesize int(10),
  audio_size int(10),
  audio_offset int(10),
  year smallint(5),
  secs float,
  cover blob,
  vbr_scale varchar(255),
  bitrate float,
  samplerate int(10),
  samplesize int(10),
  channels tinyint(1),
  block_alignment int(10),
  endian bool,
  bpm smallint(5),
  tagversion varchar(255),
  drm bool,
  disc tinyint(1),
  audio bool,
  remote bool,
  lossless bool,
  lyrics text COLLATE NOCASE,
  musicbrainz_id varchar(40),
  musicmagic_mixable bool,
  replay_gain float,
  replay_peak float,
  extid varchar(64),
  primary_artist int(10),
  urlmd5 char(32) NOT NULL default '0',
  coverid char(8) default NULL,
  cover_cached char(1) default NULL,
  virtual char(1) default NULL,
  added_time int(10) default NULL,
  updated_time int(10) default NULL,
  dlna_profile varchar(32),
  hash char(8),
  work integer,
  subtitle blob,
  grouping blob,
  performance blob,
  discsubtitle blob,
  FOREIGN KEY (`album`) REFERENCES `albums` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS trackTitleIndex ON tracks (title);
CREATE INDEX IF NOT EXISTS trackAlbumIndex ON tracks (album);
CREATE INDEX IF NOT EXISTS ctSortIndex ON tracks (content_type);
CREATE INDEX IF NOT EXISTS trackSortIndex ON tracks (titlesort);
CREATE INDEX IF NOT EXISTS trackSearchIndex ON tracks (titlesearch);
CREATE INDEX IF NOT EXISTS trackCustomSearchIndex ON tracks (customsearch);
CREATE INDEX IF NOT EXISTS trackBitrateIndex ON tracks (bitrate);
CREATE INDEX IF NOT EXISTS trackDiscIndex ON tracks (disc);
CREATE INDEX IF NOT EXISTS trackFilesizeIndex ON tracks (filesize);
CREATE INDEX IF NOT EXISTS trackTimestampIndex ON tracks (timestamp);
CREATE INDEX IF NOT EXISTS trackTracknumIndex ON tracks (tracknum);
CREATE INDEX IF NOT EXISTS trackAudioIndex ON tracks (audio);
CREATE INDEX IF NOT EXISTS trackLyricsIndex ON tracks (lyrics);
CREATE INDEX IF NOT EXISTS trackRemoteIndex ON tracks (remote);
CREATE INDEX IF NOT EXISTS trackLosslessIndex ON tracks (lossless);
CREATE INDEX IF NOT EXISTS urlIndex ON tracks (url);
CREATE INDEX IF NOT EXISTS trackExtId ON tracks (extid);
CREATE INDEX IF NOT EXISTS urlmd5Index ON tracks (urlmd5);
CREATE INDEX IF NOT EXISTS coveridIndex ON tracks (coverid);
CREATE INDEX IF NOT EXISTS tracksWorkIndex ON tracks (work);
CREATE INDEX IF NOT EXISTS trackPrimaryArtistIndex ON tracks (primary_artist);

CREATE TABLE IF NOT EXISTS playlist_track (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  position  int(10),
  playlist  int(10),
  track     text NOT NULL,
  FOREIGN KEY (`playlist`) REFERENCES `tracks` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS positionIndex ON playlist_track (position);
CREATE INDEX IF NOT EXISTS playlistIndex ON playlist_track (playlist);

CREATE TABLE IF NOT EXISTS albums (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  title blob,
  titlesort text,
  titlesearch text,
  customsearch text,
  compilation bool,
  year  smallint(5),
  artwork char(8),
  disc  tinyint(1),
  discc  tinyint(1),
  replay_gain float,
  replay_peak float,
  musicbrainz_id varchar(40),
  musicmagic_mixable bool,
  contributor int(10),
  extid varchar(64),
  release_type varchar(64),
  subtitle blob,
  label blob
);
CREATE INDEX IF NOT EXISTS albumsTitleIndex ON albums (title);
CREATE INDEX IF NOT EXISTS albumsSortIndex ON albums (titlesort);
CREATE INDEX IF NOT EXISTS albumsSearchIndex ON albums (titlesearch);
CREATE INDEX IF NOT EXISTS albumsCustomSearchIndex ON albums (customsearch);
CREATE INDEX IF NOT EXISTS compilationSortIndex ON albums (compilation);
CREATE INDEX IF NOT EXISTS albumsYearIndex ON albums (year);
CREATE INDEX IF NOT EXISTS albumsDiscIndex ON albums (disc);
CREATE INDEX IF NOT EXISTS albumsDiscCountIndex ON albums (discc);
CREATE INDEX IF NOT EXISTS albumsArtworkIndex ON albums (artwork);
CREATE INDEX IF NOT EXISTS albumsReleaseTypeIndex ON albums (release_type);

CREATE TABLE IF NOT EXISTS years ( id smallint(5), PRIMARY KEY (id) );

CREATE TABLE IF NOT EXISTS contributors (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name blob,
  namesort text,
  namesearch text,
  customsearch text,
  musicbrainz_id varchar(40),
  musicmagic_mixable bool,
  extid varchar(512),
  portrait blob default NULL,
  portraitid char(8) default NULL
);
CREATE INDEX IF NOT EXISTS contributorsNameIndex ON contributors (name);
CREATE INDEX IF NOT EXISTS contributorsSortIndex ON contributors (namesort);
CREATE INDEX IF NOT EXISTS contributorsSearchIndex ON contributors (namesearch);
CREATE INDEX IF NOT EXISTS contributorsCustomSearchIndex ON contributors (customsearch);
CREATE INDEX IF NOT EXISTS portraitidIndex ON contributors (portraitid);
CREATE INDEX IF NOT EXISTS contributorsMbidIndex ON contributors (musicbrainz_id);

CREATE TABLE IF NOT EXISTS contributor_track (
  role  int(10),
  contributor  int(10),
  track  int(10),
  PRIMARY KEY (role,contributor,track),
  FOREIGN KEY (`track`) REFERENCES `tracks` (`id`) ON DELETE CASCADE,
  FOREIGN KEY (`contributor`) REFERENCES `contributors` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS contributor_trackContribIndex ON contributor_track (contributor);
CREATE INDEX IF NOT EXISTS contributor_trackTrackIndex ON contributor_track (track);
CREATE INDEX IF NOT EXISTS contributor_trackRoleIndex ON contributor_track (role);

CREATE TABLE IF NOT EXISTS contributor_album (
  role  int(10),
  contributor  int(10),
  album  int(10),
  PRIMARY KEY (role,contributor,album),
  FOREIGN KEY (`album`) REFERENCES `albums` (`id`) ON DELETE CASCADE,
  FOREIGN KEY (`contributor`) REFERENCES `contributors` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS contributor_albumContribIndex ON contributor_album (contributor);
CREATE INDEX IF NOT EXISTS contributor_albumAlbumIndex ON contributor_album (album);
CREATE INDEX IF NOT EXISTS contributor_albumRoleIndex ON contributor_album (role);

CREATE TABLE IF NOT EXISTS genres (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name blob,
  namesort text,
  namesearch text,
  customsearch text,
  musicmagic_mixable bool
);
CREATE INDEX IF NOT EXISTS genreNameIndex ON genres (name);
CREATE INDEX IF NOT EXISTS genreSortIndex ON genres (namesort);
CREATE INDEX IF NOT EXISTS genreSearchIndex ON genres (namesearch);
CREATE INDEX IF NOT EXISTS genreCustomSearchIndex ON genres (customsearch);

CREATE TABLE IF NOT EXISTS genre_track (
  genre  int(10),
  track  int(10),
  PRIMARY KEY (genre,track),
  FOREIGN KEY (`track`) REFERENCES `tracks` (`id`) ON DELETE CASCADE,
  FOREIGN KEY (`genre`) REFERENCES `genres` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS genre_trackGenreIndex ON genre_track (genre);
CREATE INDEX IF NOT EXISTS genre_trackTrackIndex ON genre_track (track);

CREATE TABLE IF NOT EXISTS comments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  track  int(10),
  value text,
  FOREIGN KEY (`track`) REFERENCES `tracks` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS comments_trackIndex ON comments (track);

CREATE TABLE IF NOT EXISTS progress (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  type varchar(255),
  name varchar(255),
  active bool,
  total int(10),
  done int(10),
  start int(10),
  finish int(10),
  info varchar(255)
);

CREATE TABLE IF NOT EXISTS tracks_persistent (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  url text NOT NULL,
  musicbrainz_id varchar(40),
  added int(10),
  rating tinyint(1),
  playCount int(10),
  lastPlayed int(10),
  urlmd5 char(32) NOT NULL default '0'
);
CREATE INDEX IF NOT EXISTS trackMusicBrainzIndex ON tracks_persistent (musicbrainz_id);
CREATE INDEX IF NOT EXISTS trackUrlIndex ON tracks_persistent (url);
CREATE INDEX IF NOT EXISTS trackAddedIndex ON tracks_persistent (added);
CREATE INDEX IF NOT EXISTS trackRatingIndex ON tracks_persistent (rating);
CREATE INDEX IF NOT EXISTS trackPlayCountIndex ON tracks_persistent (playCount);
CREATE INDEX IF NOT EXISTS trackLastPlayedIndex ON tracks_persistent (lastPlayed);
CREATE INDEX IF NOT EXISTS persistentUrlmd5Index ON tracks_persistent (urlmd5);

-- RMS: bliss sonic analysis and loudness, kept across wipes like tracks_persistent.
-- `features` / `loudness` are NULL when they could not be measured; either way they are
-- redone when the file changes.  `loudness` is in LUFS, `peak` the linear true peak.
CREATE TABLE IF NOT EXISTS track_analysis (
  urlmd5 char(32) PRIMARY KEY,
  version int(10),
  timestamp int(10),
  filesize int(10),
  features blob,
  loudness float,
  peak float,
  analyzed int(10)
);

CREATE TABLE IF NOT EXISTS scanned_files (
  url text NOT NULL COLLATE NOCASE,
  timestamp int(10),
  filesize int(10)
);
CREATE INDEX IF NOT EXISTS scannedUrlIndex ON scanned_files (url);

CREATE TABLE IF NOT EXISTS library_track (
  track  int(10),
  library char(8),
  PRIMARY KEY (track,library),
  FOREIGN KEY (`track`) REFERENCES `tracks` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS libraryTrackIndex ON library_track (library);
CREATE TABLE IF NOT EXISTS library_album (
  album int(10),
  library char(8),
  PRIMARY KEY (library,album),
  FOREIGN KEY (`album`) REFERENCES `albums` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS libraryAlbumIndex ON library_album (album);
CREATE TABLE IF NOT EXISTS library_contributor (
  contributor int(10),
  library char(8),
  PRIMARY KEY (library,contributor),
  FOREIGN KEY (`contributor`) REFERENCES `contributors` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS libraryContributorIndex ON library_contributor (contributor);
CREATE TABLE IF NOT EXISTS library_genre (
  genre int(10),
  library char(8),
  PRIMARY KEY (library,genre),
  FOREIGN KEY (`genre`) REFERENCES `genres` (`id`) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS libraryGenreIndex ON library_genre (genre);

CREATE TABLE IF NOT EXISTS works (
  id  integer PRIMARY KEY AUTOINCREMENT,
  composer integer,
  title blob,
  titlesort text,
  titlesearch text,
  FOREIGN KEY (`composer`) REFERENCES `contributors` (`id`) ON DELETE CASCADE
);
"#;

/// Tables wiped by `schema_clear.sql`.
pub const CLEAR: &str = r#"
DELETE FROM playlist_track;
DELETE FROM contributor_track;
DELETE FROM contributor_album;
DELETE FROM genre_track;
DELETE FROM comments;
DELETE FROM library_track;
DELETE FROM library_album;
DELETE FROM library_contributor;
DELETE FROM library_genre;
DELETE FROM tracks;
DELETE FROM albums;
DELETE FROM years;
DELETE FROM contributors;
DELETE FROM genres;
DELETE FROM scanned_files;
DELETE FROM works;
UPDATE metainformation SET value = 0 WHERE name = 'lastRescanTime';
"#;

pub fn init_connection(c: &mut Connection) -> rusqlite::Result<()> {
    c.busy_timeout(std::time::Duration::from_secs(30))?;
    c.execute_batch(
        "PRAGMA synchronous = OFF;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = 20000;",
    )?;
    // journal_mode returns a row
    let _: String = c.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    use rusqlite::functions::FunctionFlags;
    c.create_scalar_function(
        "MD5",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let s: String = ctx.get(0)?;
            Ok(format!("{:x}", md5::compute(s.as_bytes())))
        },
    )?;
    c.create_collation("perllocale", crate::text::collate_cmp)?;
    Ok(())
}

pub fn open_pool(path: &Path) -> anyhow::Result<DbPool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // create schema once with a plain connection
    {
        let mut c = Connection::open(path)?;
        init_connection(&mut c)?;
        create_schema(&c)?;
    }
    let manager = SqliteConnectionManager::file(path).with_init(init_connection);
    let pool = Pool::builder().max_size(8).build(manager)?;
    Ok(pool)
}

pub fn create_schema(c: &Connection) -> rusqlite::Result<()> {
    // detect an incompatible/old LMS DB without metainformation: rebuild
    c.execute_batch(SCHEMA)?;
    // track_analysis from before loudness was measured: start it over
    let has_loudness = c
        .prepare("SELECT loudness FROM track_analysis LIMIT 0")
        .is_ok();
    if !has_loudness {
        c.execute_batch(
            "ALTER TABLE track_analysis ADD COLUMN loudness float;
             ALTER TABLE track_analysis ADD COLUMN peak float;
             DELETE FROM track_analysis;",
        )?;
    }
    c.execute(
        "INSERT OR REPLACE INTO dbix_migration (name, value) VALUES ('version', ?)",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

pub fn get_meta(c: &Connection, name: &str) -> Option<String> {
    c.query_row(
        "SELECT value FROM metainformation WHERE name = ?",
        [name],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

pub fn set_meta(c: &Connection, name: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO metainformation (name, value) VALUES (?, ?)",
        [name, value],
    )?;
    Ok(())
}
