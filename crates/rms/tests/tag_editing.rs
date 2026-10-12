//! Changing an album's tags from what its MusicBrainz release says, through the native API:
//! only with file changes turned on and a login; the files rewritten, the library rescanned,
//! what it keeps by fingerprint (the release match) still theirs; and undone.

#[macro_use]
mod common;

use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use common::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const REC1: &str = "11111111-1111-1111-1111-111111111111";
const RELEASE: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const LOGIN: &str = "me:secret";

fn release() -> Value {
    let tracks = [
        (REC1, "Opening", 3000),
        ("22222222-2222-2222-2222-222222222222", "Middle Song", 4000),
        ("33333333-3333-3333-3333-333333333333", "Closer", 5000),
    ];
    json!({
        "id": RELEASE, "title": "First Album", "date": "1991-09-24", "status": "Official",
        "artist-credit": [{"name": "The Band", "joinphrase": "", "artist": {"id": "artist-1", "name": "The Band"}}],
        "release-group": {"id": "rg-1", "primary-type": "Album", "secondary-types": [],
                          "first-release-date": "1991-09-24"},
        "label-info": [{"catalog-number": "CAT-1", "label": {"name": "Label"}}],
        "media": [{"position": 1, "format": "CD", "tracks": tracks.iter().enumerate().map(|(i, (rec, t, ms))| json!({
            "id": format!("track-{i}"), "number": (i + 1).to_string(), "position": i + 1,
            "title": t, "length": ms, "recording": {"id": rec, "title": t},
            "artist-credit": [{"name": "The Band", "artist": {"id": "artist-1"}}]
        })).collect::<Vec<_>>()}]
    })
}

async fn mock() -> String {
    let app = Router::new()
        .route(
            "/ws/2/recording/{id}",
            get(|Path(id): Path<String>| async move {
                Json(
                    json!({"id": id, "title": "Opening", "genres": [], "isrcs": [],
                            "artist-credit": [{"name": "The Band", "artist": {"id": "artist-1"}}]}),
                )
            }),
        )
        .route(
            "/ws/2/recording",
            get(|| async { Json(json!({"recordings": []})) }),
        )
        .route(
            "/ws/2/release",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                if q.get("recording").map(String::as_str) == Some(REC1) {
                    return Json(json!({"releases": [
                        {"id": RELEASE, "status": "Official", "media": [{"track-count": 3}]}
                    ]}));
                }
                Json(json!({"releases": []}))
            }),
        )
        .route(
            "/ws/2/release/{id}",
            get(|Path(id): Path<String>| async move {
                if id == RELEASE {
                    Json(release())
                } else {
                    Json(json!({"error": "Not Found"}))
                }
            }),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn tone(path: &std::path::Path, secs: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={secs}"));
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn album_tags_are_written_from_the_release_and_undone() {
    need_ffmpeg!();
    let url = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    let album = [("artist", "The Band"), ("album", "First Album")];
    tone(
        &m.join("first/1.flac"),
        3,
        &[
            album[0],
            album[1],
            ("title", "Opening"),
            ("track", "1"),
            ("MUSICBRAINZ_TRACKID", REC1),
        ],
    );
    // a misspelt title, a missing track number
    tone(
        &m.join("first/2.flac"),
        4,
        &[album[0], album[1], ("title", "Midle Song"), ("track", "2")],
    );
    tone(
        &m.join("first/3.flac"),
        5,
        &[album[0], album[1], ("title", "Closer")],
    );
    let server = start_server_with(m, &["--https-port", "0", "--musicbrainz-url", &url]);
    let http = server.http;
    let call =
        |login: Option<&'static str>, method: &'static str, path: String, body: Option<String>| {
            tokio::task::spawn_blocking(move || {
                let (code, body) = http_json_as(http, login, method, &path, body.as_deref());
                (
                    code,
                    serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
                )
            })
        };
    let ok = |login, method, path: String, body| async move {
        let (code, v) = call(login, method, path.clone(), body).await.unwrap();
        assert_eq!(code, 200, "{method} {path}: {v}");
        v
    };
    for _ in 0..100 {
        if ok(None, "GET", "/api/v1/server".into(), None).await["library"]["tracks"] == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ok(
        None,
        "PATCH",
        "/api/v1/settings".into(),
        Some(r#"{"musicbrainz": true}"#.into()),
    )
    .await;
    let mut s = Value::Null;
    for _ in 0..300 {
        s = ok(None, "GET", "/api/v1/settings".into(), None).await;
        if s["albums_matched"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(s["albums_matched"], 1, "{s}");
    let albums = ok(None, "GET", "/api/v1/albums".into(), None).await;
    let id = albums["items"][0]["id"].as_i64().unwrap();

    // off by default; then on, but without a login
    assert_eq!(s["edit_files"], false);
    let one = json!({"changes": [{"track_id": 1, "tags": {"title": "X"}}]}).to_string();
    let (code, _) = call(None, "POST", "/api/v1/tags".into(), Some(one.clone()))
        .await
        .unwrap();
    assert_eq!(code, 403);
    let s = ok(
        None,
        "PATCH",
        "/api/v1/settings".into(),
        Some(r#"{"edit_files": true}"#.into()),
    )
    .await;
    assert!(
        s["edit_files_blocked"].as_str().unwrap().contains("login"),
        "{s}"
    );
    let (code, _) = call(None, "POST", "/api/v1/tags".into(), Some(one))
        .await
        .unwrap();
    assert_eq!(code, 403);
    for c in [
        "pref username me",
        "pref password secret",
        "pref authorize 1",
    ] {
        cli(server.cli, c);
    }
    let s = ok(Some(LOGIN), "GET", "/api/v1/settings".into(), None).await;
    assert!(s.get("edit_files_blocked").is_none(), "{s}");

    // what the release says, next to the files' tags
    let tags = ok(
        Some(LOGIN),
        "GET",
        format!("/api/v1/albums/{id}/tags"),
        None,
    )
    .await;
    assert_eq!(tags["release"]["mbid"], RELEASE, "{tags}");
    let field = |track: &Value, f: &str| {
        track["tags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["field"] == f)
            .unwrap()
            .clone()
    };
    let tracks = tags["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 3);
    let second = &tracks[1];
    assert_eq!(field(second, "title")["current"], "Midle Song", "{second}");
    assert_eq!(field(second, "title")["proposed"], "Middle Song");
    assert_eq!(field(second, "title")["changed"], true);
    assert_eq!(field(&tracks[0], "title")["changed"], false);
    assert_eq!(field(&tracks[2], "track_number")["proposed"], "3");
    assert_eq!(field(&tracks[2], "track_number")["changed"], true);

    // apply everything the release changes
    let changes: Vec<Value> = tracks
        .iter()
        .map(|t| {
            let tags: serde_json::Map<String, Value> = t["tags"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["changed"] == true)
                .map(|v| {
                    (
                        v["field"].as_str().unwrap().to_string(),
                        v["proposed"].clone(),
                    )
                })
                .collect();
            json!({"track_id": t["track_id"], "tags": tags})
        })
        .collect();
    let r = ok(
        Some(LOGIN),
        "POST",
        "/api/v1/tags".into(),
        Some(json!({ "changes": changes }).to_string()),
    )
    .await;
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(r["failed"], json!([]));
    let batch = r["batch"].as_i64().unwrap();

    // rescanned: the library has the new tags
    let titles = |a: &Value| -> Vec<String> {
        a["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap().to_string())
            .collect()
    };
    let mut a = Value::Null;
    for _ in 0..100 {
        a = ok(Some(LOGIN), "GET", format!("/api/v1/albums/{id}"), None).await;
        if titles(&a).iter().any(|t| t == "Middle Song") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(titles(&a), ["Opening", "Middle Song", "Closer"], "{a}");
    // the release match followed the rewritten files: still matched, nothing to redo
    let r = ok(
        Some(LOGIN),
        "GET",
        format!("/api/v1/albums/{id}/release"),
        None,
    )
    .await;
    assert_eq!(r["status"], "matched", "{r}");
    let s = ok(Some(LOGIN), "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["albums_pending"], 0, "{s}");
    assert_eq!(s["musicbrainz_pending"], 0, "{s}");
    // nothing more to change
    let tags = ok(
        Some(LOGIN),
        "GET",
        format!("/api/v1/albums/{id}/tags"),
        None,
    )
    .await;
    let left: Vec<&Value> = tags["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["tags"].as_array().unwrap())
        .filter(|v| v["changed"] == true)
        .collect();
    assert!(left.is_empty(), "{left:?}");

    // undone
    let edits = ok(Some(LOGIN), "GET", "/api/v1/tags/edits".into(), None).await;
    assert_eq!(edits[0]["batch"], batch, "{edits}");
    assert_eq!(edits[0]["files"], 3);
    let r = ok(
        Some(LOGIN),
        "POST",
        format!("/api/v1/tags/edits/{batch}/undo"),
        None,
    )
    .await;
    assert_eq!(r["changed"], 3, "{r}");
    for _ in 0..100 {
        a = ok(Some(LOGIN), "GET", format!("/api/v1/albums/{id}"), None).await;
        if titles(&a).iter().any(|t| t == "Midle Song") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(titles(&a).iter().any(|t| t == "Midle Song"), "{a}");
    let (code, _) = call(
        Some(LOGIN),
        "POST",
        format!("/api/v1/tags/edits/{batch}/undo"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(code, 400, "undone twice");
}
