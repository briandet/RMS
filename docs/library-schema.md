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
| `track_stats` | Listening history: play count, last played, rating, date added. |
| `track_analysis` | Sonic analysis (bliss features) and loudness per audio content. |
| `meta` | Key/value settings of the library (`last_scan`). |

Ids are only stable within one library: a wipe-and-rescan assigns new ones. Players and
controllers keep URLs, and RMS resolves a saved queue entry by URL when its stored track id
no longer matches.

## What survives what

`track_stats` and `track_analysis` are user data. A wipe and rescan keep them, and so does a
file that changes, moves or is renamed:

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

1. The file is copied to `library.sqlite.lms-backup` (`VACUUM INTO`, which includes the WAL)
   before anything is changed. An existing backup is never overwritten; a later one gets a
   numbered name.
2. In one transaction: play counts, ratings and "added" dates move to `track_stats`, the
   analysis results are set aside as `legacy_analysis`, the old tables are dropped and the
   new schema is created. If any step fails, nothing is committed and RMS stops with the
   error, leaving the database exactly as it was.
3. The library itself is empty afterwards, so RMS starts a full scan. As each track is
   stored, it takes its old analysis if the file has not changed since (same URL, mtime and
   size), so nothing has to be analysed again. When the scan has finished, `legacy_analysis`
   is dropped.

To go back to an earlier release, stop RMS and put the backup in place of
`library.sqlite`.

## Not carried over

* Virtual libraries (`library_id`): RMS never built them. The parameter is ignored.
* MusicIP, DLNA profile and "external id" columns: RMS never set them.
* `sort:sql=…` and `search:sql=…`, which passed raw SQL in Lyrion's table names through to
  the database.
