# Library database

The music library lives in SQLite (`<data-dir>/library.sqlite`), in a schema RMS owns. It
used to copy Lyrion's. The LMS queries that Squeezebox players, SqueezePlay and Squeezer use
(`albums`, `artists`, `titles`, `musicfolder`, …) are translated onto it in
`crates/library/src/queries.rs` and `songdata.rs`, so those clients see what they always saw.

The schema is in `crates/library/src/db.rs`. `PRAGMA user_version` is its version, and
`db::MIGRATIONS` upgrades older databases step by step, one transaction per step. All tables
are `STRICT`, and foreign keys are enforced.

## Tables

| table | holds |
|---|---|
| `files` | Everything the scanner found on disk: directories, audio files, cue sheets and playlist files, with mtime and size for change detection. `cue` says whether an audio file is split into tracks by its own cue sheet (`embedded`) or by a `.cue` file (`external`). |
| `tracks` | Playable tracks: a whole audio file, or a segment of one (`cue_start`/`cue_end`, URL `file:///…#start-end`). Each belongs to a file and goes with it. |
| `albums`, `artists`, `genres`, `works` | What the tags describe. `albums.artist_id` is the album artist (Various Artists for a compilation). |
| `track_artists`, `album_artists` | Who did what. `role` is 1 artist, 2 composer, 3 conductor, 4 band, 5 album artist, 6 track artist (`config::CONTRIBUTOR_ROLES`). `position` keeps the tag order. |
| `track_genres`, `track_comments` | More tags. |
| `playlists`, `playlist_items` | Saved playlists (`saved`, in the playlist folder), other playlist files (`file`) and cue sheets (`cue`), each with its entries' URLs in order. |
| `track_stats` | Listening history: play count, skips (left within 30 seconds), last played, rating, date added. |
| `track_analysis` | Sonic analysis (bliss features) and loudness per audio content. |
| `track_rhythm` | Tempo, beat grid, downbeat, key (with its Camelot code) and an energy curve per bar, per audio content (`rhythm.rs`), for transitions. |
| `track_musicbrainz` | What MusicBrainz says about the audio (`plugins::musicbrainz` looks it up, `musicbrainz.rs` keeps it): recording id, ISRC, genres, and how it was found. It fills in the `mbid`, `isrc` and genres that tags leave out; tags always win. |
| `track_release`, `mb_releases` | Album matching (`releases.rs`): the MusicBrainz release each track's album was matched to (all of an album's tracks together), which of its tracks the audio is (`medium`, `position`), the score, how (`tag`, `auto`, `user`; `none`: nothing found) and the releases considered; and the releases fetched (as JSON). A confident match fills in the album's `mbid` and `year` and the tracks' `mbid`, `track_number` and `disc` that tags leave out. |
| `tag_edits` | Changes RMS made to music files from the web UI's album Tags page, grouped by `batch` (one apply), to undo them: `kind` `tags` (`tagwrite.rs`: per file, the fields as they were and as written, JSON), `move` (`organize.rs`: a file's URL before and after) or `cover` (the folder picture written and the one set aside, or a file's old embedded front cover, kept in `<data-dir>/cover-backup`). |
| `release_art` | The Cover Art Archive's front cover of a release (`releaseart.rs`), as a file in the cache folder, or none; given to confidently matched albums without art of their own, and given up when they have some. Kept across wipes. |
| `track_cover_release` | For a track on its own (no album tag, or its album not surely matched): the release of its recording whose cover stands for it (`releaseart.rs`: the album its tags name, else its original album), or none. Kept across wipes. |
| `discovered` | Songs mixes proposed from outside what was played (ListenBrainz, `discover.rs`): what plays them, what they are similar to, and how it went (`new`, `played`, `rejected` when skipped within 30 seconds). |
| `web_cache` | Answers of web services (ListenBrainz similarities, recommendations, playlists; where Spotify has a recording), for a while. |
| `scrobble_queue` | Listens waiting to be sent to ListenBrainz (`scrobble.rs`), by account. |
| `track_effnet` | EffNet-Discogs analysis per audio content (`effnet.rs`, when turned on): the 1280-number embedding averaged over 32 excerpts (f16, little-endian) and the labels (styles, moods, themes, instruments with scores, JSON). |
| `track_spectrum` | Spectrum bars per audio content (`spectrum.rs`): 20 readings a second of four bands' levels, 0–15 each, two to a byte; each band scaled to its own range in the track. |
| `track_chromaprint` | A Chromaprint audio fingerprint of the first two minutes, per audio content (`print.rs`): the same recording in the library and in a stream. |
| `remote_tracks` | Tracks of streaming services that played to their end (Spotify): metadata only, with their ISRC and, when the library has the same recording, `local_match`. Their analyses are in the tables above under `<source>:<id>`. |
| `meta` | Key/value settings of the library (`last_scan`). |
| `tracks_fts`, `albums_fts`, `artists_fts` | Full-text (FTS5) indexes over the search keys (`search_text`, `name_search`), kept in step by triggers. |

Ids are only stable within one library: a wipe-and-rescan assigns new ones. Players and
controllers keep URLs, and RMS resolves a saved queue entry by URL when its stored track id
no longer matches.

A track's `isrc` comes from its `ISRC` (`TSRC` in ID3) tag; files scanned before version 3 of
the schema get it when they are next rescanned.

## Search

Every search (players, SqueezePlay/Squeezer, the web UI, Subsonic apps) finds the items
where each word of the query starts a word of the title, the album or the artists, in any
order; case, accents and ignored articles don't matter.  The search keys are normalized
when the scanner stores them, and the FTS5 indexes make finding them a lookup (about 1 ms
for 100,000 tracks, where scanning the column took 25 ms).  Subsonic search lists the best
matches (BM25) first.  Lyrion's *substring search* (`searchSubString`), which also matches
inside words, still scans the columns.

## What survives what

`track_stats`, the analyses (`track_analysis`, `track_rhythm`, `track_chromaprint`,
`track_effnet`, `track_spectrum`) and
`remote_tracks` are user data. A wipe and rescan keep them, and so does a file that changes,
moves or is renamed:

* **Listening history** is found by URL first. When a file is new at its URL, the scanner
  then looks for history that has lost its track, first by MusicBrainz recording id and then
  by fingerprint.
* **Analysis** is keyed by fingerprint, so a moved file keeps its analysis and a re-encoded
  one is analysed again.

The **fingerprint** (`scanner::fingerprint`) is an MD5 over the audio data's size and 8 KiB
from the middle and from the end of the audio, plus the cue segment. It is cheap to compute
during a scan and rarely touched by tag edits. Tags written in front of the audio change the
file's size for some formats, and then the fingerprint changes too.

## Converting a database from earlier releases

Releases before this schema stored the library in Lyrion's schema (`metainformation`,
`contributors`, `tracks_persistent`, …). Opening such a database converts it:

1. The database is copied to `library.sqlite.lms-backup` before anything is changed.
   This is an SQLite copy (`VACUUM INTO`), not a file copy, so it includes writes that are
   still in `library.sqlite-wal`. An existing backup is never overwritten; a later one gets a
   numbered name.
2. In one transaction: play counts, ratings and "added" dates move to `track_stats`, the
   analysis results are set aside as `legacy_analysis`, the old tables are dropped and the
   new schema is created. If any step fails, nothing is committed and RMS stops with the
   error, leaving the database exactly as it was.
3. The library itself is empty afterwards, so RMS starts a full scan. As each track is
   stored, it takes its old analysis if the file has not changed since (same URL, mtime and
   size), so nothing has to be analysed again. When the scan has finished, `legacy_analysis`
   is dropped.

To go back to an earlier release: stop RMS, delete `library.sqlite-wal` and
`library.sqlite-shm`, and put the backup in place of `library.sqlite`.  The backup is one
self-contained file.  The `-wal` and `-shm` files belong to the converted database: if they
are left next to the backup, SQLite applies them to it on the next open and corrupts it.

## Not carried over

* Virtual libraries (`library_id`): RMS never built them. The parameter is ignored.
* MusicIP, DLNA profile and "external id" columns: RMS never set them.
* `sort:sql=…` and `search:sql=…`, which passed raw SQL in Lyrion's table names through to
  the database.
