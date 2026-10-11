# Subsonic apps

RMS speaks the [Subsonic API](http://www.subsonic.org/pages/api.jsp) (version 1.16.1) with
the [OpenSubsonic](https://opensubsonic.netlify.app/) additions, so the many Subsonic apps
work with it: DSub, Symfonium, Ultrasonic, Tempo (Android), play:Sub, Amperfy, substreamer
(iOS), Feishin, Sonixd, Supersonic (desktop) and others.  They browse the library, search,
play on the phone or computer they run on (RMS streams the files to them), keep playlists,
stars and ratings, and can use a Squeezebox as their *jukebox*.

## Connecting

In the app, add a server with:

* **Address**: `http://<server>:9000` (the web UI's address).
* **User name and password**: when the server is protected (`authorize` on, with
  `username` / `password` set, as for the web UI and the CLI), those.  Otherwise anything:
  the API is as open as the web UI is.

Apps sign in one of three ways, all accepted: the password itself (`p=`, also
`p=enc:<hex>`), a token (`t=` md5 of password and salt, with `s=`), or HTTP Basic.  The
token needs the password as it was set; when the stored password is the hashed form Lyrion
used to write, turn off "token authentication" / "legacy authentication" in the app (or
set the password again with `pref password <password>`).

## What maps to what

| Subsonic | RMS |
|---|---|
| artists, albums (ID3) | the library's album artists and albums; `getArtists` lists album artists, as most Subsonic servers do |
| music folders, directories | the music folders as on disk (`getIndexes`, `getMusicDirectory`) |
| ids | `ar-7` artist, `al-42` album, `tr-1234` song, `fo-50` folder, `pl-3` playlist; like library ids, they change when the library is cleared and scanned again |
| starred | RMS favorites: starring a song, album or artist adds it to the favorites (where the player menus, Squeezer and the web UI show it); unstarring removes it |
| rating (1–5) | the library rating (shown as 0–100 elsewhere) |
| scrobble | counts the play (play count, last played) |
| playlists | saved playlists (`.m3u` files in the playlist folder) |
| `getSimilarSongs` | the same mix as *Create Mix* (sonic analysis, then genre and artist) |
| internet radio | the favorites that are streams |
| lyrics | lyrics from the files' tags (`getLyricsBySongId`) |
| jukebox | a player: see below |
| `startScan` | a rescan for changes |

Not there: podcasts, shares, chat, video, bookmarks and the saved play queue (these answer
with empty lists), user management (one user, allowed everything), and artist/album
information from the internet (empty).

## Streaming

`stream` sends the file as it is when the app can take it: without `format` (or with
`format=raw`) and within the app's `maxBitRate`.  Otherwise ffmpeg transcodes it, to MP3
at the largest standard bit rate within `maxBitRate`, or to Opus with `format=opus`.  A
cue sheet track is cut out of its file.  `timeOffset` starts later into the song
(OpenSubsonic *transcodeOffset*), and `estimateContentLength=true` sends a length for
transcoded streams.  `download` always sends the file as it is.

## Jukebox

`jukeboxControl` plays on a Squeezebox instead of the app's device: by default the first
connected player, or the one set with

```sh
printf 'pref subsonicJukebox 00:04:20:12:34:56\n' | nc <server> 9090
```

`set`, `add`, `clear`, `remove`, `skip`, `start`, `stop` (pause), `shuffle` and `setGain`
(volume) work on that player's queue, which the other clients show as usual.
