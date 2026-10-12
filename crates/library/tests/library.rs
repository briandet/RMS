mod common;

use rms_library::{LibQuery, LibQueryName, Library, PlaylistEditOp, Row, ScanMode, TrackFilter};
use serde_json::{json, Value};

fn q(name: LibQueryName, start: usize, qty: usize, params: &[(&str, &str)]) -> LibQuery {
    let mut q = LibQuery::new(name, start, qty);
    for (k, v) in params {
        q = q.param(k, v);
    }
    q
}

fn run(
    lib: &Library,
    name: LibQueryName,
    start: usize,
    qty: usize,
    params: &[(&str, &str)],
) -> Row {
    lib.query(&q(name, start, qty, params)).unwrap()
}

fn lp<'a>(r: &'a Row, name: &str) -> Vec<&'a serde_json::Map<String, Value>> {
    r.get(name)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|x| x.as_object().unwrap()).collect())
        .unwrap_or_default()
}

fn keys(m: &serde_json::Map<String, Value>) -> Vec<&str> {
    m.keys().map(|k| k.as_str()).collect()
}

macro_rules! need_ffmpeg {
    () => {
        if !common::ffmpeg_available() {
            eprintln!("ffmpeg not available - skipping");
            return;
        }
    };
}

#[test]
fn scan_totals_and_albums() {
    need_ffmpeg!();
    let f = common::build();
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!(s.errors, 0);
    assert!(!s.aborted);
    let t = f.lib.totals();
    // Album One, Album Two, Hits, Cue Album, Live Album
    assert_eq!(t.albums, 5);
    // 2 + 1 + 3 + 2 cue + 2 embedded cue
    assert_eq!(t.songs, 10);
    assert_eq!(t.playlists, 1);
    assert!(t.duration > 15.0);
    assert!(f.lib.progress().last_scan.is_some());
    assert!(!f.lib.is_scanning());

    // albums with the usual Material/Squeezer tags; exact LMS emission order
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        100,
        &[("tags", "lyjaS"), ("sort", "album")],
    );
    assert_eq!(r["count"], json!(5));
    assert_eq!(keys(&r), vec!["albums_loop", "count"]);
    let albums = lp(&r, "albums_loop");
    let a1 = albums.iter().find(|a| a["album"] == "Album One").unwrap();
    assert_eq!(
        keys(a1),
        vec![
            "id",
            "performance",
            "favorites_url",
            "favorites_title",
            "album",
            "year",
            "artwork_track_id",
            "artist_id",
            "artist"
        ]
    );
    assert_eq!(
        a1["favorites_url"],
        "db:album.title=Album%20One&contributor.name=Artist%20A"
    );
    assert_eq!(a1["year"], json!(2001));
    assert_eq!(a1["artist"], "Artist A");
    assert_eq!(a1["artwork_track_id"].as_str().unwrap().len(), 8);
    // sorted by titlesort: ALBUM ONE, ALBUM TWO, CUE ALBUM, HITS, LIVE ALBUM
    let titles: Vec<&str> = albums
        .iter()
        .map(|a| a["album"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["Album One", "Album Two", "Cue Album", "Hits", "Live Album"]
    );

    // unknown virtual library ids are ignored (no virtual libraries)
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        0,
        &[("library_id", "abcd1234")],
    );
    assert_eq!(r["count"], json!(5));

    // count only
    let r = run(&f.lib, LibQueryName::Albums, 0, 0, &[]);
    assert_eq!(keys(&r), vec!["count"]);
    assert_eq!(r["count"], json!(5));

    // compilation detection
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("tags", "law"), ("compilation", "1")],
    );
    let c = lp(&r, "albums_loop");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0]["album"], "Hits");
    assert_eq!(c[0]["artist"], "Various Artists");
    assert_eq!(c[0]["compilation"], json!(1));

    // filter by artist + textkey with artistalbum sort
    let artists = run(&f.lib, LibQueryName::Artists, 0, 100, &[("tags", "s")]);
    let aa = lp(&artists, "artists_loop");
    let artist_a = aa.iter().find(|a| a["artist"] == "Artist A").unwrap();
    assert_eq!(
        keys(artist_a),
        vec!["id", "artist", "textkey", "favorites_url"]
    );
    let id = artist_a["id"].as_i64().unwrap().to_string();
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("artist_id", &id), ("tags", "ls"), ("sort", "artistalbum")],
    );
    let al = lp(&r, "albums_loop");
    assert_eq!(al.len(), 1);
    assert_eq!(al[0]["album"], "Album One");
    assert_eq!(al[0]["textkey"], "A");

    // index list
    let r = run(&f.lib, LibQueryName::Albums, 0, 100, &[("tags", "lZ")]);
    assert_eq!(keys(&r)[0], "indexList");
    assert_eq!(r["indexList"][0], json!(["A", 2]));

    // year filter + yearalbum sort
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("year", "1999"), ("tags", "ly")],
    );
    assert_eq!(lp(&r, "albums_loop")[0]["album"], "Album Two");

    // bad sort
    assert!(f
        .lib
        .query(&q(LibQueryName::Albums, 0, 10, &[("sort", "bogus")]))
        .is_err());
}

#[test]
fn artists_genres_years_titles() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();

    let r = run(&f.lib, LibQueryName::Artists, 0, 100, &[]);
    let names: Vec<&str> = lp(&r, "artists_loop")
        .iter()
        .map(|a| a["artist"].as_str().unwrap())
        .collect();
    // "The Artist B" sorts as "ARTIST B"; hidden directory content is not scanned
    assert_eq!(names[0], "Artist A");
    assert_eq!(names[1], "The Artist B");
    assert!(names.contains(&"Cue Band"));
    assert!(!names.contains(&"Secret"));
    assert_eq!(r["count"].as_u64().unwrap() as usize, names.len());
    // paging
    let r2 = run(&f.lib, LibQueryName::Artists, 1, 2, &[]);
    assert_eq!(lp(&r2, "artists_loop").len(), 2);
    assert_eq!(lp(&r2, "artists_loop")[0]["artist"], "The Artist B");
    assert_eq!(r2["count"], r["count"]);

    // album artists (Material/Squeezer default): no composers, no compilation-only artists
    let r = run(
        &f.lib,
        LibQueryName::Artists,
        0,
        100,
        &[("role_id", "ALBUMARTIST")],
    );
    let names: Vec<&str> = lp(&r, "artists_loop")
        .iter()
        .map(|a| a["artist"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["Artist A", "The Artist B", "Cue Band", "Live Artist"]
    );
    let r = run(
        &f.lib,
        LibQueryName::Artists,
        0,
        100,
        &[("role_id", "COMPOSER")],
    );
    assert_eq!(lp(&r, "artists_loop")[0]["artist"], "Some Composer");
    assert_eq!(r["count"], json!(1));
    // unified artist list adds a Various Artists item on the first page
    let mut cfg = f.lib.config();
    cfg.use_unified_artists_list = true;
    f.lib.set_config(cfg);
    let r = run(&f.lib, LibQueryName::Artists, 0, 100, &[("tags", "s")]);
    let l = lp(&r, "artists_loop");
    assert_eq!(l[0]["artist"], "Various Artists");
    assert_eq!(l[0]["textkey"], " ");
    assert_eq!(r["count"].as_u64().unwrap() as usize, l.len());
    assert!(!l.iter().any(|a| a["artist"] == "Xavier"));
    let mut cfg = f.lib.config();
    cfg.use_unified_artists_list = false;
    f.lib.set_config(cfg);

    // genres: "Jazz; Blues" is split on the splitList
    let r = run(&f.lib, LibQueryName::Genres, 0, 100, &[("tags", "s")]);
    let g: Vec<&str> = lp(&r, "genres_loop")
        .iter()
        .map(|a| a["genre"].as_str().unwrap())
        .collect();
    assert!(g.contains(&"Jazz") && g.contains(&"Blues") && g.contains(&"Rock"));
    let jazz = lp(&r, "genres_loop")
        .into_iter()
        .find(|x| x["genre"] == "Jazz")
        .unwrap();
    assert_eq!(keys(jazz), vec!["id", "genre", "favorites_url", "textkey"]);

    let r = run(&f.lib, LibQueryName::Years, 0, 100, &[]);
    let y: Vec<i64> = lp(&r, "years_loop")
        .iter()
        .map(|a| a["year"].as_i64().unwrap())
        .collect();
    assert_eq!(y, vec![2001, 1999, 1985]);
    assert_eq!(keys(lp(&r, "years_loop")[0]), vec!["year", "favorites_url"]);

    // titles default tags gald, in tag order after id/title
    // multi-word search: every word must match title, album or contributor (title matches first)
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        100,
        &[("search", "song one")],
    );
    let t = lp(&r, "titles_loop");
    assert_eq!(t.len(), 2);
    assert_eq!(t[1]["title"], "Song Two"); // on "Album One"
    assert_eq!(
        keys(t[0]),
        vec!["id", "title", "genre", "artist", "album", "duration"]
    );
    assert_eq!(t[0]["title"], "Song One");
    assert_eq!(t[0]["genre"], "Rock");

    // tracknum sort within an album, with album_header
    let albums = run(&f.lib, LibQueryName::Albums, 0, 100, &[]);
    let a1 = lp(&albums, "albums_loop")
        .into_iter()
        .find(|a| a["album"] == "Album One")
        .unwrap()["id"]
        .as_i64()
        .unwrap()
        .to_string();
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        100,
        &[("album_id", &a1), ("sort", "tracknum"), ("tags", "tAcuo")],
    );
    let t = lp(&r, "titles_loop");
    assert_eq!(t.len(), 2);
    assert_eq!(t[0]["tracknum"], json!(1));
    assert_eq!(t[1]["tracknum"], json!(2));
    assert_eq!(t[0]["type"], "flc");
    assert!(t[0]["url"].as_str().unwrap().starts_with("file:///"));
    assert_eq!(r["album_header"]["title_names"], json!(["Artist A"]));

    // the MP3: track "1/10", composer, The-artist sort key
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        10,
        &[("search", "mp3"), ("tags", "aAtyrjJK")],
    );
    let t = lp(&r, "titles_loop")[0];
    assert_eq!(t["tracknum"], json!(1));
    assert_eq!(t["composer"], "Some Composer");
    assert_eq!(t["year"], json!(1999));
    assert!(t["bitrate"].as_str().unwrap().ends_with("kbps CBR"));
    assert_eq!(t["coverart"], json!(1));

    // count only and paging validity
    let r = run(&f.lib, LibQueryName::Titles, 0, 0, &[]);
    assert_eq!(keys(&r), vec!["count"]);
    assert_eq!(r["count"], json!(10));
    let r = run(&f.lib, LibQueryName::Titles, 50, 10, &[]);
    assert!(r.get("titles_loop").is_none());
}

#[test]
fn cue_sheets() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        100,
        &[("search", "cue"), ("tags", "uadtlAg"), ("sort", "tracknum")],
    );
    let t = lp(&r, "titles_loop");
    assert_eq!(t.len(), 2, "{:?}", r);
    assert_eq!(t[0]["title"], "Cue One");
    assert!(t[0]["url"].as_str().unwrap().ends_with("album.flac#0-3"));
    assert_eq!(t[0]["duration"], json!(3));
    assert_eq!(t[1]["title"], "Cue Two");
    assert!(t[1]["url"].as_str().unwrap().ends_with("album.flac#3-6"));
    assert_eq!(t[0]["album"], "Cue Album");
    assert_eq!(t[0]["albumartist"], "Cue Band");
    assert_eq!(t[0]["genre"], "Classical");
    let tr = f.lib.track_by_url(t[1]["url"].as_str().unwrap()).unwrap();
    assert!(tr.virtual_cue);
    assert_eq!(tr.cue_start, Some(3.0));
    assert_eq!(tr.cue_end, Some(6.0));
    assert_eq!(tr.year, Some(1985));
    // the referenced file is split into tracks: a file, not a track itself
    let base = rms_library::url::strip_anchor(&tr.url).to_string();
    assert!(f.lib.track_by_url(&base).is_none());
    assert!(matches!(
        f.lib.ensure_item(&base),
        Some(rms_library::queries::FolderItem::File { .. })
    ));

    // embedded CUESHEET → fec container + 2 virtual tracks
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        100,
        &[("search", "live"), ("tags", "ud"), ("sort", "tracknum")],
    );
    let t = lp(&r, "titles_loop");
    assert_eq!(t.len(), 2, "{:?}", r);
    assert_eq!(t[0]["title"], "Live One");
    assert!(t[0]["url"].as_str().unwrap().ends_with("live.flac#0-2"));
    assert!(t[1]["url"].as_str().unwrap().ends_with("live.flac#2-4"));

    // rescan with an edited cue sheet replaces the virtual tracks
    let cue = f.media.join("Cue/album.cue");
    let txt = std::fs::read_to_string(&cue)
        .unwrap()
        .replace("Cue Two", "Cue Deux");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&cue, txt).unwrap();
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!(s.changed, 1);
    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        100,
        &[("search", "cue"), ("sort", "tracknum")],
    );
    let names: Vec<&str> = lp(&r, "titles_loop")
        .iter()
        .map(|x| x["title"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Cue One", "Cue Deux"]);
    assert_eq!(f.lib.totals().songs, 10);
}

#[test]
fn incremental_rescan() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let before = f.lib.totals();

    // nothing changed
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!((s.new, s.changed, s.deleted), (0, 0, 0));

    // change a file (retag), delete one, add one
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let p = f.media.join("Artist A/Album One/02 - Song Two.flac");
    common::make_audio(
        &p,
        2.5,
        &[
            ("title", "Song Two (Remix)"),
            ("artist", "Artist A"),
            ("album", "Album One"),
            ("TRACKNUMBER", "2"),
            ("genre", "Electronic"),
        ],
        None,
    );
    std::fs::remove_file(f.media.join("Comp/Hits/03.m4a")).unwrap();
    common::make_audio(
        &f.media.join("New/new.ogg"),
        1.0,
        &[
            ("title", "Brand New"),
            ("artist", "Newcomer"),
            ("album", "Fresh"),
        ],
        None,
    );

    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!(s.changed, 1, "{:?}", s);
    assert_eq!(s.deleted, 1, "{:?}", s);
    assert_eq!(s.new, 1, "{:?}", s);
    let after = f.lib.totals();
    assert_eq!(after.songs, before.songs);
    assert_eq!(after.albums, before.albums + 1);

    let r = run(
        &f.lib,
        LibQueryName::Titles,
        0,
        10,
        &[("search", "remix"), ("tags", "gl")],
    );
    let t = lp(&r, "titles_loop");
    assert_eq!(t.len(), 1);
    assert_eq!(t[0]["genre"], "Electronic");
    assert_eq!(t[0]["album"], "Album One");
    // old genre "Rock" still used by Song One; "Zed" removed with the deleted file
    let r = run(&f.lib, LibQueryName::Artists, 0, 100, &[]);
    let names: Vec<&str> = lp(&r, "artists_loop")
        .iter()
        .map(|a| a["artist"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"Zed"));
    assert!(names.contains(&"Newcomer"));
    // Hits still a compilation with two artists
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("compilation", "1"), ("tags", "l")],
    );
    assert_eq!(lp(&r, "albums_loop").len(), 1);

    // wipe & rescan gives the same totals
    let s = f.lib.scan(ScanMode::Wipe).unwrap();
    assert_eq!(s.new, 9); // files, not tracks
    assert_eq!(f.lib.totals().songs, after.songs);
}

#[test]
fn search_and_songinfo() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();

    let r = run(&f.lib, LibQueryName::Search, 0, 10, &[("term", "song two")]);
    // "Song Two" (title) and "The Mp3 Song" (on "Album Two"); title matches first
    assert_eq!(r["tracks_count"], json!(2));
    assert_eq!(lp(&r, "tracks_loop")[0]["track"], "Song Two");
    assert_eq!(keys(lp(&r, "tracks_loop")[0]), vec!["track_id", "track"]);
    assert!(r.get("albums_count").is_none());
    assert_eq!(r["count"], json!(2));

    // multi-word AND across title and artist
    let r = run(
        &f.lib,
        LibQueryName::Search,
        0,
        10,
        &[("term", "artist song"), ("extended", "1")],
    );
    let tracks: Vec<&str> = lp(&r, "tracks_loop")
        .iter()
        .map(|x| x["track"].as_str().unwrap())
        .collect();
    assert_eq!(tracks.len(), 3, "{:?}", r);
    assert!(lp(&r, "tracks_loop")[0].contains_key("coverid"));
    let r = run(&f.lib, LibQueryName::Search, 0, 10, &[("term", "artist")]);
    // Artist A, The Artist B, Live Artist, Various Artists (word-start prefix match)
    assert_eq!(r["contributors_count"], json!(4));
    let c = lp(&r, "contributors_loop");
    assert_eq!(keys(c[0]), vec!["contributor_id", "contributor"]);
    let r = run(&f.lib, LibQueryName::Search, 0, 10, &[("term", "jazz")]);
    assert_eq!(lp(&r, "genres_loop")[0]["genre"], "Jazz");
    let r = run(
        &f.lib,
        LibQueryName::Search,
        0,
        10,
        &[("term", "album one")],
    );
    assert_eq!(lp(&r, "albums_loop")[0]["album"], "Album One");
    // accent-insensitive: "sóng" matches "Song"
    let r = run(&f.lib, LibQueryName::Search, 0, 10, &[("term", "sóng óne")]);
    assert_eq!(lp(&r, "tracks_loop")[0]["track"], "Song One");

    // songinfo: one key per loop element
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            search: Some("song one".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t[0].title, "Song One");
    let id = t[0].id.to_string();
    let r = run(
        &f.lib,
        LibQueryName::SongInfo,
        0,
        100,
        &[("track_id", &id), ("tags", "aldgtyck")],
    );
    let l = lp(&r, "songinfo_loop");
    let ks: Vec<&str> = l
        .iter()
        .map(|m| m.keys().next().unwrap().as_str())
        .collect();
    assert_eq!(
        ks,
        vec![
            "id", "title", "artist", "album", "duration", "genre", "tracknum", "year", "coverid",
            "comment"
        ]
    );
    assert_eq!(l[9]["comment"], "first comment");
    assert!(r.get("count").is_none());
    // default tags
    let r = run(&f.lib, LibQueryName::SongInfo, 0, 100, &[("track_id", &id)]);
    let l = lp(&r, "songinfo_loop");
    assert!(l.iter().any(|m| m.contains_key("bitrate")));
    assert!(l.iter().any(|m| m.contains_key("modificationTime")));
    // songinfo paging over key/value pairs
    let r = run(
        &f.lib,
        LibQueryName::SongInfo,
        1,
        2,
        &[("track_id", &id), ("tags", "al")],
    );
    assert_eq!(lp(&r, "songinfo_loop").len(), 2);
    assert!(lp(&r, "songinfo_loop")[0].contains_key("title"));

    // track_row (status style, SQL path)
    let row = f.lib.track_row(&t[0], "aAlKNcxJ");
    assert_eq!(keys(&row)[..2], ["id", "title"]);
    assert_eq!(row["artist"], "Artist A");
    assert_eq!(row["remote"], json!(0));
    assert_eq!(row["artwork_track_id"].as_str().unwrap().len(), 8);
}

#[test]
fn musicfolder_and_tracks_for() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let r = run(
        &f.lib,
        LibQueryName::MusicFolder,
        0,
        100,
        &[("tags", "cdsuto")],
    );
    let l = lp(&r, "folder_loop");
    let names: Vec<&str> = l.iter().map(|x| x["filename"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["Artist A", "Artist B", "Comp", "Cue", "EmbeddedCue"]
    );
    assert_eq!(l[0]["type"], "folder");
    assert_eq!(
        keys(l[0]),
        vec!["id", "filename", "type", "coverid", "duration", "textkey", "url", "title", "ct"]
    );
    let cue_folder = l[3]["id"].as_i64().unwrap().to_string();
    let r = run(
        &f.lib,
        LibQueryName::MusicFolder,
        0,
        100,
        &[("folder_id", &cue_folder), ("tags", "o")],
    );
    let l = lp(&r, "folder_loop");
    let entries: Vec<(&str, &str)> = l
        .iter()
        .map(|x| (x["filename"].as_str().unwrap(), x["type"].as_str().unwrap()))
        .collect();
    assert_eq!(
        entries,
        vec![("album.cue", "playlist"), ("album.flac", "unknown")]
    );

    // drill down to a track
    let a = run(&f.lib, LibQueryName::MusicFolder, 0, 1, &[]);
    let artist_a = lp(&a, "folder_loop")[0]["id"].as_i64().unwrap().to_string();
    let r = run(
        &f.lib,
        LibQueryName::MusicFolder,
        0,
        10,
        &[("folder_id", &artist_a)],
    );
    let album_one = lp(&r, "folder_loop")[0]["id"].as_i64().unwrap().to_string();
    let r = run(
        &f.lib,
        LibQueryName::MusicFolder,
        0,
        10,
        &[("folder_id", &album_one), ("tags", "d")],
    );
    let l = lp(&r, "folder_loop");
    assert_eq!(l.len(), 2);
    assert_eq!(l[0]["type"], "track");
    assert_eq!(l[0]["duration"], "0:02");
    // return_top
    let r = run(
        &f.lib,
        LibQueryName::MusicFolder,
        0,
        10,
        &[("folder_id", &album_one), ("return_top", "1")],
    );
    assert_eq!(r["count"], json!(1));
    assert_eq!(lp(&r, "folder_loop")[0]["filename"], "Album One");

    // tracks_for: folder (recursive, cue containers expanded), album, artist, genre, year
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            folder_id: Some(artist_a.parse().unwrap()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
        vec!["Song One", "Song Two"]
    );
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            folder_id: Some(cue_folder.parse().unwrap()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
        vec!["Cue One", "Cue Two"]
    );
    let album_id = t[0].album_id.unwrap();
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            album_id: Some(album_id.to_string()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t.len(), 2);
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            genre_id: None,
            year: Some(2001),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t.len(), 2);
    let va = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        1,
        &[("compilation", "1"), ("tags", "S")],
    );
    let va_id = lp(&va, "albums_loop")[0]["artist_id"].as_i64().unwrap();
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            artist_id: Some(va_id),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        t.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(),
        vec!["Hit 1", "Hit 2", "Hit 3"]
    );
    let ids: Vec<i64> = t.iter().rev().map(|x| x.id).collect();
    let t2 = f
        .lib
        .tracks_for(&TrackFilter {
            track_ids: ids.clone(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(t2.iter().map(|x| x.id).collect::<Vec<_>>(), ids);
}

#[test]
fn playlists() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let r = run(&f.lib, LibQueryName::Playlists, 0, 10, &[("tags", "us")]);
    let l = lp(&r, "playlists_loop");
    assert_eq!(l.len(), 1);
    assert_eq!(
        keys(l[0]),
        vec!["id", "playlist", "url", "favorites_url", "textkey"]
    );
    assert_eq!(l[0]["playlist"], "Favourites");
    // playlists 0 0 → no count (LMS quirk)
    let r0 = run(&f.lib, LibQueryName::Playlists, 0, 0, &[]);
    assert!(r0.get("count").is_none());

    let pid = l[0]["id"].as_i64().unwrap();
    let r = run(
        &f.lib,
        LibQueryName::PlaylistTracks,
        0,
        10,
        &[("playlist_id", &pid.to_string())],
    );
    assert_eq!(r["count"], json!(3));
    let t = lp(&r, "playlisttracks_loop");
    assert_eq!(keys(t[0])[0], "playlist index");
    assert_eq!(t[0]["title"], "Song Two");
    assert_eq!(t[1]["playlist index"], json!(1));
    assert_eq!(r["__playlistTitle"], "Favourites");
    let pl = f
        .lib
        .tracks_for(&TrackFilter {
            playlist_id: Some(pid),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pl.len(), 3);
    assert!(pl[2].remote);

    // save / edit / rename / delete
    let urls: Vec<String> = pl.iter().take(2).map(|t| t.url.clone()).collect();
    let id = f.lib.save_playlist("My: New/List", &urls).unwrap();
    let file = f.playlists.join("My  New List.m3u");
    let file = if file.exists() {
        file
    } else {
        f.playlists.join("My New List.m3u")
    };
    assert!(
        file.exists(),
        "{:?}",
        std::fs::read_dir(&f.playlists)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>()
    );
    let content = std::fs::read(&file).unwrap();
    assert!(content.starts_with(&[0xEF, 0xBB, 0xBF]));
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("#EXTURL:file:///"));
    assert!(text.contains("#EXTINF:2,Song Two"));
    let (name, _, tracks) = f.lib.playlist_by_id(id).unwrap();
    assert_eq!(name, "My: New/List");
    assert_eq!(tracks, urls);
    f.lib.playlist_edit(id, PlaylistEditOp::Down(0)).unwrap();
    assert_eq!(
        f.lib.playlist_by_id(id).unwrap().2,
        vec![urls[1].clone(), urls[0].clone()]
    );
    f.lib.playlist_edit(id, PlaylistEditOp::Delete(1)).unwrap();
    assert_eq!(f.lib.playlist_by_id(id).unwrap().2.len(), 1);
    f.lib.rename_playlist(id, "Renamed").unwrap();
    assert!(f.playlists.join("Renamed.m3u").exists());
    assert!(!file.exists());
    let r = run(&f.lib, LibQueryName::Playlists, 0, 10, &[]);
    let names: Vec<&str> = lp(&r, "playlists_loop")
        .iter()
        .map(|x| x["playlist"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Favourites", "Renamed"]);
    // a rescan of the playlist folder keeps the saved playlist
    let s = f.lib.scan(ScanMode::Playlists).unwrap();
    assert_eq!(s.deleted, 0);
    assert_eq!(f.lib.playlist_by_id(id).unwrap().2.len(), 1);
    f.lib.delete_playlist(id).unwrap();
    assert!(!f.playlists.join("Renamed.m3u").exists());
    assert!(f.lib.playlist_by_id(id).is_none());
}

#[test]
fn artwork_and_stats() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let r = run(&f.lib, LibQueryName::Albums, 0, 10, &[("tags", "lj")]);
    let albums = lp(&r, "albums_loop");
    // embedded art (flac attached picture)
    let a1 = albums.iter().find(|a| a["album"] == "Album One").unwrap();
    let cover = a1["artwork_track_id"].as_str().unwrap();
    let (bytes, ct) = f.lib.artwork(cover, "cover").unwrap();
    assert_eq!(ct, "image/png");
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (64, 48));
    let (bytes, ct) = f.lib.artwork(cover, "cover_32x32_o").unwrap();
    assert_eq!(ct, "image/png");
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (32, 24));
    let (bytes, ct) = f.lib.artwork(cover, "cover_40x40_m.jpg").unwrap();
    assert_eq!(ct, "image/jpeg");
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 40);
    // cached on disk
    assert!(f.lib.cache_dir().join("artwork").join(cover).exists());
    // folder.jpg for the mp3 album
    let a2 = albums.iter().find(|a| a["album"] == "Album Two").unwrap();
    let (bytes, ct) = f
        .lib
        .artwork(
            a2["artwork_track_id"].as_str().unwrap(),
            "thumb_20x20_p.png",
        )
        .unwrap();
    assert_eq!(ct, "image/png");
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 20);
    // numeric track id
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            search: Some("song one".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(f.lib.artwork(&t[0].id.to_string(), "cover.jpg").is_some());
    assert!(t[0].cover);
    // no artwork
    assert!(f.lib.artwork("deadbeef", "cover").is_none());
    let resized = rms_library::Library::resize_image(&bytes, "image_10x10_o.png").unwrap();
    assert_eq!(image::load_from_memory(&resized.0).unwrap().width(), 10);

    // stats
    let u = t[0].url.clone();
    f.lib.mark_played(&u);
    f.lib.mark_played(&u);
    f.lib.set_rating(&t[0].id.to_string(), 80);
    let tr = f.lib.track_by_url(&u).unwrap();
    assert_eq!(tr.playcount, Some(2));
    assert_eq!(tr.rating, Some(80));
    assert!(tr.lastplayed.is_some());
    let row = f.lib.track_row(&tr, "RO");
    assert_eq!(row["rating"], json!(80));
    assert_eq!(row["playcount"], json!(2));
    // stats survive a wipe
    f.lib.scan(ScanMode::Wipe).unwrap();
    let tr = f.lib.track_by_url(&u).unwrap();
    assert_eq!(tr.playcount, Some(2));

    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("tags", "l"), ("sort", "recentlyplayed")],
    );
    assert_eq!(lp(&r, "albums_loop")[0]["album"], "Album One");
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("tags", "l"), ("sort", "new")],
    );
    assert_eq!(r["count"], json!(5));
}

#[test]
fn library_is_send_sync_and_handles_removed_media_dirs() {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<Library>();
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    // scan from another thread, query concurrently
    let lib2 = f.lib.clone();
    let h = std::thread::spawn(move || lib2.scan(ScanMode::Full).unwrap());
    let _ = run(&f.lib, LibQueryName::Albums, 0, 10, &[]);
    h.join().unwrap();
    // narrow the media dirs: tracks outside are removed on the next full scan
    let mut cfg = f.lib.config();
    cfg.media_dirs = vec![f.media.join("Artist A")];
    f.lib.set_config(cfg);
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert!(s.deleted > 0);
    let t = f.lib.totals();
    assert_eq!(t.songs, 2);
    assert_eq!(t.albums, 1);
    // single directory rescan
    let mut cfg = f.lib.config();
    cfg.media_dirs = vec![f.media.clone()];
    f.lib.set_config(cfg);
    f.lib.scan(ScanMode::Dir(f.media.join("Artist B"))).unwrap();
    assert_eq!(f.lib.totals().songs, 3);
}

/// What a folder watcher relies on: rescanning one folder picks up new, changed and
/// removed files and removed subfolders below it, and leaves the rest alone.
#[test]
fn a_folder_rescan_sees_what_changed_below_it() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let before = f.lib.totals().songs;
    let other = f.lib.track_by_url(&rms_library::url::file_url_from_path(
        &f.media.join("Artist A/Album One/01 - Song One.flac"),
    ));
    assert!(other.is_some());
    // a file removed
    let b = f.media.join("Artist B");
    let removed = walkdir::WalkDir::new(&b)
        .into_iter()
        .filter_map(Result::ok)
        .find(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .is_some_and(|x| x == "flac" || x == "mp3")
        })
        .expect("an audio file in Artist B")
        .into_path();
    std::fs::remove_file(&removed).unwrap();
    f.lib.scan(ScanMode::Dir(b.clone())).unwrap();
    assert_eq!(f.lib.totals().songs, before - 1);
    assert!(f
        .lib
        .track_by_url(&rms_library::url::file_url_from_path(&removed))
        .is_none());
    // a whole folder removed, seen from its parent
    let gone: usize = walkdir::WalkDir::new(&b)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && f.lib
                    .track_by_url(&rms_library::url::file_url_from_path(e.path()))
                    .is_some()
        })
        .count();
    std::fs::remove_dir_all(&b).unwrap();
    f.lib.scan(ScanMode::Dir(f.media.clone())).unwrap();
    assert_eq!(f.lib.totals().songs, before - 1 - gone as u64);
    // the rest kept its ids
    let still = f.lib.track_by_url(&rms_library::url::file_url_from_path(
        &f.media.join("Artist A/Album One/01 - Song One.flac"),
    ));
    assert_eq!(still.map(|t| t.id), other.map(|t| t.id));
}

fn titles_found(lib: &Library, search: &str) -> Vec<String> {
    let r = run(lib, LibQueryName::Titles, 0, 100, &[("search", search)]);
    let mut v: Vec<String> = lp(&r, "titles_loop")
        .iter()
        .map(|t| t["title"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

/// Search goes through the full-text index: every word must start a word of the title,
/// album or artists, in any order, without regard to case or accents.
#[test]
fn full_text_search() {
    need_ffmpeg!();
    let f = common::build();
    common::make_audio(
        &f.media.join("Björk/Début/01 Human Behaviour.flac"),
        1.0,
        &[
            ("title", "Human Behaviour"),
            ("artist", "Björk"),
            ("album", "Début"),
        ],
        None,
    );
    f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!(
        titles_found(&f.lib, "song"),
        ["Song One", "Song Two", "The Mp3 Song"]
    );
    // words in any order, across title, album and artist
    assert_eq!(
        titles_found(&f.lib, "two artist"),
        ["Live Two", "Song Two", "The Mp3 Song"]
    );
    assert_eq!(titles_found(&f.lib, "song mp3"), ["The Mp3 Song"]);
    assert_eq!(titles_found(&f.lib, "album two mp3"), ["The Mp3 Song"]);
    // word starts, not the middle of words
    assert_eq!(
        titles_found(&f.lib, "so"),
        ["Song One", "Song Two", "The Mp3 Song"]
    );
    assert!(titles_found(&f.lib, "ong").is_empty());
    // case and accents
    assert_eq!(titles_found(&f.lib, "BJORK debut"), ["Human Behaviour"]);
    assert_eq!(titles_found(&f.lib, "björk"), ["Human Behaviour"]);
    // punctuation alone matches everything, quotes are just characters
    assert_eq!(titles_found(&f.lib, "\"human\""), ["Human Behaviour"]);
    // albums and artists have their own indexes
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("search", "deb"), ("tags", "l")],
    );
    assert_eq!(lp(&r, "albums_loop")[0]["album"], "Début");
    let r = run(&f.lib, LibQueryName::Artists, 0, 10, &[("search", "bjo")]);
    assert_eq!(lp(&r, "artists_loop")[0]["artist"], "Björk");
    let found = f.lib.search_all(
        "human",
        rms_library::catalog::Window::new(0, 10),
        rms_library::catalog::Window::new(0, 10),
        rms_library::catalog::Window::new(0, 10),
    );
    assert_eq!(found.tracks.len(), 1);

    // the index follows changes: a retag, a removed file, a wipe
    common::make_audio(
        &f.media.join("Björk/Début/01 Human Behaviour.flac"),
        1.5,
        &[
            ("title", "Venus as a Boy"),
            ("artist", "Björk"),
            ("album", "Début"),
        ],
        None,
    );
    f.lib.scan(ScanMode::Full).unwrap();
    assert!(titles_found(&f.lib, "human").is_empty());
    assert_eq!(titles_found(&f.lib, "venus boy"), ["Venus as a Boy"]);
    std::fs::remove_dir_all(f.media.join("Björk")).unwrap();
    f.lib.scan(ScanMode::Full).unwrap();
    assert!(titles_found(&f.lib, "venus").is_empty());
    f.lib.scan(ScanMode::Wipe).unwrap();
    assert_eq!(titles_found(&f.lib, "mp3 song"), ["The Mp3 Song"]);
}

/// A database from before the search index (and the later versions) gets it, filled, when it
/// is opened.
#[test]
fn the_search_index_is_built_for_an_older_database() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let db = f.dir.path().join("library.db");
    {
        // back to schema version 1
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "DROP TABLE tracks_fts; DROP TABLE albums_fts; DROP TABLE artists_fts;
             DROP TRIGGER tracks_fts_insert; DROP TRIGGER tracks_fts_delete;
             DROP TRIGGER tracks_fts_update; DROP TRIGGER albums_fts_insert;
             DROP TRIGGER albums_fts_delete; DROP TRIGGER albums_fts_update;
             DROP TRIGGER artists_fts_insert; DROP TRIGGER artists_fts_delete;
             DROP TRIGGER artists_fts_update;
             DROP TABLE remote_tracks; DROP TABLE track_rhythm; DROP TABLE track_chromaprint;
             DROP TABLE track_musicbrainz;
             DROP TABLE discovered; DROP TABLE web_cache;
             DROP TABLE scrobble_queue;
             DROP TABLE mb_releases; DROP TABLE track_release; DROP TABLE tag_edits;
             ALTER TABLE track_stats DROP COLUMN skip_count;
             DROP INDEX tracks_isrc; ALTER TABLE tracks DROP COLUMN isrc;
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    let lib = Library::open(&db, &f.dir.path().join("cache")).unwrap();
    assert_eq!(titles_found(&lib, "mp3"), ["The Mp3 Song"]);
    // and the later versions' tables
    let c = rusqlite::Connection::open(&db).unwrap();
    for t in [
        "remote_tracks",
        "track_rhythm",
        "track_chromaprint",
        "track_musicbrainz",
        "track_release",
        "mb_releases",
    ] {
        let n: i64 = c
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[test]
fn contributor_portraits() {
    need_ffmpeg!();
    let f = common::build();
    // artist.jpg next to the album folder, "<name>.png" in the artfolder
    common::make_image(&f.media.join("Artist A/artist.jpg"), "green", 60, 40);
    let art = f.dir.path().join("artists");
    common::make_image(&art.join("The Artist B.png"), "yellow", 30, 30);
    let mut cfg = f.lib.config();
    cfg.art_folder = Some(art.clone());
    f.lib.set_config(cfg);
    f.lib.scan(ScanMode::Full).unwrap();

    let r = run(&f.lib, LibQueryName::Artists, 0, 100, &[("tags", "s4")]);
    let artists = lp(&r, "artists_loop");
    let pid = |name: &str| {
        artists
            .iter()
            .find(|a| a["artist"] == name)
            .and_then(|a| a.get("portraitid"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let a = pid("Artist A").expect("portrait for Artist A");
    assert_eq!(a.len(), 8);
    assert!(pid("The Artist B").is_some());
    assert_eq!(pid("Xavier"), None);
    // ... in albums and track rows
    let r = run(&f.lib, LibQueryName::Albums, 0, 10, &[("tags", "la4")]);
    let albums = lp(&r, "albums_loop");
    let a1 = albums.iter().find(|x| x["album"] == "Album One").unwrap();
    assert_eq!(a1["portraitid"], json!(a));
    let t = f
        .lib
        .tracks_for(&TrackFilter {
            search: Some("song one".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(f.lib.track_row(&t[0], "a4")["portraitid"], json!(a));

    // served (and resized) by portraitid
    let (bytes, ct) = f.lib.contributor_image(&a, "image").unwrap();
    assert_eq!(ct, "image/jpeg");
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 60);
    let (bytes, _) = f.lib.contributor_image(&a, "image_20x20_o").unwrap();
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 20);
    assert!(f.lib.contributor_image("deadbeef", "image").is_none());
    assert!(f.lib.contributor_image("../x", "image").is_none());

    // a picture that was removed is dropped on the next scan
    std::fs::remove_file(f.media.join("Artist A/artist.jpg")).unwrap();
    f.lib.scan(ScanMode::Full).unwrap();
    let r = run(&f.lib, LibQueryName::Artists, 0, 100, &[("tags", "4")]);
    let artists = lp(&r, "artists_loop");
    let a_row = artists.iter().find(|x| x["artist"] == "Artist A").unwrap();
    assert!(a_row.get("portraitid").is_none());
    let missing = f.lib.contributors_without_portrait();
    assert!(missing.iter().any(|(_, n)| n == "Artist A"));
    assert!(!missing.iter().any(|(_, n)| n == "The Artist B"));
}

#[test]
fn mixes_from_the_library() {
    use rms_library::mix::Seed;
    use std::collections::HashSet;
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let all = f.lib.tracks_for(&TrackFilter::default()).unwrap();
    let id = |title: &str| all.iter().find(|t| t.title == title).unwrap().id;
    let (one, two) = (id("Song One"), id("Song Two"));
    let none = HashSet::new();

    // Song Two shares Song One's artist, album, genre (Rock) and year: the best match
    for _ in 0..5 {
        let similar = f.lib.similar_tracks(&[one], 3, &none);
        assert_eq!(similar.first(), Some(&two), "{similar:?}");
        assert!(!similar.contains(&one), "the seed is not part of its mix");
    }
    // excluded tracks (already in the playlist) are never picked
    let ex: HashSet<i64> = [two].into();
    assert!(!f.lib.similar_tracks(&[one], 5, &ex).contains(&two));

    // random songs limited to the playlist's genres
    let rock = f.lib.genres_of(&[one]);
    assert_eq!(rock.len(), 1);
    let picks = f.lib.random_tracks(10, &rock, None, &none);
    let mut picks_sorted = picks.clone();
    picks_sorted.sort();
    let mut expect = vec![one, two];
    expect.sort();
    assert_eq!(picks_sorted, expect);
    assert_eq!(
        f.lib.random_tracks(10, &rock, None, &[one].into()),
        vec![two]
    );
    assert!(f.lib.random_tracks(10, &[], Some(1999), &none).len() == 1);

    // seeds: an album's tracks in order; random picks exist
    let album = all
        .iter()
        .find(|t| t.title == "Song One")
        .unwrap()
        .album_id
        .unwrap();
    assert_eq!(f.lib.seed_tracks(Seed::Album(album)), vec![one, two]);
    assert!(f.lib.random_album(&[]).is_some());
    assert!(f.lib.random_artist().is_some());
    assert!(f.lib.random_year().is_some());
}

/// Open the library database directly (alongside the library's own connections).
fn raw_db(f: &common::Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open(f.dir.path().join("library.db")).unwrap()
}

#[test]
fn converts_a_lyrion_schema_database() {
    need_ffmpeg!();
    let f = common::build();
    let song_one = f.media.join("Artist A/Album One/01 - Song One.flac");
    let song_url = rms_library::url::file_url_from_path(&song_one);
    let md = std::fs::metadata(&song_one).unwrap();
    let (mtime, size) = {
        use std::os::unix::fs::MetadataExt;
        (md.mtime(), md.len() as i64)
    };

    // a database as earlier RMS releases wrote it
    let db = f.dir.path().join("old.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(include_str!("data/lms_schema.sql"))
            .unwrap();
        c.execute_batch(
            "INSERT INTO metainformation (name, value) VALUES ('lastRescanTime', '1700000000');
             CREATE INDEX extra ON tracks (title);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO tracks (url, title, content_type, audio) VALUES (?, 'Song One', 'flc', 1)",
            [&song_url],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tracks_persistent (url, urlmd5, added, rating, playCount, lastPlayed)
             VALUES (?, ?, 1600000000, 80, 7, 1700000000)",
            rusqlite::params![song_url, rms_library::url::md5_hex(&song_url)],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tracks_persistent (url, urlmd5, added, playCount) VALUES ('file:///gone.mp3', 'x', 1, 3)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO track_analysis (urlmd5, version, timestamp, filesize, features, loudness, peak, analyzed)
             VALUES (?, ?, ?, ?, ?, -14.0, 0.9, 1700000000)",
            rusqlite::params![
                rms_library::url::md5_hex(&song_url),
                rms_library::sonic::version(),
                mtime,
                size,
                vec![0u8; 4 * 23]
            ],
        )
        .unwrap();
    }

    let lib = Library::open(&db, &f.dir.path().join("cache2")).unwrap();
    // the old database is kept as it was
    let backup = f.dir.path().join("old.db.lms-backup");
    {
        let b = rusqlite::Connection::open(&backup).unwrap();
        let n: i64 = b
            .query_row(
                "SELECT playCount FROM tracks_persistent WHERE url = ?",
                [&song_url],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 7);
    }
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, rms_library::db::SCHEMA_VERSION);
        let old: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('metainformation', 'tracks_persistent', 'contributors')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old, 0);
        let stats: i64 = c
            .query_row("SELECT COUNT(*) FROM track_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stats, 2);
    }
    // the library itself is rebuilt by a scan
    assert_eq!(lib.totals().songs, 0);
    lib.set_config(f.lib.config());
    lib.scan(ScanMode::Full).unwrap();
    let t = lib.track_by_url(&song_url).unwrap();
    assert_eq!((t.playcount, t.rating), (Some(7), Some(80)));
    // the analysis went to the track, and the leftovers are gone
    let a = lib.analysis_totals();
    assert_eq!((a.analyzed, a.measured), (1, 1));
    let c = rusqlite::Connection::open(&db).unwrap();
    let left: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'legacy_analysis'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    // opening again changes nothing and makes no second backup
    drop(lib);
    let lib = Library::open(&db, &f.dir.path().join("cache2")).unwrap();
    assert!(!f.dir.path().join("old.db.lms-backup.1").exists());
    assert_eq!(lib.totals().songs, 10);
}

#[test]
fn history_and_analysis_follow_a_moved_file() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let from = f.media.join("Artist A/Album One/01 - Song One.flac");
    let u = rms_library::url::file_url_from_path(&from);
    f.lib.mark_played(&u);
    f.lib.mark_played(&u);
    f.lib.set_rating(&u, 60);
    {
        let c = raw_db(&f);
        c.execute(
            "INSERT INTO track_analysis (fingerprint, version, features, loudness, peak, analyzed_at)
             SELECT fingerprint, ?, NULL, -12.0, 0.5, 1 FROM tracks WHERE url = ?",
            rusqlite::params![rms_library::sonic::version(), u],
        )
        .unwrap();
    }
    assert_eq!(f.lib.analysis_totals().measured, 1);

    // move and rename the file
    let to = f.media.join("Moved/Song One (renamed).flac");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(&from, &to).unwrap();
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!((s.new, s.deleted), (1, 1), "{s:?}");
    assert!(f.lib.track_by_url(&u).is_none());
    let t = f
        .lib
        .track_by_url(&rms_library::url::file_url_from_path(&to))
        .unwrap();
    assert_eq!((t.playcount, t.rating), (Some(2), Some(60)));
    assert_eq!(f.lib.analysis_totals().measured, 1);
}

#[test]
fn a_failed_conversion_leaves_the_old_database_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("broken.db");
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        // tracks_persistent without the columns the conversion reads
        c.execute_batch(
            "CREATE TABLE metainformation (name TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE tracks (id INTEGER PRIMARY KEY, url TEXT);
             CREATE TABLE tracks_persistent (id INTEGER PRIMARY KEY, something TEXT);
             INSERT INTO tracks (url) VALUES ('file:///a.flac');",
        )
        .unwrap();
    }
    assert!(Library::open(&db, &dir.path().join("cache")).is_err());
    let c = rusqlite::Connection::open(&db).unwrap();
    let v: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 0);
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables, ["metainformation", "tracks", "tracks_persistent"]);
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert!(dir.path().join("broken.db.lms-backup").exists());
}

#[test]
fn sound_index_is_kept_between_mixes_and_reloaded_after_a_scan() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    // give one track an analysis, as the background analysis would
    {
        let c = raw_db(&f);
        let dim = rms_library::sonic::Index::empty().dim();
        let blob: Vec<u8> = vec![0.5f32; dim]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        c.execute(
            "INSERT INTO track_analysis (fingerprint, version, features, analyzed_at)
             SELECT fingerprint, ?, ?, 1 FROM tracks WHERE title = 'Song One'",
            rusqlite::params![rms_library::sonic::version(), blob],
        )
        .unwrap();
    }
    let a = f.lib.sound_index();
    assert_eq!(a.len(), 1);
    // kept: the same index, not a reload
    assert!(std::sync::Arc::ptr_eq(&a, &f.lib.sound_index()));
    // a scan may renumber tracks: reloaded afterwards
    f.lib.scan(ScanMode::Wipe).unwrap();
    let b = f.lib.sound_index();
    assert!(!std::sync::Arc::ptr_eq(&a, &b));
    assert_eq!(b.len(), 1);
}

#[test]
fn the_backup_includes_writes_still_in_the_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.sqlite");
    // an open connection in WAL mode whose last write was never checkpointed
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE metainformation (name TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE tracks_persistent (id INTEGER PRIMARY KEY, url TEXT, musicbrainz_id TEXT,
               added INTEGER, rating INTEGER, playCount INTEGER, lastPlayed INTEGER, urlmd5 TEXT);",
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO tracks_persistent (url, added, playCount) VALUES ('file:///late.flac', 1, 9)",
            [],
        )
        .unwrap();
    let wal = dir.path().join("library.sqlite-wal");
    assert!(std::fs::metadata(&wal)
        .map(|m| m.len() > 0)
        .unwrap_or(false));

    let _lib = Library::open(&db, &dir.path().join("cache")).unwrap();
    drop(writer);
    let backup = rusqlite::Connection::open(dir.path().join("library.sqlite.lms-backup")).unwrap();
    let n: i64 = backup
        .query_row(
            "SELECT playCount FROM tracks_persistent WHERE url = 'file:///late.flac'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 9);
    assert!(!dir.path().join("library.sqlite.lms-backup-wal").exists());
}

/// Features for `track_analysis`: all `v`, but `first` in the first slot.
fn features(first: f32, v: f32) -> Vec<u8> {
    let dim = rms_library::sonic::Index::empty().dim();
    let mut f = vec![v; dim];
    f[0] = first;
    f.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[test]
fn learned_streamed_tracks_join_mixes_unless_the_library_has_them() {
    use rms_library::remote::{is_remote, RemoteTrack};
    use std::collections::HashSet;
    need_ffmpeg!();
    let f = common::build();
    // a file with an ISRC tag (Vorbis comment)
    common::make_audio(
        &f.media.join("Artist C/Single/01 - Tagged.flac"),
        2.0,
        &[
            ("title", "Tagged"),
            ("artist", "Artist C"),
            ("ISRC", "gb-aye-06-01498"),
        ],
        None,
    );
    f.lib.scan(ScanMode::Full).unwrap();
    let all = f.lib.tracks_for(&TrackFilter::default()).unwrap();
    let id = |title: &str| all.iter().find(|t| t.title == title).unwrap().id;
    let one = id("Song One");
    let c = raw_db(&f);
    let isrc: Option<String> = c
        .query_row("SELECT isrc FROM tracks WHERE title = 'Tagged'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(isrc.as_deref(), Some("GBAYE0601498"));

    // three streamed tracks: one by Song One's artist, one sounding just like Song One, and
    // one the library has (by ISRC)
    let remote = |id: &str, title: &str, artist: &str, isrc: Option<&str>| RemoteTrack {
        source: "spotify".into(),
        source_id: id.into(),
        url: format!("spotify://track:{id}"),
        title: title.into(),
        artists: vec![artist.into()],
        album: Some("Elsewhere".into()),
        year: Some(2001),
        duration: Some(200.0),
        isrc: isrc.map(str::to_string),
        cover: None,
    };
    let by_artist = remote("a1", "Another Song", "Artist A", None);
    let sounds_alike = remote("s1", "Lookalike", "Someone Else", None);
    let owned = remote(
        "o1",
        "Tagged (Remastered)",
        "Artist C",
        Some("GBAYE0601498"),
    );
    let mut ids = Vec::new();
    for (t, feat) in [
        (&by_artist, features(5.0, 0.0)),
        (&sounds_alike, features(0.9, 0.9)),
        (&owned, features(0.5, 0.5)),
    ] {
        let row = f.lib.store_remote(&c, t).unwrap();
        ids.push(rms_library::remote::remote_id(row));
        c.execute(
            "INSERT INTO track_analysis (fingerprint, version, features, analyzed_at) VALUES (?, ?, ?, 1)",
            rusqlite::params![t.fingerprint(), rms_library::sonic::version(), feat],
        )
        .unwrap();
    }
    // Song One sounds like `sounds_alike` (and `owned`)
    c.execute(
        "INSERT INTO track_analysis (fingerprint, version, features, analyzed_at)
         SELECT fingerprint, ?, ?, 1 FROM tracks WHERE title = 'Song One'",
        rusqlite::params![rms_library::sonic::version(), features(0.5, 0.5)],
    )
    .unwrap();
    let (by_artist_id, alike_id, owned_id) = (ids[0], ids[1], ids[2]);
    assert!(ids.iter().all(|i| is_remote(*i)));
    assert_eq!(f.lib.sound_index().len(), 4);
    assert_eq!(f.lib.remote_totals("spotify").analyzed, 3);
    assert_eq!(
        f.lib.remote_with_local_copy(&ids),
        [owned_id].into_iter().collect::<HashSet<_>>()
    );

    let none = HashSet::new();
    let mut seen = HashSet::new();
    for _ in 0..10 {
        let mix = f.lib.similar_tracks_with(&[one], 8, &none, true);
        assert!(
            !mix.contains(&owned_id),
            "the library's copy plays instead: {mix:?}"
        );
        seen.extend(mix);
    }
    assert!(seen.contains(&alike_id), "{seen:?}");
    assert!(seen.contains(&by_artist_id), "{seen:?}");
    // without remote tracks, none
    for _ in 0..5 {
        assert!(!f
            .lib
            .similar_tracks(&[one], 8, &none)
            .iter()
            .any(|i| is_remote(*i)));
    }
    // a streamed track seeds a mix too
    assert!(!f
        .lib
        .similar_tracks_with(&[alike_id], 5, &none, true)
        .is_empty());

    // what plays them, and what they are
    let urls = vec![
        sounds_alike.url.clone(),
        "spotify://track:unknown".to_string(),
    ];
    assert_eq!(
        f.lib.remote_ids_for_urls(&urls),
        vec![(sounds_alike.url.clone(), alike_id)]
    );
    assert_eq!(f.lib.remote_track(alike_id).unwrap(), sounds_alike);

    // forgetting them
    assert_eq!(f.lib.forget_remote("spotify").unwrap(), 3);
    assert_eq!(f.lib.remote_totals("spotify").tracks, 0);
    assert_eq!(f.lib.sound_index().len(), 1);
}

#[test]
fn a_streamed_track_is_learned_from_its_samples() {
    use rms_library::remote::RemoteTrack;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.db");
    let lib = Library::open(&db, &dir.path().join("cache")).unwrap();
    // 40 s of a drum-like pulse over a chord, as a stream's decoded audio
    let sr = rms_library::sonic::SAMPLE_RATE as usize;
    let samples: Vec<f32> = (0..40 * sr)
        .map(|i| {
            let t = i as f32 / sr as f32;
            let beat = (t * 2.0).fract();
            let kick = (-beat * 30.0).exp() * (2.0 * std::f32::consts::PI * 60.0 * t).sin();
            let chord: f32 = [261.63f32, 329.63, 392.0]
                .iter()
                .map(|f| (2.0 * std::f32::consts::PI * f * t).sin())
                .sum();
            0.5 * kick + 0.1 * chord
        })
        .collect();
    let t = RemoteTrack {
        source: "spotify".into(),
        source_id: "x1".into(),
        url: "spotify://track:x1".into(),
        title: "Pulse".into(),
        artists: vec!["Synth".into()],
        duration: Some(40.0),
        ..Default::default()
    };
    assert!(!lib.remote_analyzed("spotify", "x1"));
    lib.learn_remote(&t, &samples).unwrap();
    assert!(lib.remote_analyzed("spotify", "x1"));
    assert_eq!(lib.remote_totals("spotify").analyzed, 1);
    let c = rusqlite::Connection::open(&db).unwrap();
    let (bpm, camelot): (Option<f64>, Option<String>) = c
        .query_row(
            "SELECT bpm, camelot FROM track_rhythm WHERE fingerprint = 'spotify:x1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(bpm.is_some_and(|b| (b - 120.0).abs() < 1.0), "{bpm:?}");
    assert_eq!(camelot.as_deref(), Some("8B"), "C major");
    // too short to learn
    assert!(lib.learn_remote(&t, &samples[..10 * sr]).is_err());
}

#[test]
fn a_streamed_track_is_recognised_as_a_library_file_by_its_sound() {
    use rms_library::remote::RemoteTrack;
    need_ffmpeg!();
    let f = common::build();
    let file = f.media.join("Artist D/Live/01 - Untagged.flac");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let signal = "aevalsrc='0.3*sin(2*PI*(220+55*floor(mod(t*2,7)))*t)+0.2*sin(2*PI*330*t)*exp(-6*mod(t,0.5))+0.03*(random(0)-0.5)':s=44100:d=45";
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            signal,
        ])
        .args([
            "-metadata",
            "title=Untagged",
            "-metadata",
            "artist=Artist D",
        ])
        .arg(&file)
        .status()
        .unwrap();
    assert!(ok.success());
    f.lib.scan(ScanMode::Full).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    f.lib.analyze_pending(1, Some("ffmpeg"), &stop);
    let c = raw_db(&f);
    let printed: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM track_chromaprint WHERE print IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(printed >= 1);
    let rhythms: i64 = c
        .query_row("SELECT COUNT(*) FROM track_rhythm", [], |r| r.get(0))
        .unwrap();
    assert!(rhythms >= 1, "the background pass analyses rhythm too");

    // the same audio as a stream (decoded as the Spotify capture hands it over), under other
    // tags and without an ISRC
    let pcm = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file)
        .args(["-ac", "1", "-ar", "22050", "-f", "f32le", "-"])
        .output()
        .unwrap()
        .stdout;
    let samples: Vec<f32> = pcm
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let streamed = |id: &str| RemoteTrack {
        source: "spotify".into(),
        source_id: id.into(),
        url: format!("spotify://track:{id}"),
        title: "Something Else Entirely".into(),
        artists: vec!["Nobody".into()],
        duration: Some(45.0),
        ..Default::default()
    };
    f.lib.learn_remote(&streamed("same"), &samples).unwrap();
    // and different audio of the same length
    let other: Vec<f32> = samples.iter().rev().copied().collect();
    f.lib.learn_remote(&streamed("other"), &other).unwrap();
    let ids: Vec<i64> = f
        .lib
        .remote_ids_for_urls(&[
            "spotify://track:same".into(),
            "spotify://track:other".into(),
        ])
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    let owned = f.lib.remote_with_local_copy(&ids);
    assert!(owned.contains(&ids[0]), "{owned:?}");
    assert!(!owned.contains(&ids[1]), "{owned:?}");
}

#[test]
fn skipped_discoveries_are_rejected_and_their_artist_after_two() {
    use rms_library::discover::Proposal;
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let all = f.lib.tracks_for(&TrackFilter::default()).unwrap();
    let url = |title: &str| all.iter().find(|t| t.title == title).unwrap().url.clone();
    let (one, two) = (url("Song One"), url("Song Two"));
    // nothing played yet: both are unplayed tracks by Artist A
    assert_eq!(f.lib.unplayed_by_artist(None, "Artist A", 10).len(), 2);
    for u in [&one, &two] {
        assert!(!f.lib.heard(u));
        f.lib.note_discovered(&Proposal {
            url: u.clone(),
            title: "x".into(),
            artist: "Artist A".into(),
            ..Default::default()
        });
    }
    assert!(!f.lib.heard(&one), "proposed but not played yet");
    f.lib.mark_started(&one);
    assert!(f.lib.heard(&one));
    f.lib.mark_skipped(&one);
    assert!(!f.lib.artist_rejected("Artist A"), "one rejection");
    f.lib.mark_skipped(&two);
    assert!(f.lib.artist_rejected("Artist A"), "two");
    assert!(f.lib.heard(&two));
    assert!(f.lib.unplayed_by_artist(None, "Artist A", 10).is_empty());
    let t = f.lib.discovery_totals();
    assert_eq!((t.proposed, t.played, t.rejected), (2, 0, 2));
    // skips count for any track
    let c = raw_db(&f);
    let skips: i64 = c
        .query_row(
            "SELECT skip_count FROM track_stats WHERE url = ?",
            [&one],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(skips, 1);
}

#[test]
fn listens_carry_what_the_library_knows_and_wait_in_a_queue() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let all = f.lib.tracks_for(&TrackFilter::default()).unwrap();
    let t = all.iter().find(|t| t.title == "Song One").unwrap();
    let info = f.lib.listen_info(&t.url).expect("a library track");
    assert_eq!(info.title, "Song One");
    assert_eq!(info.artist, "Artist A");
    assert!(info.duration.is_some());
    assert!(f.lib.listen_info("file:///nowhere.flac").is_none());

    f.lib.queue_listen("listenbrainz:a", 100, "{}").unwrap();
    f.lib.queue_listen("listenbrainz:a", 200, "{}").unwrap();
    f.lib.queue_listen("listenbrainz:b", 300, "{}").unwrap();
    let q = f.lib.queued_listens("listenbrainz:a", 10);
    assert_eq!(
        q.iter().map(|l| l.listened_at).collect::<Vec<_>>(),
        [100, 200]
    );
    f.lib.listens_failed(&[q[0].id], "down");
    assert_eq!(
        f.lib.listens_pending("listenbrainz:a"),
        (2, Some("down".into()))
    );
    f.lib.listens_done(&[q[0].id, q[1].id]);
    assert_eq!(f.lib.listens_pending("listenbrainz:a"), (0, None));
    f.lib.drop_listens("listenbrainz:b");
    assert_eq!(f.lib.listens_pending("listenbrainz:b").0, 0);
}

#[test]
fn a_retagged_album_is_a_compilation_no_more() {
    need_ffmpeg!();
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let comps = |lib: &Library| {
        let r = run(
            lib,
            LibQueryName::Albums,
            0,
            10,
            &[("compilation", "1"), ("tags", "la")],
        );
        lp(&r, "albums_loop")
            .iter()
            .map(|a| a["album"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(comps(&f.lib), ["Hits"]);

    // its three artists made one (as tag editing or Picard would)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let one: rms_library::tagwrite::Values =
        [("artist".to_string(), Some("One Artist".to_string()))].into();
    for n in ["01", "02", "03"] {
        let p = f.media.join(format!("Comp/Hits/{n}.m4a"));
        rms_library::tagwrite::write(&p, &one, &f.dir.path().join("scratch")).unwrap();
    }
    let s = f.lib.scan(ScanMode::Full).unwrap();
    assert_eq!(s.changed, 3, "{s:?}");
    assert!(comps(&f.lib).is_empty(), "{:?}", comps(&f.lib));
    let r = run(
        &f.lib,
        LibQueryName::Albums,
        0,
        10,
        &[("search", "hits"), ("tags", "la")],
    );
    let a = lp(&r, "albums_loop");
    assert_eq!(a.len(), 1);
    assert_eq!(a[0]["artist"], "One Artist", "{a:?}");
}
