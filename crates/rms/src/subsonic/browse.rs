//! Browsing and search: artists, albums, songs, folders, genres, lists, lyrics.

use super::{Error, Params, Reply, Result};
use crate::api::library::{self as lib, FolderParams};
use crate::api::model::FolderEntryType;
use crate::server::Server;
use rms_library::catalog::{AlbumInfo, AlbumOrder, ArtistInfo, Window};
use rms_library::Track;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------- ids

/// `ar-7`, `al-42`, `tr-1234`, `fo-50`, `pl-3`: what kind of thing an id is.  A bare number
/// is taken as the kind the method expects.
pub fn parse_id(id: &str, prefix: &str) -> Option<i64> {
    match id.split_once('-') {
        Some((p, n)) if p == prefix => n.parse().ok(),
        Some(_) => None,
        None => id.parse().ok(),
    }
}

pub fn require_id(p: &Params, key: &str, prefix: &str, what: &str) -> Result<i64> {
    let raw = p.require(key)?;
    parse_id(raw, prefix).ok_or_else(|| Error::not_found(what))
}

fn iso(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------- starred (favorites)

/// What is starred: the favorites that are library tracks, albums or artists.
pub struct Starred {
    tracks: HashSet<String>,
    albums: HashSet<i64>,
    artists: HashSet<i64>,
    pub when: String,
}

impl Starred {
    pub fn load(server: &Server) -> Starred {
        let mut s = Starred {
            tracks: HashSet::new(),
            albums: HashSet::new(),
            artists: HashSet::new(),
            when: iso(chrono::Utc::now().timestamp()),
        };
        let mut albums = Vec::new();
        let mut artists = Vec::new();
        for (_, url) in server.favorites.flat_audio() {
            if url.starts_with("file:") {
                s.tracks.insert(url);
            } else if let Some((class, fields)) =
                crate::control::jive::favorites::parse_db_url(&url)
            {
                let field = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
                match class.as_str() {
                    "album" => {
                        if let Some(t) = field("album.title") {
                            albums.push((t, field("contributor.name")));
                        }
                    }
                    "contributor" => artists.extend(field("contributor.name")),
                    _ => {}
                }
            }
        }
        s.albums = server.library.albums_named(&albums).into_iter().collect();
        s.artists = server.library.artists_named(&artists).into_iter().collect();
        s
    }

    fn track(&self, t: &Track) -> Option<&str> {
        self.tracks.contains(&t.url).then_some(self.when.as_str())
    }
    fn album(&self, id: i64) -> Option<&str> {
        self.albums.contains(&id).then_some(self.when.as_str())
    }
    fn artist(&self, id: i64) -> Option<&str> {
        self.artists.contains(&id).then_some(self.when.as_str())
    }

    pub fn album_ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.albums.iter().copied().collect();
        v.sort();
        v
    }
    pub fn artist_ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.artists.iter().copied().collect();
        v.sort();
        v
    }
    pub fn track_urls(&self) -> Vec<String> {
        let mut v: Vec<String> = self.tracks.iter().cloned().collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------- objects

/// Everything song objects need besides the track.
pub struct Ctx {
    pub starred: Starred,
    dirs: Vec<PathBuf>,
}

impl Ctx {
    pub fn new(server: &Server) -> Ctx {
        Ctx {
            starred: Starred::load(server),
            dirs: server
                .prefs
                .server
                .get_string_list("mediadirs")
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        }
    }

    /// Path relative to its music folder.
    fn path(&self, t: &Track) -> Option<String> {
        let p = t.path()?;
        let rel = self
            .dirs
            .iter()
            .find_map(|d| p.strip_prefix(d).ok())
            .unwrap_or(&p);
        Some(rel.to_string_lossy().trim_start_matches('/').to_string())
    }
}

fn put<T: Into<Value>>(m: &mut Map<String, Value>, k: &str, v: Option<T>) {
    if let Some(v) = v {
        m.insert(k.into(), v.into());
    }
}

pub fn cover_art_of_track(t: &Track) -> Option<String> {
    t.coverid
        .clone()
        .filter(|c| !c.is_empty() && c != "0" && t.cover)
        .or_else(|| {
            t.album_artwork
                .clone()
                .filter(|c| !c.is_empty() && c != "0")
        })
}

/// A song (`Child`).
pub fn song(t: &Track, ctx: &Ctx) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("tr-{}", t.id)));
    put(&mut m, "parent", t.album_id.map(|a| format!("al-{a}")));
    m.insert("isDir".into(), json!(false));
    m.insert("title".into(), json!(t.title));
    put(&mut m, "album", t.album.clone());
    put(&mut m, "artist", t.artist.clone());
    put(&mut m, "track", t.tracknum);
    put(&mut m, "year", t.year.filter(|&y| y > 0));
    put(&mut m, "genre", t.genre.clone());
    put(&mut m, "coverArt", cover_art_of_track(t));
    put(&mut m, "size", t.filesize);
    let path = t.path();
    let suffix = path
        .as_ref()
        .and_then(|p| p.extension())
        .map(|e| e.to_string_lossy().to_lowercase());
    let mime = crate::stream::mime_for_format(&t.content_type)
        .map(str::to_string)
        .or_else(|| {
            path.as_ref()
                .and_then(|p| mime_guess::from_path(p).first())
                .map(|m| m.to_string())
        });
    put(&mut m, "contentType", mime);
    put(&mut m, "suffix", suffix);
    put(&mut m, "duration", t.secs.map(|s| s.round() as i64));
    put(&mut m, "bitRate", t.bitrate.map(|b| b / 1000));
    put(&mut m, "path", ctx.path(t));
    m.insert("isVideo".into(), json!(false));
    m.insert("playCount".into(), json!(t.playcount.unwrap_or(0)));
    put(&mut m, "played", t.lastplayed.map(iso));
    put(&mut m, "created", t.added_time.map(iso));
    put(&mut m, "albumId", t.album_id.map(|a| format!("al-{a}")));
    put(&mut m, "artistId", t.artist_id.map(|a| format!("ar-{a}")));
    m.insert("type".into(), json!("music"));
    m.insert("mediaType".into(), json!("song"));
    put(&mut m, "discNumber", t.disc);
    put(
        &mut m,
        "userRating",
        t.rating.filter(|&r| r > 0).map(|r| (r as u32).div_ceil(20)),
    );
    put(&mut m, "starred", ctx.starred.track(t));
    put(&mut m, "samplingRate", t.samplerate);
    put(&mut m, "bitDepth", t.samplesize);
    put(&mut m, "channelCount", t.channels);
    put(&mut m, "bpm", t.bpm);
    put(&mut m, "comment", t.comment.clone());
    put(&mut m, "musicBrainzId", t.musicbrainz_id.clone());
    put(
        &mut m,
        "sortName",
        sort_name(&t.title, t.titlesort.as_deref()),
    );
    put(&mut m, "displayArtist", t.artist.clone());
    put(&mut m, "displayAlbumArtist", t.albumartist.clone());
    put(&mut m, "displayComposer", t.composer.clone());
    if let Some(g) = &t.genre {
        m.insert("genres".into(), json!([{ "name": g }]));
    }
    if let (Some(id), Some(name)) = (t.artist_id, &t.artist) {
        m.insert(
            "artists".into(),
            json!([{ "id": format!("ar-{id}"), "name": name }]),
        );
    }
    if let (Some(id), Some(name)) = (t.albumartist_id, &t.albumartist) {
        m.insert(
            "albumArtists".into(),
            json!([{ "id": format!("ar-{id}"), "name": name }]),
        );
    }
    let mut gain = Map::new();
    put(&mut gain, "trackGain", t.replay_gain);
    put(&mut gain, "trackPeak", t.replay_peak);
    put(&mut gain, "albumGain", t.album_replay_gain);
    put(&mut gain, "albumPeak", t.album_replay_peak);
    if !gain.is_empty() {
        m.insert("replayGain".into(), Value::Object(gain));
    }
    Value::Object(m)
}

/// An album as an `AlbumID3`.
pub fn album_id3(a: &AlbumInfo, ctx: &Ctx) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("al-{}", a.id)));
    m.insert("name".into(), json!(a.title));
    put(&mut m, "artist", a.artist.clone());
    put(&mut m, "artistId", a.artist_id.map(|i| format!("ar-{i}")));
    put(&mut m, "coverArt", a.cover_id.clone());
    m.insert("songCount".into(), json!(a.song_count));
    m.insert("duration".into(), json!(a.duration.round() as i64));
    m.insert("playCount".into(), json!(a.play_count));
    m.insert("created".into(), json!(iso(a.added_at)));
    put(&mut m, "played", a.last_played.map(iso));
    put(&mut m, "year", a.year);
    put(&mut m, "genre", a.genre.clone());
    put(&mut m, "starred", ctx.starred.album(a.id));
    m.insert("isCompilation".into(), json!(a.compilation));
    if let Some(g) = &a.genre {
        m.insert("genres".into(), json!([{ "name": g }]));
    }
    if let (Some(id), Some(name)) = (a.artist_id, &a.artist) {
        m.insert(
            "artists".into(),
            json!([{ "id": format!("ar-{id}"), "name": name }]),
        );
        m.insert("displayArtist".into(), json!(name));
    }
    Value::Object(m)
}

/// An album as a directory `Child` (the non-ID3 lists).
pub fn album_child(a: &AlbumInfo, ctx: &Ctx) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("al-{}", a.id)));
    put(&mut m, "parent", a.artist_id.map(|i| format!("ar-{i}")));
    m.insert("isDir".into(), json!(true));
    m.insert("title".into(), json!(a.title));
    m.insert("album".into(), json!(a.title));
    put(&mut m, "artist", a.artist.clone());
    put(&mut m, "year", a.year);
    put(&mut m, "genre", a.genre.clone());
    put(&mut m, "coverArt", a.cover_id.clone());
    m.insert("playCount".into(), json!(a.play_count));
    m.insert("songCount".into(), json!(a.song_count));
    m.insert("duration".into(), json!(a.duration.round() as i64));
    m.insert("created".into(), json!(iso(a.added_at)));
    put(&mut m, "starred", ctx.starred.album(a.id));
    put(&mut m, "albumId", Some(format!("al-{}", a.id)));
    put(&mut m, "artistId", a.artist_id.map(|i| format!("ar-{i}")));
    m.insert("mediaType".into(), json!("album"));
    Value::Object(m)
}

pub fn artist_id3(a: &ArtistInfo, ctx: &Ctx) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(format!("ar-{}", a.id)));
    m.insert("name".into(), json!(a.name));
    put(
        &mut m,
        "coverArt",
        a.portrait_id.as_ref().map(|p| format!("pt-{p}")),
    );
    m.insert("albumCount".into(), json!(a.album_count));
    put(&mut m, "starred", ctx.starred.artist(a.id));
    put(&mut m, "sortName", sort_name(&a.name, Some(&a.sort_name)));
    Value::Object(m)
}

/// A sort name worth sending: one that is more than the name in other letter case.
fn sort_name(name: &str, sort: Option<&str>) -> Option<String> {
    sort.filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case(name))
        .map(str::to_string)
}

/// Index letter of a sort name.
fn letter(sort_name: &str) -> String {
    match sort_name.chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect(),
        _ => "#".into(),
    }
}

fn ignored_articles(server: &Server) -> String {
    server.library.config().ignored_articles.clone()
}

fn indexed(items: Vec<(String, Value)>, key: &str) -> Vec<Value> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    for (sort, v) in items {
        let l = letter(&sort);
        match out.last_mut() {
            Some((cur, list)) if *cur == l => list.push(v),
            _ => out.push((l, vec![v])),
        }
    }
    out.into_iter()
        .map(|(name, list)| {
            let mut m = Map::new();
            m.insert("name".into(), json!(name));
            m.insert(key.into(), Value::Array(list));
            Value::Object(m)
        })
        .collect()
}

// ---------------------------------------------------------------- methods

pub fn music_folders(server: &Server) -> Result<Reply> {
    let name = server.prefs.server.get_str("libraryname");
    let name = if name.is_empty() {
        "Music".to_string()
    } else {
        name
    };
    Ok(Reply::one(
        "musicFolders",
        json!({ "musicFolder": [{ "id": 1, "name": name }] }),
    ))
}

fn lib_err(e: lib::LibraryError) -> Error {
    match e {
        lib::LibraryError::NotFound => Error::not_found("Directory"),
        lib::LibraryError::Invalid(m) | lib::LibraryError::Failed(m) => Error::new(0, m),
    }
}

pub async fn indexes(server: &Arc<Server>, _p: &Params) -> Result<Reply> {
    let top = lib::folder(
        server,
        FolderParams {
            offset: 0,
            limit: Some(lib::MAX_LIMIT),
            id: None,
        },
    )
    .await
    .map_err(lib_err)?;
    let ctx = Ctx::new(server);
    let mut dirs = Vec::new();
    let mut track_ids = Vec::new();
    for e in &top.items {
        match e.kind {
            FolderEntryType::Folder => dirs.push((
                e.name.clone(),
                json!({ "id": format!("fo-{}", e.id), "name": e.name }),
            )),
            FolderEntryType::Track => track_ids.push(e.id),
            _ => {}
        }
    }
    dirs.sort_by_key(|(n, _)| n.to_lowercase());
    let songs: Vec<Value> = server
        .library
        .tracks_by_id(&track_ids)
        .iter()
        .map(|t| song(t, &ctx))
        .collect();
    let last = server.library.last_scan_time().unwrap_or(0) * 1000;
    Ok(Reply::one(
        "indexes",
        json!({
            "lastModified": last,
            "ignoredArticles": ignored_articles(server),
            "index": indexed(dirs, "artist"),
            "child": songs,
        }),
    ))
}

pub async fn music_directory(server: &Arc<Server>, p: &Params) -> Result<Reply> {
    let raw = p.require("id")?;
    let ctx = Ctx::new(server);
    if let Some(id) = raw.strip_prefix("al-").and_then(|n| n.parse::<i64>().ok()) {
        let a = server
            .library
            .album_info(id)
            .ok_or_else(|| Error::not_found("Album"))?;
        let songs: Vec<Value> = server
            .library
            .album_tracks(id)
            .iter()
            .map(|t| song(t, &ctx))
            .collect();
        let mut d = json!({ "id": raw, "name": a.title, "child": songs });
        if let Some(ar) = a.artist_id {
            d["parent"] = json!(format!("ar-{ar}"));
        }
        put_starred(&mut d, ctx.starred.album(id));
        return Ok(Reply::one("directory", d));
    }
    if let Some(id) = raw.strip_prefix("ar-").and_then(|n| n.parse::<i64>().ok()) {
        let a = server
            .library
            .artist_info(id)
            .ok_or_else(|| Error::not_found("Artist"))?;
        let albums: Vec<Value> = server
            .library
            .artist_albums(id)
            .iter()
            .map(|al| album_child(al, &ctx))
            .collect();
        let mut d = json!({ "id": raw, "name": a.name, "child": albums });
        put_starred(&mut d, ctx.starred.artist(id));
        return Ok(Reply::one("directory", d));
    }
    let id = parse_id(raw, "fo").ok_or_else(|| Error::not_found("Directory"))?;
    let name = match server.library.folder_item(id) {
        Some(rms_library::queries::FolderItem::Dir { url, .. }) => {
            crate::util::path_from_file_url(&url)
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_default()
        }
        _ => return Err(Error::not_found("Directory")),
    };
    let entries = lib::folder(
        server,
        FolderParams {
            offset: 0,
            limit: Some(lib::MAX_LIMIT),
            id: Some(id),
        },
    )
    .await
    .map_err(lib_err)?;
    let track_ids: Vec<i64> = entries
        .items
        .iter()
        .filter(|e| e.kind == FolderEntryType::Track)
        .map(|e| e.id)
        .collect();
    let tracks = server.library.tracks_by_id(&track_ids);
    let mut children = Vec::new();
    for e in &entries.items {
        match e.kind {
            FolderEntryType::Folder => children.push(json!({
                "id": format!("fo-{}", e.id), "parent": raw, "isDir": true, "title": e.name,
            })),
            FolderEntryType::Track => {
                if let Some(t) = tracks.iter().find(|t| t.id == e.id) {
                    let mut s = song(t, &ctx);
                    s["parent"] = json!(raw);
                    children.push(s);
                }
            }
            _ => {}
        }
    }
    Ok(Reply::one(
        "directory",
        json!({ "id": raw, "name": name, "child": children }),
    ))
}

fn put_starred(v: &mut Value, when: Option<&str>) {
    if let Some(w) = when {
        v["starred"] = json!(w);
    }
}

pub fn artists(server: &Server) -> Result<Reply> {
    let ctx = Ctx::new(server);
    let items = server
        .library
        .album_artists()
        .iter()
        .map(|a| (a.sort_name.clone(), artist_id3(a, &ctx)))
        .collect();
    Ok(Reply::one(
        "artists",
        json!({ "ignoredArticles": ignored_articles(server), "index": indexed(items, "artist") }),
    ))
}

pub fn artist(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "id", "ar", "Artist")?;
    let a = server
        .library
        .artist_info(id)
        .ok_or_else(|| Error::not_found("Artist"))?;
    let ctx = Ctx::new(server);
    let mut v = artist_id3(&a, &ctx);
    let albums: Vec<Value> = server
        .library
        .artist_albums(id)
        .iter()
        .map(|al| album_id3(al, &ctx))
        .collect();
    v["albumCount"] = json!(albums.len());
    v["album"] = Value::Array(albums);
    Ok(Reply::one("artist", v))
}

pub fn album(server: &Server, p: &Params) -> Result<Reply> {
    let id = require_id(p, "id", "al", "Album")?;
    let a = server
        .library
        .album_info(id)
        .ok_or_else(|| Error::not_found("Album"))?;
    let ctx = Ctx::new(server);
    let mut v = album_id3(&a, &ctx);
    v["song"] = Value::Array(
        server
            .library
            .album_tracks(id)
            .iter()
            .map(|t| song(t, &ctx))
            .collect(),
    );
    Ok(Reply::one("album", v))
}

pub fn song_by_id(server: &Server, id: i64) -> Result<Track> {
    server
        .library
        .track(id)
        .ok_or_else(|| Error::not_found("Song"))
}

pub fn song_reply(server: &Server, p: &Params) -> Result<Track> {
    song_by_id(server, require_id(p, "id", "tr", "Song")?)
}

pub fn get_song(server: &Server, p: &Params) -> Result<Reply> {
    let t = song_reply(server, p)?;
    Ok(Reply::one("song", song(&t, &Ctx::new(server))))
}

pub fn genres(server: &Server) -> Result<Reply> {
    let genres: Vec<Value> = server
        .library
        .genre_list()
        .iter()
        .map(|g| json!({ "value": g.name, "songCount": g.song_count, "albumCount": g.album_count }))
        .collect();
    Ok(Reply::one("genres", json!({ "genre": genres })))
}

pub fn album_list(server: &Server, p: &Params, id3: bool) -> Result<Reply> {
    let kind = p.require("type")?;
    let size = p.size("size", 10, 500)?;
    let offset = p.size("offset", 0, usize::MAX)?;
    let ctx = Ctx::new(server);
    let order = match kind {
        "random" => AlbumOrder::Random,
        "newest" => AlbumOrder::Newest,
        "frequent" => AlbumOrder::Frequent,
        "recent" => AlbumOrder::Recent,
        "highest" => AlbumOrder::Highest,
        "alphabeticalByName" => AlbumOrder::Title,
        "alphabeticalByArtist" => AlbumOrder::Artist,
        "starred" => AlbumOrder::Ids(ctx.starred.album_ids()),
        "byYear" => AlbumOrder::Years {
            from: p
                .int("fromYear")?
                .ok_or_else(|| Error::new(10, "Required parameter is missing: fromYear"))?,
            to: p
                .int("toYear")?
                .ok_or_else(|| Error::new(10, "Required parameter is missing: toYear"))?,
        },
        "byGenre" => AlbumOrder::Genre(p.require("genre")?.to_string()),
        other => return Err(Error::new(0, format!("Unknown list type {other}"))),
    };
    let albums = server.library.album_list(&order, Window::new(offset, size));
    let list: Vec<Value> = albums
        .iter()
        .map(|a| {
            if id3 {
                album_id3(a, &ctx)
            } else {
                album_child(a, &ctx)
            }
        })
        .collect();
    let key = if id3 { "albumList2" } else { "albumList" };
    Ok(Reply::one(key, json!({ "album": list })))
}

pub fn random_songs(server: &Server, p: &Params) -> Result<Reply> {
    let size = p.size("size", 10, 500)?;
    let tracks = server.library.random_track_list(
        size,
        p.get("genre").filter(|g| !g.is_empty()),
        p.int("fromYear")?,
        p.int("toYear")?,
    );
    let ctx = Ctx::new(server);
    let songs: Vec<Value> = tracks.iter().map(|t| song(t, &ctx)).collect();
    Ok(Reply::one("randomSongs", json!({ "song": songs })))
}

pub fn songs_by_genre(server: &Server, p: &Params) -> Result<Reply> {
    let genre = p.require("genre")?;
    let w = Window::new(p.size("offset", 0, usize::MAX)?, p.size("count", 10, 500)?);
    let ctx = Ctx::new(server);
    let songs: Vec<Value> = server
        .library
        .genre_tracks(genre, w)
        .iter()
        .map(|t| song(t, &ctx))
        .collect();
    Ok(Reply::one("songsByGenre", json!({ "song": songs })))
}

pub fn search(server: &Server, p: &Params, id3: bool) -> Result<Reply> {
    // clients send `""` (or nothing) to list everything
    let q = p.get("query").unwrap_or("").trim().trim_matches('"').trim();
    let w = |count: &str, offset: &str| -> Result<Window> {
        Ok(Window::new(
            p.size(offset, 0, usize::MAX)?,
            p.size(count, 20, 10_000)?,
        ))
    };
    let found = server.library.search_all(
        q,
        w("artistCount", "artistOffset")?,
        w("albumCount", "albumOffset")?,
        w("songCount", "songOffset")?,
    );
    let ctx = Ctx::new(server);
    let artists: Vec<Value> = found
        .artists
        .iter()
        .map(|a| {
            if id3 {
                artist_id3(a, &ctx)
            } else {
                json!({ "id": format!("ar-{}", a.id), "name": a.name })
            }
        })
        .collect();
    let albums: Vec<Value> = found
        .albums
        .iter()
        .map(|a| {
            if id3 {
                album_id3(a, &ctx)
            } else {
                album_child(a, &ctx)
            }
        })
        .collect();
    let songs: Vec<Value> = found.tracks.iter().map(|t| song(t, &ctx)).collect();
    let key = if id3 {
        "searchResult3"
    } else {
        "searchResult2"
    };
    Ok(Reply::one(
        key,
        json!({ "artist": artists, "album": albums, "song": songs }),
    ))
}

pub fn starred(server: &Server, id3: bool) -> Result<Reply> {
    let ctx = Ctx::new(server);
    let artists: Vec<Value> = ctx
        .starred
        .artist_ids()
        .into_iter()
        .filter_map(|id| server.library.artist_info(id))
        .map(|a| {
            if id3 {
                artist_id3(&a, &ctx)
            } else {
                json!({ "id": format!("ar-{}", a.id), "name": a.name, "starred": ctx.starred.when })
            }
        })
        .collect();
    let albums: Vec<Value> = server
        .library
        .album_list(
            &AlbumOrder::Ids(ctx.starred.album_ids()),
            Window::new(0, usize::MAX),
        )
        .iter()
        .map(|a| {
            if id3 {
                album_id3(a, &ctx)
            } else {
                album_child(a, &ctx)
            }
        })
        .collect();
    let songs: Vec<Value> = ctx
        .starred
        .track_urls()
        .iter()
        .filter_map(|u| server.library.track_by_url(u))
        .map(|t| song(&t, &ctx))
        .collect();
    let key = if id3 { "starred2" } else { "starred" };
    Ok(Reply::one(
        key,
        json!({ "artist": artists, "album": albums, "song": songs }),
    ))
}

pub fn similar_songs(server: &Server, p: &Params, key: &str) -> Result<Reply> {
    use rms_library::mix::Seed;
    let raw = p.require("id")?;
    let seed = if let Some(id) = raw.strip_prefix("ar-").and_then(|n| n.parse().ok()) {
        Seed::Artist(id)
    } else if let Some(id) = raw.strip_prefix("al-").and_then(|n| n.parse().ok()) {
        Seed::Album(id)
    } else {
        Seed::Track(parse_id(raw, "tr").ok_or_else(|| Error::not_found("Song"))?)
    };
    let count = p.size("count", 50, 500)?;
    let seeds = server.library.seed_tracks(seed);
    let exclude: HashSet<i64> = seeds.iter().copied().collect();
    let ids = server.library.similar_tracks(&seeds, count, &exclude);
    let ctx = Ctx::new(server);
    let songs: Vec<Value> = server
        .library
        .tracks_by_id(&ids)
        .iter()
        .map(|t| song(t, &ctx))
        .collect();
    Ok(Reply::one(key, json!({ "song": songs })))
}

pub fn lyrics(server: &Server, p: &Params) -> Result<Reply> {
    let (artist, title) = (p.get("artist").unwrap_or(""), p.get("title").unwrap_or(""));
    let found = server
        .library
        .search_all(
            title,
            Window::new(0, 0),
            Window::new(0, 0),
            Window::new(0, 50),
        )
        .tracks
        .into_iter()
        .find(|t| {
            t.lyrics.is_some()
                && t.title.eq_ignore_ascii_case(title)
                && (artist.is_empty()
                    || t.artist
                        .as_deref()
                        .is_some_and(|a| a.eq_ignore_ascii_case(artist)))
        });
    let mut v = json!({});
    if let Some(t) = found {
        v = json!({ "artist": t.artist, "title": t.title, "value": t.lyrics });
    }
    Ok(Reply::one("lyrics", v))
}

pub fn lyrics_by_song_id(server: &Server, p: &Params) -> Result<Reply> {
    let t = song_reply(server, p)?;
    let list: Vec<Value> = t
        .lyrics
        .iter()
        .map(|text| {
            let lines: Vec<Value> = text.lines().map(|l| json!({ "value": l })).collect();
            json!({ "displayArtist": t.artist, "displayTitle": t.title, "lang": "und",
                    "synced": false, "line": lines })
        })
        .collect();
    Ok(Reply::one(
        "lyricsList",
        json!({ "structuredLyrics": list }),
    ))
}

pub fn now_playing(server: &Server) -> Result<Reply> {
    let ctx = Ctx::new(server);
    let mut entries = Vec::new();
    for (i, player) in server.players().iter().enumerate() {
        let state = crate::api::state::player(server, player);
        if state.mode != crate::api::model::PlayMode::Play {
            continue;
        }
        let Some(t) = state
            .now_playing
            .and_then(|t| t.id)
            .and_then(|id| server.library.track(id))
        else {
            continue;
        };
        let mut s = song(&t, &ctx);
        s["username"] = json!(super::ANY_USER);
        s["minutesAgo"] = json!(0);
        s["playerId"] = json!(i + 1);
        s["playerName"] = json!(state.name);
        entries.push(s);
    }
    Ok(Reply::one("nowPlaying", json!({ "entry": entries })))
}

pub fn radio_stations(server: &Server) -> Result<Reply> {
    let stations: Vec<Value> = server
        .favorites
        .flat_audio()
        .into_iter()
        .filter(|(_, u)| u.starts_with("http://") || u.starts_with("https://"))
        .enumerate()
        .map(|(i, (name, url))| json!({ "id": format!("rd-{i}"), "name": name, "streamUrl": url }))
        .collect();
    Ok(Reply::one(
        "internetRadioStations",
        json!({ "internetRadioStation": stations }),
    ))
}
