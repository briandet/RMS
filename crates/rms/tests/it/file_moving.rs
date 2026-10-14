//! Moving an album's files where the name template says, through the native API: only with
//! file changes on and a login; covers and other files go along, the old folder goes away,
//! and what points at the files follows them (ratings, a saved playlist and its file,
//! favorites); undone, they all go back.

use crate::common::*;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const LOGIN: &str = "me:secret";

fn tone(path: &Path, secs: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={secs}"));
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

fn url_of(p: &Path) -> String {
    format!("file://{}", p.display()).replace(' ', "%20")
}

/// A file's URL as one CLI argument (the CLI unescapes its arguments).
fn cli_url(p: &Path) -> String {
    url_of(p).replace('%', "%25").replace(':', "%3A")
}

#[tokio::test(flavor = "multi_thread")]
async fn album_files_move_where_the_template_says_and_back() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
    let lists = dir.path().join("playlists");
    std::fs::create_dir_all(&lists).unwrap();
    let album = [
        ("artist", "The Band"),
        ("album", "First Album"),
        ("date", "1991"),
    ];
    let a = m.join("incoming/stuff/a.flac");
    let b = m.join("incoming/stuff/b.flac");
    tone(
        &a,
        3,
        &[
            album[0],
            album[1],
            album[2],
            ("title", "Opening"),
            ("track", "1"),
        ],
    );
    tone(
        &b,
        4,
        &[
            album[0],
            album[1],
            album[2],
            ("title", "Why?"),
            ("track", "2"),
        ],
    );
    std::fs::write(m.join("incoming/stuff/cover.jpg"), b"not really a jpeg").unwrap();
    // another album, left alone
    let other = m.join("incoming/other.flac");
    tone(
        &other,
        5,
        &[("artist", "X"), ("album", "Y"), ("title", "Z")],
    );
    std::fs::write(
        lists.join("Mine.m3u"),
        format!("#EXTM3U\n{}\n", a.display()),
    )
    .unwrap();

    let server = start_server_with(
        &m,
        &[
            "--https-port",
            "0",
            "--playlist-dir",
            lists.to_str().unwrap(),
        ],
    );
    let http = server.http;
    let call = |method: &'static str, path: String, body: Option<String>| {
        tokio::task::spawn_blocking(move || {
            let (code, body) = http_json_as(http, Some(LOGIN), method, &path, body.as_deref());
            (
                code,
                serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            )
        })
    };
    let ok = |method: &'static str, path: String, body: Option<String>| async move {
        let (code, v) = call(method, path.clone(), body).await.unwrap();
        assert_eq!(code, 200, "{method} {path}: {v}");
        v
    };
    for _ in 0..100 {
        let (_, v) = call("GET", "/api/v1/server".into(), None).await.unwrap();
        if v["library"]["tracks"] == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // a rating and a favorite for the first file
    cli(server.cli, &format!("rating {} 80", cli_url(&a)));
    ok(
        "POST",
        "/api/v1/favorites".into(),
        Some(json!({"url": url_of(&a), "title": "Opening"}).to_string()),
    )
    .await;
    // (JSON-RPC: the CLI wants a login of its own once there is one)
    let rating = |url: &str| {
        let body = json!({"id": 1, "method": "slim.request", "params": ["", ["rating", url, "?"]]})
            .to_string();
        async move {
            let v = ok("POST", "/jsonrpc.js".into(), Some(body)).await;
            v["result"]["_rating"].as_i64().unwrap_or(-1)
        }
    };
    assert_eq!(rating(&url_of(&a)).await, 80);
    let albums = ok("GET", "/api/v1/albums?q=first".into(), None).await;
    let id = albums["items"][0]["id"].as_i64().unwrap();

    // where they would go
    let plan = ok("GET", format!("/api/v1/albums/{id}/files"), None).await;
    let moves: Vec<(String, String)> = plan["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["from"].as_str().unwrap().to_string(),
                f["to"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        moves,
        [
            (
                "incoming/stuff/a.flac".into(),
                "The Band/First Album (1991)/01 Opening.flac".into()
            ),
            (
                "incoming/stuff/b.flac".into(),
                "The Band/First Album (1991)/02 Why_.flac".into()
            ),
            (
                "incoming/stuff/cover.jpg".into(),
                "The Band/First Album (1991)/cover.jpg".into()
            ),
        ],
        "{plan}"
    );
    // another template, previewed; a bad one refused
    let p = ok(
        "GET",
        format!("/api/v1/albums/{id}/files?template=%7Balbum%7D%2F%7Btitle%7D"),
        None,
    )
    .await;
    assert_eq!(p["files"][0]["to"], "First Album/Opening.flac", "{p}");
    let (code, _) = call(
        "GET",
        format!("/api/v1/albums/{id}/files?template=%7Bnope%7D"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(code, 400);

    // not without file changes and a login
    let (code, _) = call(
        "POST",
        format!("/api/v1/albums/{id}/files"),
        Some("{}".into()),
    )
    .await
    .unwrap();
    assert_eq!(code, 403);
    for c in [
        "pref editFiles 1",
        "pref username me",
        "pref password secret",
        "pref authorize 1",
    ] {
        cli(server.cli, c);
    }

    let r = ok(
        "POST",
        format!("/api/v1/albums/{id}/files"),
        Some("{}".into()),
    )
    .await;
    assert_eq!(r["changed"], 2, "{r}");
    assert_eq!(r["failed"], json!([]));
    let batch = r["batch"].as_i64().unwrap();
    let new_id = r["album_id"].as_i64().unwrap();
    let dest = m.join("The Band/First Album (1991)");
    let new_a = dest.join("01 Opening.flac");
    assert!(new_a.exists() && dest.join("02 Why_.flac").exists());
    assert!(dest.join("cover.jpg").exists(), "the cover went along");
    assert!(
        !m.join("incoming/stuff").exists(),
        "the emptied folder went away"
    );
    assert!(other.exists(), "the other album stayed");

    // the library has the files where they are now, before answering
    let a2 = ok("GET", format!("/api/v1/albums/{new_id}"), None).await;
    let urls: Vec<&str> = a2["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["url"].as_str().unwrap())
        .collect();
    assert_eq!(urls.len(), 2, "{a2}");
    assert!(
        urls.iter().all(|u| u.contains("First%20Album%20(1991)")),
        "{urls:?}"
    );
    // what pointed at the file followed it
    let new_url = urls.iter().find(|u| u.contains("Opening")).unwrap();
    assert_eq!(rating(new_url).await, 80);
    let m3u = std::fs::read_to_string(lists.join("Mine.m3u")).unwrap();
    assert!(m3u.contains("01 Opening.flac"), "{m3u}");
    let opml = std::fs::read_to_string(server._dir.path().join("prefs/favorites.opml")).unwrap();
    assert!(opml.contains("01%20Opening.flac"), "{opml}");
    let edits = ok("GET", "/api/v1/tags/edits".into(), None).await;
    assert_eq!(edits[0]["kind"], "move", "{edits}");

    // and back
    let r = ok("POST", format!("/api/v1/tags/edits/{batch}/undo"), None).await;
    assert_eq!(r["changed"], 3, "{r}");
    assert!(a.exists() && b.exists() && m.join("incoming/stuff/cover.jpg").exists());
    assert!(
        !m.join("The Band").exists(),
        "{:?}",
        std::fs::read_dir(&m).unwrap().collect::<Vec<_>>()
    );
    let m3u = std::fs::read_to_string(lists.join("Mine.m3u")).unwrap();
    assert!(m3u.contains("stuff/a.flac"), "{m3u}");
    assert_eq!(rating(&url_of(&a)).await, 80);
}
