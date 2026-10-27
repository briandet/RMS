// Generated from crates/rms/src/api/model.rs by `UPDATE_TYPES=1 cargo test -p rms \
// typescript`.  Do not edit.  The API is described in docs/api.md.

export type ServerInfo = { name: string, version: string, uuid: string, 
/**
 * Present while a library scan runs.
 */
scan?: ScanProgress, 
/**
 * Unix time the last scan finished.
 */
last_scan?: number, library: LibraryTotals, 
/**
 * The port of the server's HTTPS address, when it has one (the same routes as HTTP).
 */
https_port?: number, 
/**
 * HTTPS uses a certificate from the server's own authority, which devices trust once
 * `/tls/rms-ca.crt` is installed on them.
 */
local_ca: boolean, };

export type ScanProgress = { 
/**
 * Current step, e.g. `directory_new`, `post_processing`; empty while starting.
 */
step: string, done: number, total: number, };

export type LibraryTotals = { albums: number, artists: number, genres: number, tracks: number, 
/**
 * Seconds.
 */
duration: number, };

export type Player = { 
/**
 * MAC address, e.g. `00:04:20:12:34:56`.
 */
id: string, name: string, 
/**
 * Model id (`squeezelite`, `baby`, `fab4`, …).
 */
model: string, model_name: string, ip?: string, firmware: string, connected: boolean, power: boolean, mode: PlayMode, 
/**
 * Playing, but waiting for the buffer or for synced players: the position is not moving.
 */
buffering: boolean, 
/**
 * Seconds into the current track when this state was produced.  While `mode` is
 * `play` and not `buffering`, clients advance it with their own clock.
 */
elapsed: number, duration?: number, can_seek: boolean, 
/**
 * 0–100.
 */
volume: number, muted: boolean, 
/**
 * The player outputs a fixed level (digital volume control off on a digital output).
 */
volume_fixed: boolean, shuffle: Shuffle, repeat: Repeat, queue: QueueInfo, now_playing?: Track, 
/**
 * Ids of the other players in this player's sync group.
 */
synced_with: Array<string>, 
/**
 * Seconds until the sleep timer turns the player off.
 */
sleep?: number, };

export type PlayMode = "play" | "pause" | "stop";

export type Shuffle = "off" | "songs" | "albums";

export type Repeat = "off" | "one" | "all";

export type QueueInfo = { 
/**
 * Changes whenever the queue's contents or order change.
 */
version: number, length: number, 
/**
 * Index of the current track; absent when the queue is empty.
 */
index?: number, 
/**
 * Name of the saved playlist the queue was loaded from.
 */
name?: string, };

export type Spectrum = { kind: SpectrumKind, 
/**
 * What the curve belongs to; a different key is a different curve.
 */
key: string, 
/**
 * Readings per second.
 */
fps: number, 
/**
 * Position in the track (seconds) of the first reading.
 */
start: number, 
/**
 * The readings, base64: two bytes each, four levels 0–15 (bass, low mids, high mids,
 * treble), the first of each pair in the high four bits.
 */
curve: string, };

export type SpectrumKind = "track" | "live" | "none";

export type TableRequest = { 
/**
 * Words and `field:value` filters.
 */
q: string, 
/**
 * A groupable field: one row per album, artist, year, …; absent or `track`: one row per
 * track.
 */
group?: string, 
/**
 * Field names; `count` is a grouped row's number of tracks.
 */
columns: Array<string>, sort: Array<TableSort>, offset: number, 
/**
 * Rows (default 200, at most 1000).
 */
limit?: number, };

export type TableSort = { field: string, desc: boolean, };

export type TablePage = { 
/**
 * Rows in all.
 */
total: number, offset: number, rows: Array<TableRow>, 
/**
 * Tracks the query finds, and their length in all (seconds).
 */
tracks: number, duration: number, };

export type TableRow = { 
/**
 * The track, on rows of tracks.
 */
id?: number, 
/**
 * What a grouped row stands for (an album id, a year, a folder, …).
 */
key: string | number | boolean | null, 
/**
 * Its name (`null`: the tracks without one).
 */
label: string | number | boolean | null, 
/**
 * Query words that find a grouped row's tracks (`album_id:12`), to add to the query.
 */
drill?: string, 
/**
 * The grouping to show a grouped row's tracks in.
 */
next?: string, 
/**
 * One per column: a value, `null`, `{"varies": n}` (a grouped row's tracks differ) or
 * `[min, max]`.
 */
cells: Array<string | number | boolean | null | { varies: number } | [number, number]>, };

export type TableField = { name: string, label: string, 
/**
 * `text`, `int`, `real`, `duration` (seconds), `time` (Unix), `bool`, `size` (bytes),
 * `bitrate` (bit/s), `percent` (0–1).
 */
kind: string, 
/**
 * Where it belongs: `Tags`, `Audio`, `Listening`, `Analysis`, `MusicBrainz`, `Kind (EffNet)`,
 * `Mood (EffNet)`, `File`.
 */
section: string, 
/**
 * What a grouped row shows: `same` (the value its tracks share, else varies), `sum`,
 * `avg`, `max`, `range`.
 */
agg: string, 
/**
 * Rows can be grouped by it; `next` is the grouping a row's tracks are shown in.
 */
next?: string, };

export type TableView = { name: string, q: string, group?: string, columns: Array<string>, sort: Array<TableSort>, 
/**
 * Column widths in pixels, by field name.
 */
widths: Record<string, number>, };

export type Track = { 
/**
 * Library track id; absent for streams and services.
 */
id?: number, url: string, title: string, artist?: string, artist_id?: number, album?: string, album_id?: number, duration?: number, track_number?: number, disc?: number, year?: number, 
/**
 * Artwork path relative to the server root (`music/<id>/cover`,
 * `imageproxy/<url>/image`).  Append `_<w>x<h>_o` for a resized copy.
 */
artwork?: string, 
/**
 * Spotify menu ids of a Spotify track's album and first artist (`album:<id>`,
 * `artist:<id>`), to browse to them.
 */
spotify_album?: string, spotify_artist?: string, 
/**
 * Internet radio, a service track or another remote URL.
 */
remote: boolean, 
/**
 * Station name, for a radio stream that reports the current song as its title.
 */
station?: string, 
/**
 * Content type (`flc`, `mp3`, …).
 */
format?: string, 
/**
 * What the library knows about a library track beyond its tags; only when asked for
 * (`detail=true`).
 */
detail?: TrackDetail, };

export type TrackDetail = { genres: Array<string>, 
/**
 * Beats per minute, from the rhythm analysis, else the `BPM` tag.
 */
bpm?: number, 
/**
 * Musical key as a Camelot code (`8A`), from the rhythm analysis.
 */
key?: string, 
/**
 * 0–100.
 */
rating?: number, play_count: number, 
/**
 * Times it was left within 30 seconds.
 */
skip_count: number, 
/**
 * Unix time.
 */
last_played?: number, 
/**
 * Unix time it was added to the library.
 */
added?: number, 
/**
 * MusicBrainz recording id.
 */
mbid?: string, isrc?: string, 
/**
 * Integrated loudness, LUFS.
 */
loudness?: number, 
/**
 * The sonic analysis is done, so similar songs can go by how it sounds.
 */
analyzed: boolean, 
/**
 * What kind of music it is, by EffNet (when that analysis is on and has reached it).
 */
kind?: TrackKind, };

export type TrackKind = { styles: Array<Label>, moods: Array<Label>, themes: Array<Label>, instruments: Array<Label>, };

export type Label = { name: string, score: number, };

export type TrackAnalysis = { track_id: number, title: string, artist?: string, 
/**
 * Its album, with the album's artist.
 */
album?: Album, file: AnalysisFile, 
/**
 * bliss: what it sounds like, which drives similar songs and mixes; loudness.
 */
bliss: AnalysisStage<BlissAnalysis>, 
/**
 * Tempo, beats, key and energy.
 */
rhythm: AnalysisStage<RhythmAnalysis>, 
/**
 * The audio fingerprint, to recognise the same recording.
 */
chromaprint: AnalysisStage<ChromaprintAnalysis>, 
/**
 * EffNet-Discogs: what kind of music it is.
 */
effnet: AnalysisStage<EffnetAnalysis>, 
/**
 * The MusicBrainz lookup.
 */
musicbrainz: AnalysisStage<MusicBrainzAnalysis>, };

export type AnalysisStatus = "done" | "failed" | "stale" | "pending" | "skipped" | "off";

export type AnalysisStage<T> = { status: AnalysisStatus, 
/**
 * Version of the analysis that made the stored results, and the current one.
 */
version?: number, current_version: number, 
/**
 * Unix time.
 */
analyzed_at?: number, 
/**
 * Present when `done`.
 */
result?: T, };

export type AnalysisFile = { url: string, path?: string, 
/**
 * Identifies the audio across moves and renames; the analyses are stored under it.
 */
fingerprint?: string, format: string, 
/**
 * Bytes.
 */
size?: number, 
/**
 * Bits per second.
 */
bitrate?: number, vbr: boolean, sample_rate?: number, sample_size?: number, channels?: number, lossless: boolean, 
/**
 * Seconds.
 */
duration?: number, 
/**
 * A cue-sheet track: where it starts and ends in the file, seconds.
 */
segment_start?: number, segment_end?: number, 
/**
 * ReplayGain, dB, and peak, from the tags or the measured loudness.
 */
replay_gain?: number, replay_peak?: number, 
/**
 * The `BPM` tag.
 */
bpm_tag?: number, };

export type Feature = { name: string, value: number, };

export type Neighbour = { id: number, title: string, artist?: string, 
/**
 * bliss: the distance (smaller is closer); EffNet: the cosine similarity (larger is
 * closer, at most 1).
 */
score: number, };

export type BlissAnalysis = { 
/**
 * bliss's features, normalised to about -1..1, in its order: tempo, zero-crossing rate,
 * spectral centroid, rolloff and flatness (mean, deviation), loudness (mean, deviation),
 * then chroma.
 */
features: Array<Feature>, 
/**
 * Integrated loudness (EBU R128), LUFS.
 */
loudness?: number, 
/**
 * True peak, linear (1.0 is full scale).
 */
peak?: number, 
/**
 * The library tracks that sound closest.
 */
nearest: Array<Neighbour>, 
/**
 * Tracks closer than this are taken for the same recording.
 */
duplicate_distance: number, };

export type RhythmAnalysis = { 
/**
 * Beats per minute; 0 when there is no steady beat.
 */
bpm: number, 
/**
 * How steady and strong the beat is, 0–1 (below about 0.3: not beat-matched in mixes).
 */
beat_confidence: number, 
/**
 * Beats tracked, and 4/4 bars from the first downbeat.
 */
beats: number, bars: number, 
/**
 * Seconds into the track.
 */
first_downbeat?: number, 
/**
 * Camelot code (`8A`) and name (`A minor`).
 */
key: string, key_name: string, 
/**
 * 0–1: how clearly the key beats the runner-up.
 */
key_confidence: number, 
/**
 * Loudness per bar (per 2 seconds without a beat), dB.
 */
energy: Array<number>, 
/**
 * Seconds per `energy` value.
 */
energy_step?: number, };

export type ChromaprintAnalysis = { 
/**
 * 32-bit items, about 8 a second.
 */
items: number, 
/**
 * Seconds of audio it covers (the first two minutes at most).
 */
duration?: number, 
/**
 * Library tracks that sound like the same recording (by bliss), and whether their
 * fingerprints confirm it.
 */
duplicates: Array<Duplicate>, };

export type Duplicate = { id: number, title: string, artist?: string, same_recording: boolean, };

export type EffnetAnalysis = { labels: TrackKind, 
/**
 * The library tracks closest in kind.
 */
nearest: Array<Neighbour>, };

export type MusicBrainzAnalysis = { 
/**
 * Recording id.
 */
mbid?: string, isrc?: string, artist_mbid?: string, genres: Array<string>, 
/**
 * How it was found: `tag`, `isrc`, `acoustid` or `search`; `none` when it wasn't.
 */
how: string, };

export type AnalysisProgress = { 
/**
 * The background analysis (bliss, rhythm, Chromaprint, EffNet and spectrum bars, in one
 * pass over the audio) is running.
 */
running: boolean, bliss: AnalysisCount, rhythm: AnalysisCount, chromaprint: AnalysisCount, effnet: AnalysisCount, spectrum: AnalysisCount, musicbrainz: AnalysisCount, };

export type AnalysisCount = { 
/**
 * Turned on in the server settings.
 */
enabled: boolean, total: number, done: number, failed: number, 
/**
 * Not analysed yet, or by an older version.
 */
pending: number, };

export type Queue = { version: number, length: number, index?: number, offset: number, items: Array<Track>, };

export type Item = { "type": "track", id: number, } | { "type": "tracks", ids: Array<number>, } | { "type": "album", id: number, } | { "type": "artist", id: number, } | { "type": "genre", id: number, } | { "type": "year", year: number, } | { "type": "playlist", id: number, } | { "type": "folder", id: number, } | { "type": "url", url: string, } | { "type": "favorite", id: string, } | { "type": "spotify", id: string, } | { "type": "listenbrainz", id: string, } | { "type": "radio", id: string, } | { "type": "query", q: string, sort: Array<TableSort>, };

export type EnqueueMode = "play" | "add" | "next";

export type Command = { "command": "play" } | { "command": "pause" } | { "command": "toggle_pause" } | { "command": "stop" } | { "command": "next" } | { "command": "previous" } | { "command": "seek", position: number, } | { "command": "volume", value: number, } | { "command": "mute", muted?: boolean, } | { "command": "power", on: boolean, } | { "command": "shuffle", mode: Shuffle, } | { "command": "repeat", mode: Repeat, } | { "command": "jump", index: number, } | { "command": "move", from: number, to: number, } | { "command": "remove", index: number, } | { "command": "clear" } | { "command": "enqueue", item: Item, mode?: EnqueueMode, 
/**
 * With `mode: play`, the entry of the new queue to start at.
 */
start?: number, } | { "command": "mix", seed: Item, } | { "command": "sync", with: string, } | { "command": "unsync" } | { "command": "rename", name: string, };

export type CommandResult = { 
/**
 * Tracks added by `enqueue` / `mix`.
 */
count?: number, };

export type Event = { "type": "snapshot", server: ServerInfo, players: Array<Player>, } | { "type": "server", server: ServerInfo, } | { "type": "player", player: Player, } | { "type": "player_removed", id: string, };

export type ApiError = { error: string, };

export type Page<T> = { total: number, offset: number, items: Array<T>, };

export type Artist = { id: number, name: string, 
/**
 * Picture path relative to the server root (`contributor/<id>/image`).
 */
portrait?: string, 
/**
 * Index letter of the sort order (in lists).
 */
letter?: string, };

export type Album = { id: number, title: string, 
/**
 * The album artist; Various Artists for a compilation.
 */
artist?: string, artist_id?: number, year?: number, compilation: boolean, 
/**
 * `album`, `ep`, `single`, `compilation`, `live`, … (as tagged).
 */
release_type?: string, 
/**
 * Like `Track.artwork`.
 */
artwork?: string, 
/**
 * Index letter of the sort order (in lists sorted by title or artist).
 */
letter?: string, };

export type AlbumDetail = { 
/**
 * Seconds, all tracks together.
 */
duration: number, tracks: Array<Track>, id: number, title: string, 
/**
 * The album artist; Various Artists for a compilation.
 */
artist?: string, artist_id?: number, year?: number, compilation: boolean, 
/**
 * `album`, `ep`, `single`, `compilation`, `live`, … (as tagged).
 */
release_type?: string, 
/**
 * Like `Track.artwork`.
 */
artwork?: string, 
/**
 * Index letter of the sort order (in lists sorted by title or artist).
 */
letter?: string, };

export type AlbumRelease = { status: ReleaseStatus, 
/**
 * How the release was found: `tag` (the tags name it), `auto` (best fit), `user` (chosen).
 */
how?: string, 
/**
 * How well the album fits the release, 0–1.
 */
score?: number, release?: ReleaseInfo, 
/**
 * The album's tracks, and the release track each one is.
 */
tracks: Array<ReleaseTrackMatch>, 
/**
 * The releases considered, best first.
 */
candidates: Array<ReleaseCandidate>, };

export type ReleaseStatus = "matched" | "uncertain" | "none" | "pending";

export type ReleaseInfo = { mbid: string, title: string, artist: string, 
/**
 * `YYYY`, `YYYY-MM` or `YYYY-MM-DD`; empty when unknown.
 */
date: string, 
/**
 * The first release of this album (the original year).
 */
original_date: string, country: string, 
/**
 * `Official`, `Promotion`, `Bootleg`, …
 */
status: string, label: string, catalog_number: string, barcode: string, 
/**
 * The media: `CD`, `2×CD`, `Digital Media`, …
 */
format: string, 
/**
 * `Album`, `Single`, `EP`, … and secondary types (`Compilation`, `Live`, …).
 */
release_type: Array<string>, track_count: number, disc_count: number, 
/**
 * The release's page on MusicBrainz.
 */
url: string, };

export type ReleaseTrackMatch = { track_id: number, 
/**
 * Where the track is on the release; absent when it is none of its tracks.
 */
disc?: number, position?: number, 
/**
 * The track's number as printed ("A1", "3").
 */
number?: string, title?: string, artist?: string, 
/**
 * Seconds.
 */
length?: number, 
/**
 * MusicBrainz recording id.
 */
recording?: string, };

export type ReleaseCandidate = { mbid: string, score: number, release?: ReleaseInfo, };

export type ReleaseChoice = { 
/**
 * A MusicBrainz release id (from `candidates`, or from musicbrainz.org).
 */
mbid: string, };

export type AlbumTags = { 
/**
 * The release the proposals come from.
 */
release?: ReleaseInfo, 
/**
 * Why the files can't be changed now (see `edit_files_blocked` in the settings).
 */
blocked?: string, 
/**
 * Not an album: the tracks without an album tag, each on its own.
 */
loose: boolean, tracks: Array<TrackTags>, };

export type TrackTags = { track_id: number, 
/**
 * The file.
 */
path: string, 
/**
 * Why this file can't be changed, if it can't.
 */
error?: string, tags: Array<TagValue>, };

export type TagValue = { 
/**
 * `title`, `artist`, `album`, `album_artist`, `track_number`, `track_total`,
 * `disc_number`, `disc_total`, `date`, `original_date`, `label`, `catalog_number`,
 * `barcode`, `musicbrainz_recording_id`, `musicbrainz_track_id`,
 * `musicbrainz_release_id`, `musicbrainz_release_group_id`, `musicbrainz_artist_id`,
 * `musicbrainz_album_artist_id`.
 */
field: string, 
/**
 * In the file (several values joined with "; "); absent when the file has none.
 */
current?: string, 
/**
 * What the release says; absent when it says nothing.
 */
proposed?: string, 
/**
 * `proposed` differs from `current`.
 */
changed: boolean, };

export type TagChanges = { changes: Array<TrackTagChange>, };

export type TrackTagChange = { track_id: number, tags: { [key in string]: string | null }, };

export type TagEditResult = { 
/**
 * The change's entry in `GET /api/v1/tags/edits`, to undo it.
 */
batch?: number, 
/**
 * Files changed.
 */
changed: number, failed: Array<TagEditFailure>, };

export type TagEditFailure = { track_id: number, error: string, };

export type TagEdit = { batch: number, 
/**
 * `tags`, `move` or `cover`.
 */
kind: string, album?: string, 
/**
 * Files changed.
 */
files: number, 
/**
 * Unix time.
 */
applied_at: number, undone_at?: number, };

export type AlbumFiles = { 
/**
 * The template used (the setting's, or the one asked for).
 */
template: string, 
/**
 * Why files can't be moved now, if they can't.
 */
blocked?: string, files: Array<FileMove>, };

export type FileMove = { 
/**
 * Absent for a file that is not a track (cover, booklet) going with the album.
 */
track_id?: number, 
/**
 * Relative to its music folder.
 */
from: string, to: string, 
/**
 * `to` differs from `from`.
 */
changed: boolean, 
/**
 * Why it can't move, if it can't.
 */
error?: string, };

export type MoveRequest = { 
/**
 * Instead of the setting's.
 */
template?: string, };

export type MoveResult = { 
/**
 * The change's entry in `GET /api/v1/tags/edits`, to undo it.
 */
batch?: number, 
/**
 * Tracks moved.
 */
changed: number, failed: Array<TagEditFailure>, 
/**
 * The album after the library caught up (its id can change).
 */
album_id?: number, };

export type CoverArt = { 
/**
 * The MusicBrainz release the images are of; absent while the album isn't matched.
 */
release_mbid?: string, 
/**
 * Front cover first.
 */
images: Array<CoverImage>, 
/**
 * The album's files, and how many have an embedded front cover (it wins over a picture
 * in the folder).
 */
files: number, embedded: number, 
/**
 * The picture file in the album's folder that stands for it, if any.
 */
folder_image?: string, 
/**
 * The album's folder holds other music too: a picture there would be theirs as well, so
 * the cover can only be embedded.
 */
folder_shared: boolean, 
/**
 * The album shows the archive's front cover of its release: it has no art of its own
 * (the files are not changed; saving it in the folder or the files keeps it).
 */
archive: boolean, 
/**
 * Why covers can't be written now, if they can't.
 */
blocked?: string, };

export type CoverImage = { id: string, 
/**
 * `Front`, `Back`, `Booklet`, …
 */
types: Array<string>, front: boolean, 
/**
 * A small version, on coverartarchive.org.
 */
thumbnail: string, };

export type CoverChoice = { 
/**
 * One of `images`.
 */
image: string, 
/**
 * Save it as `cover.jpg` (or `.png`) in the album's folder.
 */
folder: boolean, 
/**
 * Embed it as the front cover of each of the album's files.
 */
embed: boolean, };

export type Genre = { id: number, name: string, };

export type Year = { 
/**
 * 0 for tracks without a year.
 */
year: number, };

export type Playlist = { id: number, name: string, 
/**
 * Saved in the playlist folder, so it can be changed (a playlist file among the music or
 * a cue sheet can't).
 */
editable: boolean, };

export type FolderEntryType = "folder" | "track" | "playlist" | "other";

export type FolderEntry = { type: FolderEntryType, 
/**
 * Folder id, playlist id or track id, depending on `type`.
 */
id: number, name: string, track?: Track, };

export type SearchResults = { artists: Page<Artist>, albums: Page<Album>, tracks: Page<Track>, genres: Page<Genre>, };

export type SimilarRequest = { 
/**
 * Tracks, albums, artists, genres or years to start from.
 */
seeds: Array<Item>, 
/**
 * How many (default 20, at most 200).
 */
limit?: number, 
/**
 * Track ids to leave out (what is already chosen).
 */
exclude: Array<number>, 
/**
 * Include each track's `detail`.
 */
detail: boolean, };

export type BrowseItemType = "folder" | "audio" | "search" | "text";

export type BrowseItem = { id: string, name: string, type: BrowseItemType, 
/**
 * Can be queued (as `{"type": "favorite"|"spotify", "id": …}`), a folder as a whole.
 */
playable: boolean, url?: string, 
/**
 * Like `Track.artwork`.
 */
artwork?: string, };

export type NewFavorite = { url: string, title: string, };

export type SpotifyStatus = { signed_in: boolean, user?: string, 
/**
 * `premium`, `free`, …
 */
product?: string, 
/**
 * Why browsing or playback doesn't work right now.
 */
error?: string, 
/**
 * Why a player isn't available as a Spotify Connect device (tried again later).
 */
connect_error?: string, 
/**
 * Learn what tracks played from Spotify sound like, for mixes.
 */
learn: boolean, 
/**
 * Tracks learned so far.
 */
learned: number, };

export type LearnedTracks = { items: Array<LearnedTrack>, };

export type LearnedTrack = { 
/**
 * What plays it (`spotify://track:<id>`).
 */
url: string, title: string, 
/**
 * The artists' names, joined.
 */
artist: string, album?: string, year?: number, 
/**
 * Seconds.
 */
duration?: number, 
/**
 * Image URL.
 */
artwork?: string, bpm?: number, 
/**
 * Camelot key (`8B`).
 */
key?: string, 
/**
 * Integrated loudness, LUFS.
 */
loudness?: number, 
/**
 * MusicBrainz genres.
 */
genres: Array<string>, 
/**
 * The most likely Discogs style, by its sound.
 */
style?: string, 
/**
 * The library has a copy of it.
 */
in_library: boolean, 
/**
 * When it was learned, Unix seconds.
 */
learned_at: number, };

export type SpotifyQueue = { 
/**
 * The player plays its Spotify Connect device (else `items` is empty).
 */
connect: boolean, items: Array<Track>, 
/**
 * Why Spotify's queue can't be shown.
 */
error?: string, };

export type SpotifyLogin = { code?: string, };

export type SpotifyLoginResult = { 
/**
 * The page to open to sign in.
 */
url?: string, 
/**
 * Signed in as.
 */
user?: string, };

export type SpotifyPatch = { learn?: boolean, };

export type ServerSettings = { library_name: string, media_dirs: Array<string>, 
/**
 * Scan what changes in the music folders as it changes.
 */
watch_folders: boolean, 
/**
 * UPnP / DLNA renderers on the network play as players.
 */
upnp_players: boolean, 
/**
 * Chromecasts on the network play as players.
 */
chromecast_players: boolean, 
/**
 * AirPlay speakers on the network play as players.
 */
airplay_players: boolean, 
/**
 * Players fetch internet radio from the station themselves rather than through the
 * server (which then can't show radio's spectrum bars).
 */
radio_direct: boolean, 
/**
 * Folder with pictures named after artists; empty for none.
 */
art_folder: string, online_artist_images: boolean, 
/**
 * kbps: 96, 160 or 320.
 */
spotify_bitrate: number, spotify_normalisation: boolean, 
/**
 * Identify recordings with MusicBrainz (and AcoustID) to fill in what tags leave out.
 */
musicbrainz: boolean, 
/**
 * AcoustID application key, for recognising recordings by their sound; empty for none.
 */
acoustid_key: string, 
/**
 * Recordings identified on MusicBrainz so far.
 */
musicbrainz_identified: number, 
/**
 * Recordings waiting to be looked up.
 */
musicbrainz_pending: number, 
/**
 * Albums matched to their MusicBrainz release; matched only possibly (worth checking);
 * waiting to be matched.
 */
albums_matched: number, albums_uncertain: number, albums_pending: number, 
/**
 * Tags can be changed in the music files (the web UI's album tag editor).
 */
edit_files: boolean, 
/**
 * Why music files can't be changed now (off, or no login), if they can't.
 */
edit_files_blocked: string | null, 
/**
 * Where an album's files go when organised, relative to their music folder
 * (`{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}`).
 */
file_name_template: string, 
/**
 * Mixes take some songs from outside what was played, found with ListenBrainz.
 */
listenbrainz: boolean, 
/**
 * Songs discovered so far, and of those played and rejected (skipped early).
 */
discovered: number, discovered_played: number, discovered_rejected: number, 
/**
 * ListenBrainz accounts, for scrobbling and recommendations.
 */
listenbrainz_accounts: Array<ListenBrainzAccount>, 
/**
 * Scrobble what plays from Spotify too.
 */
scrobble_spotify: boolean, 
/**
 * Listens waiting to be sent, and the last error sending them.
 */
scrobble_pending: number, scrobble_error: string | null, 
/**
 * Analyse what kind of music each track is with EffNet-Discogs (models downloaded from
 * MTG, non-commercial licence).
 */
effnet: boolean, 
/**
 * `off`, `downloading`, `ready` or `failed` (with `effnet_error`).
 */
effnet_status: string, effnet_error: string | null, 
/**
 * Tracks analysed with EffNet, and still to analyse.
 */
effnet_analyzed: number, effnet_pending: number, 
/**
 * The TV visualiser can run (pmviz and presets are installed).
 */
visualizer_available: boolean, 
/**
 * Why it can't, when it can't.
 */
visualizer_error: string | null, 
/**
 * The video encoder it uses (`h264_vaapi`: the GPU's, `libx264`: the CPU's).
 */
visualizer_encoder: string, 
/**
 * Video size (`1920x1080`).
 */
visualizer_size: Choice, 
/**
 * Size the visuals are drawn at, scaled up to the video size ("": the same).
 */
visualizer_render_size: Choice, visualizer_fps: Choice, 
/**
 * The encoder's constant quality: higher is a smaller stream (each +6 about halves it).
 */
visualizer_quality: Range, 
/**
 * Seconds each preset shows.
 */
visualizer_preset_seconds: Range, 
/**
 * The Cast receiver app Chromecasts play in (8 hex digits), or empty for RMS's own:
 * registered in the Google Cast developer console for
 * `https://<owner>.github.io/<repo>/receiver/`.
 */
visualizer_receiver_app: string, 
/**
 * 0 none, 1 crossfade, 2 fade in, 3 fade out, 4 fade in and out.
 */
transition_type: number, 
/**
 * Seconds.
 */
transition_duration: number, 
/**
 * 0 off, 1 track gain, 2 album gain, 3 smart.
 */
replay_gain_mode: number, 
/**
 * What plays when the queue runs out ("0": nothing).
 */
dont_stop_the_music: Choice, 
/**
 * Percent of a mix discovered from outside what was played (0–40; with ListenBrainz on).
 */
adventurous: number, 
/**
 * The ListenBrainz accounts (user names) players scrobble to.
 */
scrobble_to: Array<string>, };

export type ServerSettingsPatch = { library_name?: string, media_dirs?: Array<string>, watch_folders?: boolean, upnp_players?: boolean, chromecast_players?: boolean, airplay_players?: boolean, radio_direct?: boolean, art_folder?: string, online_artist_images?: boolean, spotify_bitrate?: number, spotify_normalisation?: boolean, musicbrainz?: boolean, acoustid_key?: string, edit_files?: boolean, file_name_template?: string, listenbrainz?: boolean, scrobble_spotify?: boolean, effnet?: boolean, visualizer_size?: string, visualizer_render_size?: string, visualizer_fps?: string, visualizer_quality?: number, visualizer_preset_seconds?: number, visualizer_receiver_app?: string, transition_type?: number, transition_duration?: number, replay_gain_mode?: number, dont_stop_the_music?: string, adventurous?: number, scrobble_to?: Array<string>, };

export type ScanMode = "changes" | "playlists" | "full";

export type ScanRequest = { mode?: ScanMode, };

export type Choice = { value: string, options: Array<ChoiceOption>, };

export type ChoiceOption = { value: string, label: string, };

export type Range = { value: number, min: number, max: number, };

export type PlayerSettings = { 
/**
 * kbps; 0 for no limit.
 */
max_bitrate: number, digital_volume_control: boolean, 
/**
 * For players with a screen.
 */
display?: DisplaySettings, 
/**
 * For Chromecasts: the TV visualiser (`auto`: the video for devices with a screen, `1`
 * the video from the server, `tv` drawn on the TV by RMS's receiver app, `0` off).
 */
visualizer?: Choice, 
/**
 * Milliseconds the player's sound comes out later than it says (`playDelay`: TV
 * speakers, a soundbar), so synced players line up; 0–1000.
 */
play_delay: number, };

export type DisplaySettings = { 
/**
 * What the screen shows while playing.
 */
playing_mode: Choice, text_size: Choice, text_size_off: Choice, brightness_on: Choice, brightness_off: Choice, brightness_idle: Choice, auto_brightness_min?: Range, auto_brightness_sensitivity?: Range, };

export type PlayerSettingsPatch = { play_delay?: number, max_bitrate?: number, digital_volume_control?: boolean, playing_mode?: string, text_size?: string, text_size_off?: string, brightness_on?: string, brightness_off?: string, brightness_idle?: string, auto_brightness_min?: number, auto_brightness_sensitivity?: number, visualizer?: string, };

export type ToRenderer = { "type": "load", url: string, format: string, 
/**
 * For `pcm`.
 */
pcm?: Pcm, autostart: boolean, } | { "type": "preload", url: string, format: string, pcm?: Pcm, } | { "type": "pause" } | { "type": "resume", at?: number, } | { "type": "stop" } | { "type": "volume", volume: number, muted: boolean, } | { "type": "hold", ms: number, } | { "type": "skip", ms: number, };

export type FromRenderer = { "type": "ready" } | { "type": "started" } | { "type": "progress", elapsed: number, duration?: number, jiffies?: number, } | { "type": "clock", jiffies: number, } | { "type": "fetched" } | { "type": "paused" } | { "type": "resumed" } | { "type": "ended" } | { "type": "error", message: string, };

export type Pcm = { bits: number, rate: number, channels: number, little_endian: boolean, };
