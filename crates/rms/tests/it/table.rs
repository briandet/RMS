//! The Table through the native API: `POST /api/v1/query`, its fields, saved views, and
//! queueing what a query finds.

use crate::common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn call(port: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let (code, text) = http_json(port, method, path, body.map(|b| b.to_string()).as_deref());
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn query(port: u16, body: Value) -> Value {
    let (code, page) = call(port, "POST", "/api/v1/query", Some(body.clone()));
    assert_eq!(code, 200, "{body}: {page}");
    page
}

#[test]
fn the_table_over_the_api() {
    need_ffmpeg!();
    let music = tempfile::tempdir().unwrap();
    let tracks = [
        ("Band/One/01 Alpha.flac", "Alpha", "One", "1999"),
        ("Band/One/02 Beta.flac", "Beta", "One", "1999"),
        ("Band/Two/01 Gamma.mp3", "Gamma", "Two", "2005"),
        ("Band/Two/02 Delta.mp3", "Delta", "Two", "2006"),
    ];
    for (i, (path, title, album, year)) in tracks.iter().enumerate() {
        make_audio(
            &music.path().join(path),
            i as u32 + 1,
            &[
                ("title", title),
                ("artist", "Band"),
                ("album", album),
                ("date", year),
                ("track", &(i % 2 + 1).to_string()),
            ],
        );
    }
    let server = start_server(music.path());
    let http = server.http;

    // every track, once the scan has found them
    let deadline = Instant::now() + Duration::from_secs(20);
    let all = loop {
        let p = query(
            http,
            json!({"columns": ["title", "album", "year", "format"]}),
        );
        if p["total"] == 4 {
            break p;
        }
        assert!(Instant::now() < deadline, "not scanned: {p}");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(all["tracks"], 4);
    let titles: Vec<&str> = all["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["cells"][0].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Alpha", "Beta", "Gamma", "Delta"]);
    assert!(all["rows"][0]["id"].is_i64());

    // albums, newest first, with their tracks counted and their years as a range
    let albums = query(
        http,
        json!({"q": "band", "group": "album", "columns": ["album", "count", "year", "format"],
               "sort": [{"field": "year", "desc": true}]}),
    );
    assert_eq!(albums["total"], 2);
    let two = &albums["rows"][0];
    assert_eq!(two["label"], "Two");
    assert_eq!(two["cells"], json!(["Two", 2, [2005, 2006], "mp3"]));
    assert_eq!(two["next"], "track");
    let drill = two["drill"].as_str().unwrap();
    let inside = query(http, json!({"q": drill, "columns": ["title"]}));
    assert_eq!(inside["total"], 2);

    // a mistake in the query is a 400 that says what's wrong
    let (code, e) = call(
        http,
        "POST",
        "/api/v1/query",
        Some(json!({"q": "colour:red", "columns": ["title"]})),
    );
    assert_eq!(code, 400);
    assert!(e["error"].as_str().unwrap().contains("colour"), "{e}");
    let (code, _) = call(
        http,
        "POST",
        "/api/v1/query",
        Some(json!({"group": "title", "columns": ["title"]})),
    );
    assert_eq!(code, 400);

    // the columns there are
    let (code, fields) = call(http, "GET", "/api/v1/query/fields", None);
    assert_eq!(code, 200);
    let year = fields
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "year")
        .unwrap();
    assert_eq!(
        year,
        &json!({"name": "year", "label": "Year", "kind": "int", "section": "Tags", "agg": "range", "next": "album"})
    );
    assert!(!fields
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["name"] == "album_id"));

    // saved views
    let (_, views) = call(http, "GET", "/api/v1/query/views", None);
    assert_eq!(views, json!([]));
    let view = json!({"name": "Old", "q": "year:<2000", "group": "album", "columns": ["album", "count"],
                      "sort": [{"field": "count", "desc": true}], "widths": {"album": 240.0}});
    let (code, saved) = call(http, "PUT", "/api/v1/query/views", Some(json!([view])));
    assert_eq!(code, 200, "{saved}");
    assert_eq!(
        call(http, "GET", "/api/v1/query/views", None).1,
        json!([view])
    );
    let (code, _) = call(
        http,
        "PUT",
        "/api/v1/query/views",
        Some(json!([view, view])),
    );
    assert_eq!(code, 400, "two views with one name");

    // play what a query finds
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));
    let path = format!("/api/v1/players/{}/command", MAC_S.replace(':', "%3A"));
    let (code, r) = call(
        http,
        "POST",
        &path,
        Some(json!({"command": "enqueue", "mode": "add",
                    "item": {"type": "query", "q": "album:two", "sort": [{"field": "title"}]}})),
    );
    assert_eq!(code, 200, "{r}");
    assert_eq!(r["count"], 2);
    let (_, q) = call(
        http,
        "GET",
        &format!("/api/v1/players/{}/queue", MAC_S.replace(':', "%3A")),
        None,
    );
    let queued: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(queued, ["Delta", "Gamma"]);
}
