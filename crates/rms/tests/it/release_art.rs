//! Cover art for albums without any: once an album is matched to its MusicBrainz release, the
//! Cover Art Archive's front cover of it shows for the album (the files untouched); a picture
//! put in the album's folder later takes over.

use crate::common::*;
use axum::extract::{Path as UrlPath, Query};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

const REC1: &str = "11111111-1111-1111-1111-111111111111";
const RELEASE: &str = "aaaaaaaa-0000-0000-0000-000000000001";

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

fn jpeg(rgb: [u8; 3]) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(32, 32, image::Rgb(rgb));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

async fn mock() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let b = base.clone();
    let app = Router::new()
        .route(
            "/ws/2/recording/{id}",
            get(|UrlPath(id): UrlPath<String>| async move {
                Json(json!({"id": id, "title": "Opening", "genres": [], "isrcs": [],
                            "artist-credit": [{"name": "The Band", "artist": {"id": "artist-1"}}]}))
            }),
        )
        .route("/ws/2/recording", get(|| async { Json(json!({"recordings": []})) }))
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
            get(|UrlPath(id): UrlPath<String>| async move {
                if id == RELEASE {
                    Json(release())
                } else {
                    Json(json!({"error": "Not Found"}))
                }
            }),
        )
        // the Cover Art Archive: a back cover listed first, then the front
        .route(
            "/release/{id}",
            get(move |UrlPath(id): UrlPath<String>| {
                let b = b.clone();
                async move {
                    if id != RELEASE {
                        return Err(axum::http::StatusCode::NOT_FOUND);
                    }
                    Ok(Json(json!({"images": [
                        {"id": 2, "types": ["Back"], "front": false, "image": format!("{b}/img/back.jpg"),
                         "thumbnails": {}},
                        {"id": 1, "types": ["Front"], "front": true, "image": format!("{b}/img/front.jpg"),
                         "thumbnails": {"1200": format!("{b}/img/front-1200.jpg")}}
                    ]})))
                }
            }),
        )
        .route(
            "/img/{name}",
            get(|UrlPath(name): UrlPath<String>| async move {
                match name.as_str() {
                    "front-1200.jpg" => jpeg([0, 200, 0]),
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
async fn albums_without_art_show_the_archive_s_front_cover() {
    need_ffmpeg!();
    let url = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
    let album = m.join("The Band/First Album");
    let meta = [("artist", "The Band"), ("album", "First Album")];
    let files = [
        album.join("1.flac"),
        album.join("2.flac"),
        album.join("3.flac"),
    ];
    tone(
        &files[0],
        3,
        &[
            meta[0],
            meta[1],
            ("title", "Opening"),
            ("track", "1"),
            ("MUSICBRAINZ_TRACKID", REC1),
        ],
    );
    tone(
        &files[1],
        4,
        &[meta[0], meta[1], ("title", "Middle Song"), ("track", "2")],
    );
    tone(
        &files[2],
        5,
        &[meta[0], meta[1], ("title", "Closer"), ("track", "3")],
    );
    let before: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();

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
            assert_eq!(code, 200, "{path}: {body}");
            serde_json::from_str::<Value>(&body).unwrap()
        })
    };
    for _ in 0..100 {
        if get("/api/v1/server".into()).await.unwrap()["library"]["tracks"] == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let id = get("/api/v1/albums".into()).await.unwrap()["items"][0]["id"]
        .as_i64()
        .unwrap();
    assert!(get(format!("/api/v1/albums/{id}"))
        .await
        .unwrap()
        .get("artwork")
        .is_none());
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

    // the album's picture, as served (none yet: None)
    let picture = || async move {
        let al = get(format!("/api/v1/albums/{id}")).await.unwrap();
        let path = al["artwork"].as_str()?.to_string();
        let (code, _, bytes) =
            tokio::task::spawn_blocking(move || http_bytes(http, &format!("/{path}")))
                .await
                .unwrap();
        (code == 200).then_some(bytes)
    };
    let green = jpeg([0, 200, 0]);
    let mut seen = None;
    for _ in 0..200 {
        seen = picture().await;
        if seen.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        seen.as_ref(),
        Some(&green),
        "the front cover, at its 1200-pixel size"
    );
    // every track has it, the files and the folder are as they were
    let al = get(format!("/api/v1/albums/{id}")).await.unwrap();
    assert!(
        al["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["artwork"].is_string()),
        "{al}"
    );
    let after: Vec<Vec<u8>> = files.iter().map(|f| std::fs::read(f).unwrap()).collect();
    assert!(before == after, "the files were written");
    assert_eq!(std::fs::read_dir(&album).unwrap().count(), 3);
    let art = get(format!("/api/v1/albums/{id}/cover-art")).await.unwrap();
    assert_eq!(art["archive"], true, "{art}");
    assert_eq!(art["embedded"], 0);

    // a picture of its own, put in the folder: it takes over
    let red = jpeg([200, 0, 0]);
    std::fs::write(album.join("folder.jpg"), &red).unwrap();
    let mut seen = None;
    for _ in 0..200 {
        seen = picture().await;
        if seen.as_ref() == Some(&red) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(seen.as_ref(), Some(&red), "the folder's picture wins");
    let art = get(format!("/api/v1/albums/{id}/cover-art")).await.unwrap();
    assert_eq!(art["archive"], false, "{art}");
}
