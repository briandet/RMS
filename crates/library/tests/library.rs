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
    // the referenced file is a hidden container
    let base = f
        .lib
        .track_by_url(&rms_library::url::strip_anchor(&tr.url).to_string())
        .unwrap();
    assert_eq!(base.content_type, "cur");
    assert!(!base.audio);

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
