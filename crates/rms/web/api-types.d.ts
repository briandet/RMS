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
format?: string, };

export type Queue = { version: number, length: number, index?: number, offset: number, items: Array<Track>, };

export type Item = { "type": "track", id: number, } | { "type": "album", id: number, } | { "type": "artist", id: number, } | { "type": "genre", id: number, } | { "type": "year", year: number, } | { "type": "playlist", id: number, } | { "type": "folder", id: number, } | { "type": "url", url: string, } | { "type": "favorite", id: string, } | { "type": "spotify", id: string, } | { "type": "listenbrainz", id: string, } | { "type": "radio", id: string, };

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
blocked?: string, tracks: Array<TrackTags>, };

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
 * `tags` or `move`.
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

export type Genre = { id: number, name: string, };

export type Year = { 
/**
 * 0 for tracks without a year.
 */
year: number, };

export type Playlist = { id: number, name: string, };

export type FolderEntryType = "folder" | "track" | "playlist" | "other";

export type FolderEntry = { type: FolderEntryType, 
/**
 * Folder id, playlist id or track id, depending on `type`.
 */
id: number, name: string, track?: Track, };

export type SearchResults = { artists: Page<Artist>, albums: Page<Album>, tracks: Page<Track>, genres: Page<Genre>, };

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
 * Learn what tracks played from Spotify sound like, for mixes.
 */
learn: boolean, 
/**
 * Tracks learned so far.
 */
learned: number, };

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
 * Folder with pictures named after artists; empty for none.
 */
art_folder: string, online_artist_images: boolean, 
/**
 * The user's own Spotify app; empty for the shared one.
 */
spotify_client_id: string, 
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
scrobble_pending: number, scrobble_error: string | null, };

export type ServerSettingsPatch = { library_name?: string, media_dirs?: Array<string>, watch_folders?: boolean, upnp_players?: boolean, chromecast_players?: boolean, airplay_players?: boolean, art_folder?: string, online_artist_images?: boolean, spotify_client_id?: string, spotify_bitrate?: number, spotify_normalisation?: boolean, musicbrainz?: boolean, acoustid_key?: string, edit_files?: boolean, file_name_template?: string, listenbrainz?: boolean, scrobble_spotify?: boolean, };

export type ScanMode = "changes" | "playlists" | "full";

export type ScanRequest = { mode?: ScanMode, };

export type Choice = { value: string, options: Array<ChoiceOption>, };

export type ChoiceOption = { value: string, label: string, };

export type Range = { value: number, min: number, max: number, };

export type PlayerSettings = { 
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
 * kbps; 0 for no limit.
 */
max_bitrate: number, digital_volume_control: boolean, 
/**
 * What plays when the queue runs out ("0": nothing).
 */
dont_stop_the_music?: Choice, 
/**
 * Percent of a mix discovered from outside what was played (0–40; with ListenBrainz on).
 */
adventurous: number, 
/**
 * The ListenBrainz accounts (user names) it scrobbles to.
 */
scrobble_to: Array<string>, 
/**
 * For players with a screen.
 */
display?: DisplaySettings, };

export type DisplaySettings = { 
/**
 * What the screen shows while playing.
 */
playing_mode: Choice, text_size: Choice, text_size_off: Choice, brightness_on: Choice, brightness_off: Choice, brightness_idle: Choice, auto_brightness_min?: Range, auto_brightness_sensitivity?: Range, };

export type PlayerSettingsPatch = { transition_type?: number, transition_duration?: number, replay_gain_mode?: number, max_bitrate?: number, digital_volume_control?: boolean, dont_stop_the_music?: string, adventurous?: number, scrobble_to?: Array<string>, playing_mode?: string, text_size?: string, text_size_off?: string, brightness_on?: string, brightness_off?: string, brightness_idle?: string, auto_brightness_min?: number, auto_brightness_sensitivity?: number, };

export type ToRenderer = { "type": "load", url: string, format: string, 
/**
 * For `pcm`.
 */
pcm?: Pcm, autostart: boolean, } | { "type": "preload", url: string, format: string, pcm?: Pcm, } | { "type": "pause" } | { "type": "resume", at?: number, } | { "type": "stop" } | { "type": "volume", volume: number, muted: boolean, } | { "type": "hold", ms: number, } | { "type": "skip", ms: number, };

export type FromRenderer = { "type": "ready" } | { "type": "started" } | { "type": "progress", elapsed: number, duration?: number, jiffies?: number, } | { "type": "clock", jiffies: number, } | { "type": "fetched" } | { "type": "paused" } | { "type": "resumed" } | { "type": "ended" } | { "type": "error", message: string, };

export type Pcm = { bits: number, rate: number, channels: number, little_endian: boolean, };
