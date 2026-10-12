//! Making and changing playlists through `/api/v1/playlists`, and the artist page's most
//! played tracks.

#[macro_use]
mod common;

use common::*;
use serde_json::{json, Value};
use std::time::Duration;

fn tone(path: &std::path::Path, hz: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency={hz}:duration=2"));
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

fn call(http: u16, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let b = body.map(|b| b.to_string());
    let (code, text) = http_json(http, method, path, b.as_deref());
    (code, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn ok(http: u16, method: &str, path: &str, body: Option<Value>) -> Value {
    let (code, v) = call(http, method, path, body);
    assert_eq!(code, 200, "{method} {path}: {v}");
    v
}

fn titles(http: u16, id: i64) -> Vec<String> {
    ok(
        http,
        "GET",
        &format!("/api/v1/playlists/{id}/tracks?limit=100"),
        None,
    )["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn playlists_are_made_and_changed() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path().join("music");
    for (i, t) in ["One", "Two", "Three"].iter().enumerate() {
        let n = (i + 1).to_string();
        tone(
            &music.join(format!("A/{n}.flac")),
            300 + 100 * i as u32,
            &[
                ("title", t),
                ("artist", "Artist A"),
                ("album", "Album A"),
                ("track", &n),
                ("RELEASETYPE", "album"),
            ],
        );
    }
    tone(
        &music.join("B/1.flac"),
        900,
        &[
            ("title", "Bee"),
            ("artist", "Artist B"),
            ("album", "B Side"),
        ],
    );
    // a playlist file among the music: the music's own, not to be changed
    std::fs::write(music.join("A/mix.m3u"), "1.flac\n2.flac\n").unwrap();
    let server = start_server_with(&music, &["--https-port", "0"]);
    let http = server.http;
    for _ in 0..100 {
        let (_, v) = call(http, "GET", "/api/v1/server", None);
        if v["library"]["tracks"] == 4 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let albums = ok(http, "GET", "/api/v1/albums?limit=10", None);
    let album_a = albums["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["title"] == "Album A")
        .unwrap();
    assert_eq!(album_a["release_type"], "album");
    let tracks = ok(http, "GET", "/api/v1/tracks?limit=10", None);
    let id_of = |t: &str| {
        tracks["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["title"] == t)
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };

    // from an album; names are unique
    let p = ok(
        http,
        "POST",
        "/api/v1/playlists",
        Some(json!({"name": " Mine ", "item": {"type": "album", "id": album_a["id"]}})),
    );
    assert_eq!(p["name"], "Mine");
    assert_eq!(p["editable"], true);
    let id = p["id"].as_i64().unwrap();
    assert_eq!(titles(http, id), ["One", "Two", "Three"]);
    let (code, e) = call(
        http,
        "POST",
        "/api/v1/playlists",
        Some(json!({"name": "Mine"})),
    );
    assert_eq!(code, 400, "{e}");

    // add before the second, reorder and remove, rename
    let r = ok(
        http,
        "POST",
        &format!("/api/v1/playlists/{id}/tracks"),
        Some(json!({"item": {"type": "track", "id": id_of("Bee")}, "position": 1})),
    );
    assert_eq!(r["count"], 1);
    assert_eq!(titles(http, id), ["One", "Bee", "Two", "Three"]);
    ok(
        http,
        "PUT",
        &format!("/api/v1/playlists/{id}/tracks"),
        Some(json!({"order": [3, 1, 0]})),
    );
    assert_eq!(titles(http, id), ["Three", "Bee", "One"]);
    let (code, _) = call(
        http,
        "PUT",
        &format!("/api/v1/playlists/{id}/tracks"),
        Some(json!({"order": [7]})),
    );
    assert_eq!(code, 400);
    let p = ok(
        http,
        "PATCH",
        &format!("/api/v1/playlists/{id}"),
        Some(json!({"name": "Ours"})),
    );
    assert_eq!(p["name"], "Ours");
    assert_eq!(
        titles(http, id),
        ["Three", "Bee", "One"],
        "kept through the rename"
    );

    // the playlist file among the music (under Folders) is not editable
    let all = ok(http, "GET", "/api/v1/playlists", None);
    assert_eq!(all["items"][0]["editable"], true);
    let root = ok(http, "GET", "/api/v1/folders", None);
    let a = root["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "A")
        .unwrap()["id"]
        .clone();
    let inside = ok(http, "GET", &format!("/api/v1/folders?id={a}"), None);
    let fid = inside["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "playlist")
        .unwrap_or_else(|| panic!("{inside}"))["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        ok(http, "GET", &format!("/api/v1/playlists/{fid}"), None)["editable"],
        false
    );
    let (code, e) = call(
        http,
        "PATCH",
        &format!("/api/v1/playlists/{fid}"),
        Some(json!({"name": "x"})),
    );
    assert_eq!(code, 400, "{e}");
    // ...but a copy can be made of it
    let copy = ok(
        http,
        "POST",
        "/api/v1/playlists",
        Some(json!({"name": "Mix copy", "item": {"type": "playlist", "id": fid}})),
    );
    assert_eq!(titles(http, copy["id"].as_i64().unwrap()), ["One", "Two"]);

    // from a player's queue
    let mut p = FakePlayer::connect(server.slim);
    for _ in 0..50 {
        if call(
            http,
            "GET",
            &format!("/api/v1/players/{}", MAC_S.replace(':', "%3A")),
            None,
        )
        .0 == 200
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ok(
        http,
        "POST",
        &format!("/api/v1/players/{}/command", MAC_S.replace(':', "%3A")),
        Some(json!({"command": "enqueue", "item": {"type": "playlist", "id": id}, "mode": "add"})),
    );
    let q = ok(
        http,
        "POST",
        "/api/v1/playlists",
        Some(json!({"name": "Queue", "from_player": MAC_S})),
    );
    assert_eq!(
        titles(http, q["id"].as_i64().unwrap()),
        ["Three", "Bee", "One"]
    );
    p.stat(b"STMt", 0);

    // deleting
    ok(http, "DELETE", &format!("/api/v1/playlists/{id}"), None);
    assert_eq!(
        call(http, "GET", &format!("/api/v1/playlists/{id}"), None).0,
        404
    );

    // the artist page: most played tracks; no similar artists without ListenBrainz
    let artists = ok(http, "GET", "/api/v1/artists?limit=10", None);
    let b = artists["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "Artist B")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        ok(
            http,
            "GET",
            &format!("/api/v1/artists/{b}/top-tracks"),
            None
        ),
        json!([])
    );
    let bee = id_of("Bee");
    for _ in 0..2 {
        assert_eq!(
            call(
                http,
                "GET",
                &format!("/rest/scrobble?id=tr-{bee}&u=x&p=y&f=json"),
                None
            )
            .0,
            200
        );
    }
    let top = ok(
        http,
        "GET",
        &format!("/api/v1/artists/{b}/top-tracks"),
        None,
    );
    assert_eq!(top[0]["title"], "Bee");
    assert_eq!(
        ok(http, "GET", &format!("/api/v1/artists/{b}/similar"), None),
        json!([])
    );
    assert_eq!(
        call(http, "GET", "/api/v1/artists/99999/top-tracks", None).0,
        404
    );
}
