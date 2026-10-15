//! The Table's queries over the standard test library (see `common::build`): ten tracks on
//! Album One (2), Album Two (1, genres Jazz and Blues), the compilation Hits (3), Cue Album (2)
//! and Live Album (2, no genre tag: "No Genre").

use crate::common;

use rms_library::table::{TablePage, TableQuery};
use rms_library::{Library, ScanMode};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000;

fn run(lib: &Library, q: &str, group: &str, cols: &[&str], sort: &[(&str, bool)]) -> TablePage {
    lib.table(
        &TableQuery {
            q: q.into(),
            group: Some(group.into()),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            sort: sort.iter().map(|(s, d)| (s.to_string(), *d)).collect(),
            offset: 0,
            limit: 100,
        },
        NOW,
    )
    .unwrap_or_else(|e| panic!("{q:?} by {group}: {e}"))
}

/// Column `i` of every row.
fn col(p: &TablePage, i: usize) -> Vec<Value> {
    p.rows.iter().map(|r| r.cells[i].clone()).collect()
}

fn titles(p: &TablePage) -> Vec<String> {
    col(p, 0)
        .into_iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn the_table() {
    if !common::ffmpeg_available() {
        eprintln!("ffmpeg not available - skipping");
        return;
    }
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let lib = &f.lib;

    // every track, in the library's order, with its totals
    let all = run(
        lib,
        "",
        "track",
        &["title", "album", "year", "format", "genre"],
        &[],
    );
    assert_eq!(all.total, 10);
    assert_eq!(all.tracks, 10);
    assert!(all.duration > 15.0, "{}", all.duration);
    assert!(all.rows.iter().all(|r| r.id.is_some() && r.drill.is_none()));
    // several genres to a track
    let mp3 = all
        .rows
        .iter()
        .find(|r| r.cells[0] == "The Mp3 Song")
        .unwrap();
    assert_eq!(mp3.cells[4], "Blues; Jazz");
    assert_eq!(mp3.cells[3], "mp3");

    // filters
    assert_eq!(
        titles(&run(lib, "year:1999", "track", &["title"], &[])),
        ["The Mp3 Song"]
    );
    assert_eq!(
        run(lib, "year:1990..2001", "track", &["title"], &[]).total,
        3
    );
    assert_eq!(run(lib, "year:<1990", "track", &["title"], &[]).total, 2);
    assert_eq!(run(lib, "format:flc", "track", &["title"], &[]).total, 6);
    assert_eq!(run(lib, "-format:flc", "track", &["title"], &[]).total, 4);
    assert_eq!(
        run(lib, "format:mp3,mp4", "track", &["title"], &[]).total,
        4
    );
    assert_eq!(
        titles(&run(lib, "genre:jazz", "track", &["title"], &[])),
        ["The Mp3 Song"]
    );
    assert_eq!(
        run(lib, "album:=\"album one\"", "track", &["title"], &[]).total,
        2
    );
    assert_eq!(run(lib, "album:=album", "track", &["title"], &[]).total, 0);
    assert_eq!(run(lib, "missing:year", "track", &["title"], &[]).total, 5);
    assert_eq!(run(lib, "has:year", "track", &["title"], &[]).total, 5);
    // untagged tracks are in "No Genre"
    assert_eq!(run(lib, "missing:genre", "track", &["title"], &[]).total, 0);
    assert_eq!(
        run(lib, "genre:\"no genre\"", "track", &["title"], &[]).total,
        2
    );
    assert_eq!(run(lib, "duration:>1.5", "track", &["title"], &[]).total, 7);
    assert_eq!(
        run(lib, "path:\"Comp/Hits\"", "track", &["title"], &[]).total,
        3
    );
    // any of a track's artists
    assert_eq!(
        titles(&run(lib, "artist:yolanda", "track", &["title"], &[])),
        ["Hit 2"]
    );
    // words search as everywhere, and combine with fields
    assert_eq!(run(lib, "song", "track", &["title"], &[]).total, 3);
    assert_eq!(
        titles(&run(lib, "song -mp3 track:2", "track", &["title"], &[])),
        ["Song Two"]
    );
    // added just now (by the clock the test passes)
    assert_eq!(run(lib, "added:<30d", "track", &["title"], &[]).total, 0);
    let e = lib
        .table(
            &TableQuery {
                q: "colour:red".into(),
                ..Default::default()
            },
            NOW,
        )
        .unwrap_err();
    assert!(e.to_string().contains("no field called"), "{e}");

    // sorting: descending year, unknown years last
    let by_year = run(lib, "", "track", &["title", "year"], &[("year", true)]);
    assert_eq!(
        col(&by_year, 1)[..4],
        [json!(2001), json!(2001), json!(1999), json!(1985)]
    );
    assert_eq!(col(&by_year, 1)[9], Value::Null);
    let by_track = run(
        lib,
        "album:\"album one\"",
        "track",
        &["title"],
        &[("track", true)],
    );
    assert_eq!(titles(&by_track), ["Song Two", "Song One"]);

    // albums: one row each, with how many tracks, the one value they share, or varies
    let albums = run(
        lib,
        "",
        "album",
        &["album", "count", "artist", "year", "duration"],
        &[],
    );
    assert_eq!(albums.total, 5);
    let hits = albums
        .rows
        .iter()
        .find(|r| r.label == Some(json!("Hits")))
        .unwrap();
    assert_eq!(hits.cells[1], 3);
    assert_eq!(hits.cells[2], json!({ "varies": 3 }));
    assert_eq!(hits.cells[3], Value::Null);
    let one = albums
        .rows
        .iter()
        .find(|r| r.label == Some(json!("Album One")))
        .unwrap();
    assert_eq!(one.cells[2], "Artist A");
    assert_eq!(one.cells[3], 2001);
    assert!(
        (one.cells[4].as_f64().unwrap() - 4.0).abs() < 0.2,
        "{:?}",
        one.cells[4]
    );
    assert_eq!(one.next, Some("track"));
    // a row's tracks, found by its drill-down query
    let drill = one.drill.clone().unwrap();
    assert!(drill.starts_with("album_id:"), "{drill}");
    assert_eq!(
        titles(&run(lib, &drill, "track", &["title"], &[])),
        ["Song One", "Song Two"]
    );
    // most tracks first
    let biggest = run(lib, "", "album", &["album", "count"], &[("count", true)]);
    assert_eq!(biggest.rows[0].cells[0], "Hits");
    // albums whose tracks have more than one artist
    let mixed = run(lib, "varies:artist", "album", &["album", "count"], &[]);
    assert_eq!(titles(&mixed), ["Hits"]);
    assert_eq!(mixed.tracks, 3);
    assert!(lib
        .table(
            &TableQuery {
                q: "varies:artist".into(),
                ..Default::default()
            },
            NOW
        )
        .is_err());

    // genres: a track in two genres counts in both
    let genres = run(lib, "", "genre", &["genre", "count"], &[]);
    let names: Vec<_> = genres
        .rows
        .iter()
        .map(|r| r.label.clone().unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        names,
        [
            json!("Blues"),
            json!("Classical"),
            json!("Jazz"),
            json!("No Genre"),
            json!("Pop"),
            json!("Rock")
        ]
    );
    assert_eq!(
        col(&genres, 1),
        [json!(1), json!(2), json!(1), json!(2), json!(3), json!(2)]
    );
    for r in &genres.rows {
        let tracks = run(lib, r.drill.as_deref().unwrap(), "track", &["title"], &[]);
        assert_eq!(json!(tracks.total), r.cells[1], "{:?}", r.drill);
    }

    // years and formats group by their value; a row's tracks come back by it
    let years = run(lib, "", "year", &["year", "count"], &[]);
    assert_eq!(years.total, 4);
    for r in &years.rows {
        assert_eq!(
            json!(run(lib, r.drill.as_deref().unwrap(), "track", &["title"], &[]).total),
            r.cells[1]
        );
    }
    let folders = run(lib, "", "folder", &["folder", "count"], &[]);
    assert_eq!(folders.total, 5);
    for r in &folders.rows {
        assert_eq!(
            json!(run(lib, r.drill.as_deref().unwrap(), "track", &["title"], &[]).total),
            r.cells[1],
            "{:?}",
            r.drill
        );
    }
    let lossless = run(lib, "", "lossless", &["lossless", "count"], &[]);
    assert_eq!(col(&lossless, 0), [json!(false), json!(true)]);

    // the next pages come from the first's result
    let page = |offset| {
        lib.table(
            &TableQuery {
                q: String::new(),
                group: None,
                columns: vec!["title".into()],
                sort: vec![("title".into(), false)],
                offset,
                limit: 4,
            },
            NOW,
        )
        .unwrap()
    };
    let (p1, p2, p3) = (page(0), page(4), page(8));
    assert_eq!(
        (p1.total, p1.rows.len(), p2.rows.len(), p3.rows.len()),
        (10, 4, 4, 2)
    );
    // the same rows as all at once (titles sort without "The")
    let got: Vec<String> = [p1, p2, p3].iter().flat_map(titles).collect();
    assert_eq!(
        got,
        titles(&run(lib, "", "track", &["title"], &[("title", false)]))
    );
    assert_eq!(got[7], "The Mp3 Song");

    // the tracks to play for a query
    let ids = lib.table_track_ids("album:hits", &[], 100, NOW).unwrap();
    assert_eq!(ids.len(), 3);
}

/// MusicBrainz's release for Album One, its recordings identified, and EffNet's labels, as the
/// lookups and the analysis store them: the Table reads them as columns.
#[test]
fn musicbrainz_and_effnet_columns() {
    if !common::ffmpeg_available() {
        eprintln!("ffmpeg not available - skipping");
        return;
    }
    let f = common::build();
    f.lib.scan(ScanMode::Full).unwrap();
    let lib = &f.lib;
    let c = rusqlite::Connection::open(f.dir.path().join("library.db")).unwrap();
    let print = |title: &str| -> String {
        c.query_row(
            "SELECT fingerprint FROM tracks WHERE title = ?",
            [title],
            |r| r.get(0),
        )
        .unwrap()
    };
    let release = json!({
        "id": "rel-1", "title": "Album One (Deluxe)", "status": "Official", "date": "2003-02-01",
        "country": "GB", "barcode": "5012345678900",
        "artist-credit": [{"name": "Artist A", "joinphrase": " & "}, {"name": "Friends"}],
        "release-group": {"id": "rg-1", "first-release-date": "2001-05-01"},
        "label-info": [{"catalog-number": "CAT 001", "label": {"name": "Good Label"}}],
        "media": [{"position": 1, "format": "CD", "tracks": [
            {"title": "Song One", "artist-credit": [], "recording": {"artist-credit": [{"name": "Artist A"}]}},
            {"title": "Song Two (Remix)", "artist-credit": [{"name": "Artist A", "joinphrase": " feat. "}, {"name": "B"}]}
        ]}]
    });
    c.execute(
        "INSERT INTO mb_releases (mbid, body, fetched_at) VALUES ('rel-1', ?, 0)",
        [release.to_string()],
    )
    .unwrap();
    for (pos, title) in [(1, "Song One"), (2, "Song Two")] {
        c.execute(
            "INSERT INTO track_release (fingerprint, release_mbid, medium, position, score, how, matched_at)
             VALUES (?, 'rel-1', 1, ?, 0.93, 'auto', 0)",
            rusqlite::params![print(title), pos],
        )
        .unwrap();
        c.execute(
            "INSERT INTO track_musicbrainz (fingerprint, mbid, artist_mbid, genres, how, looked_up_at)
             VALUES (?, 'rec', 'art-1', 'rock' || char(31) || 'indie rock', 'acoustid', 0)",
            [print(title)],
        )
        .unwrap();
    }
    for (title, happy, style) in [
        ("Song One", 0.82, "Rock---Indie Rock"),
        ("Song Two", 0.31, "Rock---Indie Rock"),
        ("Hit 1", 0.6, "Electronic---House"),
    ] {
        let labels = json!({
            "styles": [[style, 0.7], ["Pop---Synth-pop", 0.2]],
            "moods": [["happy", happy], ["sad", 1.0 - happy], ["danceable", 0.5]],
            "themes": [["summer", 0.4]], "instruments": [["guitar", 0.6]]
        });
        // as the analysis stores it: the labels, and the likeliest ones and the moods as columns
        c.execute(
            "INSERT INTO track_effnet (fingerprint, version, embedding, labels, style, theme, instrument, happy, sad, danceable)
             VALUES (?, 1, NULL, ?, ?, 'summer', 'guitar', ?, ?, 0.5)",
            rusqlite::params![print(title), labels.to_string(), style, happy, 1.0 - happy],
        )
        .unwrap();
    }

    let cols = [
        "title",
        "mb_title",
        "mb_artist",
        "mb_album",
        "mb_album_artist",
        "label",
        "catalog_number",
        "barcode",
        "country",
        "release_status",
        "medium",
        "mb_year",
        "mb_match",
        "mb_score",
        "mb_genres",
        "mb_identified",
        "happy",
        "style",
        "theme",
    ];
    let p = run(lib, "album:\"album one\"", "track", &cols, &[]);
    assert_eq!(
        p.rows[0].cells,
        [
            json!("Song One"),
            json!("Song One"),
            json!("Artist A"),
            json!("Album One (Deluxe)"),
            json!("Artist A & Friends"),
            json!("Good Label"),
            json!("CAT 001"),
            json!("5012345678900"),
            json!("GB"),
            json!("official"),
            json!("CD"),
            json!(2001),
            json!("auto"),
            json!(0.93),
            json!("rock; indie rock"),
            json!("acoustid"),
            json!(0.82),
            json!("Rock / Indie Rock"),
            json!("summer"),
        ]
    );
    // the track's own artist credit, with its join phrase
    assert_eq!(p.rows[1].cells[2], "Artist A feat. B");

    // filters, sorting and grouping on them
    assert_eq!(
        titles(&run(lib, "happy:>70", "track", &["title"], &[])),
        ["Song One"]
    );
    assert_eq!(run(lib, "happy:30..90", "track", &["title"], &[]).total, 3);
    assert_eq!(run(lib, "label:good", "track", &["title"], &[]).total, 2);
    assert_eq!(run(lib, "missing:label", "track", &["title"], &[]).total, 8);
    assert_eq!(
        titles(&run(
            lib,
            "has:happy",
            "track",
            &["title"],
            &[("happy", true)]
        )),
        ["Song One", "Hit 1", "Song Two"]
    );
    assert_eq!(run(lib, "styles:synth", "track", &["title"], &[]).total, 3);
    assert_eq!(run(lib, "style:house", "track", &["title"], &[]).total, 1);
    let styles = run(lib, "has:style", "style", &["style", "count", "happy"], &[]);
    assert_eq!(
        col(&styles, 0),
        [json!("Electronic / House"), json!("Rock / Indie Rock")]
    );
    let rock = &styles.rows[1];
    assert_eq!(rock.cells[1], 2);
    assert!(
        (rock.cells[2].as_f64().unwrap() - 0.565).abs() < 1e-9,
        "{:?}",
        rock.cells[2]
    );
    assert_eq!(
        run(
            lib,
            rock.drill.as_deref().unwrap(),
            "track",
            &["title"],
            &[]
        )
        .total,
        2
    );
    let labels = run(lib, "", "label", &["label", "count"], &[]);
    assert_eq!(col(&labels, 0), [json!("Good Label"), Value::Null]);

    // tags that differ from MusicBrainz
    assert_eq!(
        titles(&run(lib, "differs:title", "track", &["title"], &[])),
        ["Song Two"]
    );
    assert_eq!(run(lib, "differs:album", "track", &["title"], &[]).total, 2);
    assert_eq!(run(lib, "differs:year", "track", &["title"], &[]).total, 0);
    let e = lib
        .table(
            &TableQuery {
                q: "differs:format".into(),
                ..Default::default()
            },
            NOW,
        )
        .unwrap_err();
    assert!(e.to_string().contains("compares title"), "{e}");
}
