//! A folder of loose tracks (a "downloads" folder): each track is identified on its own, the
//! folder is only a hint.  A lone song tagged with its album matches the album, not a single of
//! it; a song without an album tag shows its recording's original album's cover; a picture in
//! the folder isn't each track's own: it stays only for a track nothing identifies.  The Tags
//! page treats the tracks without an album tag as loose tracks, and won't put a cover in a
//! folder other music shares.

#[macro_use]
mod common;

use axum::extract::{Path as UrlPath, Query};
use axum::routing::get;
use axum::{Json, Router};
use common::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const SPIRIT: &str = "11111111-1111-1111-1111-111111111111";
const OTHER: &str = "22222222-2222-2222-2222-222222222222";
const ALBUM: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const SINGLE: &str = "aaaaaaaa-0000-0000-0000-000000000002";
const OTHER_ALBUM: &str = "aaaaaaaa-0000-0000-0000-000000000003";
const LOGIN: &str = "me:secret";

fn jpeg(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(32, 32, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

/// A release as listed by recording (with its group) and as looked up (with its tracks).
fn release(id: &str) -> Value {
    let (title, date, primary, tracks): (&str, &str, &str, Vec<(&str, &str, i64)>) = match id {
        ALBUM => {
            let mut t = vec![(SPIRIT, "Smells Like Teen Spirit", 3000)];
            for i in 2..=12 {
                t.push(("", "Another Song", 200_000 + i));
            }
            ("Nevermind", "1991-09-24", "Album", t)
        }
        SINGLE => (
            "Smells Like Teen Spirit",
            "1991-09-10",
            "Single",
            vec![(SPIRIT, "Smells Like Teen Spirit", 3000)],
        ),
        OTHER_ALBUM => (
            "Other Album",
            "1980",
            "Album",
            vec![(OTHER, "Other Song", 4000)],
        ),
        _ => return json!({"error": "Not Found"}),
    };
    json!({
        "id": id, "title": title, "date": date, "status": "Official",
        "artist-credit": [{"name": "Nirvana", "joinphrase": "", "artist": {"id": "artist-1", "name": "Nirvana"}}],
        "release-group": {"id": format!("rg-{id}"), "primary-type": primary, "secondary-types": [],
                          "first-release-date": date},
        "media": [{"position": 1, "format": "CD", "track-count": tracks.len(),
                   "tracks": tracks.iter().enumerate().map(|(i, (rec, t, ms))| json!({
            "id": format!("{id}-{i}"), "number": (i + 1).to_string(), "position": i + 1,
            "title": t, "length": ms, "recording": {"id": rec, "title": t}
        })).collect::<Vec<_>>()}]
    })
}

async fn mock() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let b = base.clone();
    let app = Router::new()
        .route(
            "/ws/2/recording/{id}",
            get(|UrlPath(id): UrlPath<String>| async move {
                Json(json!({"id": id, "title": "x", "genres": [], "isrcs": [],
                            "artist-credit": [{"name": "Nirvana", "artist": {"id": "artist-1"}}]}))
            }),
        )
        .route(
            "/ws/2/recording",
            get(|| async { Json(json!({"recordings": []})) }),
        )
        .route(
            "/ws/2/release",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let ids: &[&str] = match q.get("recording").map(String::as_str) {
                    // the single listed first, as it might be
                    Some(SPIRIT) => &[SINGLE, ALBUM],
                    Some(OTHER) => &[OTHER_ALBUM],
                    _ => &[],
                };
                Json(json!({"releases": ids.iter().map(|i| release(i)).collect::<Vec<_>>()}))
            }),
        )
        .route(
            "/ws/2/release/{id}",
            get(|UrlPath(id): UrlPath<String>| async move { Json(release(&id)) }),
        )
        // the Cover Art Archive
        .route(
            "/release/{id}",
            get(move |UrlPath(id): UrlPath<String>| {
                let b = b.clone();
                async move {
                    if ![ALBUM, SINGLE, OTHER_ALBUM].contains(&id.as_str()) {
                        return Err(axum::http::StatusCode::NOT_FOUND);
                    }
                    Ok(Json(
                        json!({"images": [{"id": 1, "types": ["Front"], "front": true,
                        "image": format!("{b}/img/{id}.jpg"), "thumbnails": {}}]}),
                    ))
                }
            }),
        )
        .route(
            "/img/{name}",
            get(|UrlPath(name): UrlPath<String>| async move {
                match name.trim_end_matches(".jpg") {
                    ALBUM => jpeg([0, 200, 0]),
                    SINGLE => jpeg([200, 200, 0]),
                    _ => jpeg([0, 0, 200]),
                }
            }),
        );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

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

#[tokio::test(flavor = "multi_thread")]
async fn loose_tracks_are_identified_one_by_one() {
    need_ffmpeg!();
    let url = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
    let dl = m.join("downloads");
    tone(
        &dl.join("a.flac"),
        3,
        &[
            ("artist", "Nirvana"),
            ("album", "Nevermind"),
            ("title", "Smells Like Teen Spirit"),
            ("MUSICBRAINZ_TRACKID", SPIRIT),
        ],
    );
    tone(
        &dl.join("b.flac"),
        4,
        &[
            ("artist", "Nirvana"),
            ("title", "Other Song"),
            ("MUSICBRAINZ_TRACKID", OTHER),
        ],
    );
    tone(&dl.join("c.flac"), 5, &[("title", "Nobody Knows")]);
    let red = jpeg([200, 0, 0]);
    std::fs::write(dl.join("folder.jpg"), &red).unwrap();

    let server = start_server_with(
        &m,
        &[
            "--https-port",
            "0",
            "--musicbrainz-url",
            &url,
            "--coverart-url",
            &url,
        ],
    );
    let http = server.http;
    let get = |path: String| {
        tokio::task::spawn_blocking(move || {
            let (code, body) = http_json(http, "GET", &path, None);
            (
                code,
                serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            )
        })
    };
    for _ in 0..100 {
        if get("/api/v1/server".into()).await.unwrap().1["library"]["tracks"] == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::task::spawn_blocking(move || {
        http_json(
            http,
            "PATCH",
            "/api/v1/settings",
            Some(r#"{"musicbrainz": true}"#),
        )
    })
    .await
    .unwrap();

    // each track's picture, as served
    let pictures = || async move {
        let (_, t) = get("/api/v1/tracks?limit=10".into()).await.unwrap();
        let mut out = HashMap::new();
        for t in t["items"].as_array().unwrap() {
            let Some(art) = t["artwork"].as_str().map(str::to_string) else {
                continue;
            };
            let (code, _, bytes) =
                tokio::task::spawn_blocking(move || http_bytes(http, &format!("/{art}")))
                    .await
                    .unwrap();
            if code == 200 {
                out.insert(t["title"].as_str().unwrap().to_string(), bytes);
            }
        }
        out
    };
    let (green, blue) = (jpeg([0, 200, 0]), jpeg([0, 0, 200]));
    let mut p = HashMap::new();
    for _ in 0..300 {
        p = pictures().await;
        if p.get("Smells Like Teen Spirit") == Some(&green) && p.get("Other Song") == Some(&blue) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // the album, not the single listed first; the other song's original album; the folder's
    // picture only for the track nothing identifies
    assert!(
        p.get("Smells Like Teen Spirit") == Some(&green),
        "album cover"
    );
    assert!(
        p.get("Other Song") == Some(&blue),
        "its recording's album's cover"
    );
    assert!(
        p.get("Nobody Knows") == Some(&red),
        "the folder's picture, as before"
    );

    let albums = get("/api/v1/albums".into()).await.unwrap().1;
    let id_of = |title: &str| {
        albums["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["title"] == title)
            .unwrap_or_else(|| panic!("{title}: {albums}"))["id"]
            .as_i64()
            .unwrap()
    };
    let (nevermind, loose) = (id_of("Nevermind"), id_of("No Album"));
    let (_, r) = get(format!("/api/v1/albums/{nevermind}/release"))
        .await
        .unwrap();
    assert_eq!(r["release"]["mbid"], ALBUM, "{r}");
    assert_eq!(r["status"], "matched");

    // the Tags page: loose tracks aren't an album to move; a shared folder takes no cover
    for c in [
        "pref editFiles 1",
        "pref username me",
        "pref password secret",
        "pref authorize 1",
    ] {
        cli(server.cli, c);
    }
    let call = |method: &'static str, path: String, body: Option<&'static str>| {
        tokio::task::spawn_blocking(move || {
            let (code, body) = http_json_as(http, Some(LOGIN), method, &path, body);
            (
                code,
                serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            )
        })
    };
    let (code, t) = call("GET", format!("/api/v1/albums/{loose}/tags"), None)
        .await
        .unwrap();
    assert_eq!((code, t["loose"].clone()), (200, json!(true)), "{t}");
    let (code, t) = call("GET", format!("/api/v1/albums/{nevermind}/tags"), None)
        .await
        .unwrap();
    assert_eq!((code, t["loose"].clone()), (200, json!(false)));
    let (code, _) = call("GET", format!("/api/v1/albums/{loose}/files"), None)
        .await
        .unwrap();
    assert_eq!(code, 400);
    let (code, _) = call("POST", format!("/api/v1/albums/{loose}/files"), Some("{}"))
        .await
        .unwrap();
    assert_eq!(code, 400);
    let (code, art) = call("GET", format!("/api/v1/albums/{nevermind}/cover-art"), None)
        .await
        .unwrap();
    assert_eq!(code, 200);
    assert_eq!(art["folder_shared"], true, "{art}");
    let (code, _) = call(
        "POST",
        format!("/api/v1/albums/{nevermind}/cover-art"),
        Some(r#"{"image": "1", "folder": true}"#),
    )
    .await
    .unwrap();
    assert_eq!(code, 400);
    assert_eq!(
        std::fs::read(dl.join("folder.jpg")).unwrap(),
        red,
        "left alone"
    );
    assert!(!dl.join("cover.jpg").exists());
}
