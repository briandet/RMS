//! Matching albums to their MusicBrainz release, against a stand-in for its web service: the
//! release holding the album's recordings that its tracks fit (not a compilation with one of
//! its songs), the release the tags name, and a release the user chooses.  A confident match
//! fills in what the tags leave out: the year, track numbers, MusicBrainz ids.

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
const ORIGINAL: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const REISSUE: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const HITS: &str = "aaaaaaaa-0000-0000-0000-000000000003";
const TAGGED: &str = "bbbbbbbb-0000-0000-0000-000000000001";

fn release(id: &str) -> Value {
    let (title, date, status, tracks): (&str, &str, &str, Vec<(&str, &str, i64)>) = match id {
        ORIGINAL | REISSUE => (
            "First Album",
            if id == ORIGINAL {
                "1991-09-24"
            } else {
                "2011-09-19"
            },
            "Official",
            vec![
                (REC1, "Opening", 3000),
                ("rec-2", "Middle Song", 4000),
                ("rec-3", "Closer", 5000),
            ],
        ),
        HITS => (
            "The Hits",
            "2002",
            "Official",
            vec![
                ("rec-x", "Something Else", 200000),
                (REC1, "Opening", 3000),
                ("rec-y", "Another One", 180000),
                ("rec-z", "And Another", 240000),
            ],
        ),
        TAGGED => (
            "Second Album",
            "1994",
            "Official",
            vec![("rec-4", "Only Song", 6000)],
        ),
        _ => return json!({"error": "Not Found"}),
    };
    json!({
        "id": id, "title": title, "date": date, "status": status, "country": "US",
        "artist-credit": [{"name": "The Band", "joinphrase": "", "artist": {"id": "artist-1", "name": "The Band"}}],
        "release-group": {"id": format!("rg-{title}"), "primary-type": "Album",
                          "secondary-types": if id == HITS { json!(["Compilation"]) } else { json!([]) },
                          "first-release-date": "1991-09-24"},
        "label-info": [{"catalog-number": "CAT-1", "label": {"name": "Label"}}],
        "media": [{"position": 1, "format": "CD", "tracks": tracks.iter().enumerate().map(|(i, (rec, t, ms))| json!({
            "id": format!("{id}-{i}"), "number": (i + 1).to_string(), "position": i + 1,
            "title": t, "length": ms, "recording": {"id": rec, "title": t}
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
        // nothing found by search: only the tagged recording is identified
        .route(
            "/ws/2/recording",
            get(|| async { Json(json!({"recordings": []})) }),
        )
        .route(
            "/ws/2/release",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                if q.get("recording").map(String::as_str) == Some(REC1) {
                    // the compilation first, as MusicBrainz might list it
                    return Json(json!({"releases": [
                        {"id": HITS, "status": "Official", "media": [{"track-count": 4}]},
                        {"id": ORIGINAL, "status": "Official", "media": [{"track-count": 3}]},
                        {"id": REISSUE, "status": "Official", "media": [{"track-count": 3}]}
                    ]}));
                }
                Json(json!({"releases": []}))
            }),
        )
        .route(
            "/ws/2/release/{id}",
            get(|Path(id): Path<String>| async move { Json(release(&id)) }),
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
async fn albums_are_matched_to_their_release() {
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
    tone(
        &m.join("first/2.flac"),
        4,
        &[album[0], album[1], ("title", "Middle Song"), ("track", "2")],
    );
    // no track number, no year: filled in from the release
    tone(
        &m.join("first/3.flac"),
        5,
        &[album[0], album[1], ("title", "Closer")],
    );
    tone(
        &m.join("second/1.flac"),
        6,
        &[
            ("artist", "The Band"),
            ("album", "Second Album"),
            ("title", "Only Song"),
            ("MUSICBRAINZ_ALBUMID", TAGGED),
        ],
    );
    let server = start_server_with(m, &["--https-port", "0", "--musicbrainz-url", &url]);
    let http = server.http;
    let call = |method: &'static str, path: String, body: Option<String>| {
        tokio::task::spawn_blocking(move || {
            let (code, body) = http_json(http, method, &path, body.as_deref());
            assert_eq!(code, 200, "{method} {path}: {body}");
            serde_json::from_str::<Value>(&body).unwrap()
        })
    };
    let get = |path: String| call("GET", path, None);
    for _ in 0..100 {
        if get("/api/v1/server".into()).await.unwrap()["library"]["tracks"] == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let albums = get("/api/v1/albums".into()).await.unwrap();
    let id_of = |title: &str| {
        albums["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["title"] == title)
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let (first, second) = (id_of("First Album"), id_of("Second Album"));
    let r = get(format!("/api/v1/albums/{first}/release"))
        .await
        .unwrap();
    assert_eq!(r["status"], "pending", "{r}");

    call(
        "PATCH",
        "/api/v1/settings".into(),
        Some(r#"{"musicbrainz": true}"#.into()),
    )
    .await
    .unwrap();
    let mut s = Value::Null;
    for _ in 0..300 {
        s = get("/api/v1/settings".into()).await.unwrap();
        if s["albums_pending"] == 0 && s["albums_matched"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(s["albums_matched"], 2, "{s}");

    // the release the album is, not the compilation listed first; the original over the
    // reissue (the same tracks), for its date
    let r = get(format!("/api/v1/albums/{first}/release"))
        .await
        .unwrap();
    assert_eq!(r["status"], "matched", "{r}");
    assert_eq!(r["how"], "auto");
    assert_eq!(r["release"]["mbid"], ORIGINAL, "{r}");
    assert_eq!(r["release"]["format"], "CD");
    assert_eq!(r["release"]["label"], "Label");
    let positions: Vec<i64> = r["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["position"].as_i64().unwrap())
        .collect();
    assert_eq!(positions, [1, 2, 3], "{r}");
    let cands: Vec<&str> = r["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["mbid"].as_str().unwrap())
        .collect();
    assert_eq!(cands.len(), 3, "{r}");
    assert_eq!(
        *cands.last().unwrap(),
        HITS,
        "the compilation fits least: {r}"
    );

    // filled in: the year, the missing track number
    let a = get(format!("/api/v1/albums/{first}")).await.unwrap();
    assert_eq!(a["year"], 1991, "{a}");
    let numbers: Vec<Value> = a["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["track_number"].clone())
        .collect();
    assert_eq!(numbers, [json!(1), json!(2), json!(3)], "{a}");

    // the release the tags name
    let r = get(format!("/api/v1/albums/{second}/release"))
        .await
        .unwrap();
    assert_eq!(r["how"], "tag", "{r}");
    assert_eq!(r["release"]["mbid"], TAGGED);

    // the user chooses the reissue
    let r = call(
        "PUT",
        format!("/api/v1/albums/{first}/release"),
        Some(format!(r#"{{"mbid": "{REISSUE}"}}"#)),
    )
    .await
    .unwrap();
    assert_eq!(r["how"], "user", "{r}");
    assert_eq!(r["status"], "matched");
    assert_eq!(r["release"]["mbid"], REISSUE);
    assert_eq!(r["candidates"].as_array().unwrap().len(), 3, "{r}");
    let (code, body) = tokio::task::spawn_blocking(move || {
        http_json(
            http,
            "PUT",
            &format!("/api/v1/albums/{first}/release"),
            Some(r#"{"mbid": "not-an-id"}"#),
        )
    })
    .await
    .unwrap();
    assert_eq!(code, 400, "{body}");

    // forgotten: matched again
    let r = call("DELETE", format!("/api/v1/albums/{first}/release"), None)
        .await
        .unwrap();
    assert_eq!(r["status"], "pending", "{r}");
}
