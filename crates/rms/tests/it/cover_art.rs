//! Cover art from a stand-in for the Cover Art Archive, through the native API: the images of
//! a release listed, the one chosen saved in the album's folder and embedded in its files
//! (replacing what was there), what the library serves following; undone, all as it was.

use crate::common::*;
use axum::extract::Path as UrlPath;
use axum::routing::get;
use axum::{Json, Router};
use rms_library::tagwrite;
use serde_json::{json, Value};
use std::path::Path;

const RELEASE: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const LOGIN: &str = "me:secret";

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
            "/release/{id}",
            get(move |UrlPath(id): UrlPath<String>| {
                let b = b.clone();
                async move {
                    if id != RELEASE {
                        return Err(axum::http::StatusCode::NOT_FOUND);
                    }
                    Ok(Json(json!({"images": [
                        {"id": 2, "types": ["Back"], "front": false, "image": format!("{b}/img/2.jpg"),
                         "thumbnails": {"500": format!("{b}/img/2-500.jpg")}},
                        {"id": 1, "types": ["Front"], "front": true, "image": format!("{b}/img/1.jpg"),
                         "thumbnails": {"500": format!("{b}/img/1-500.jpg"), "1200": format!("{b}/img/1-1200.jpg")}}
                    ]})))
                }
            }),
        )
        .route(
            "/img/{name}",
            get(|UrlPath(name): UrlPath<String>| async move {
                match name.as_str() {
                    "1-1200.jpg" => jpeg([0, 200, 0]),
                    _ => b"not an image".to_vec(),
                }
            }),
        );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

fn tone(path: &Path, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=duration=2",
    ]);
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn cover_art_is_saved_and_embedded_and_undone() {
    need_ffmpeg!();
    let caa = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
    let album = m.join("The Band/First Album");
    let (a, b) = (album.join("01.flac"), album.join("02.flac"));
    for (p, t) in [(&a, "One"), (&b, "Two")] {
        tone(
            p,
            &[
                ("artist", "The Band"),
                ("album", "First Album"),
                ("title", t),
            ],
        );
    }
    // what is there: an embedded cover in one file, a folder picture
    let blue = (jpeg([0, 0, 200]), "image/jpeg".to_string());
    let red = jpeg([200, 0, 0]);
    tagwrite::write_front_cover(&a, Some(&blue), &dir.path().join("s")).unwrap();
    std::fs::write(album.join("folder.jpg"), &red).unwrap();

    let server = start_server_with(&m, &["--https-port", "0", "--coverart-url", &caa]);
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
        if v["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    for c in [
        "pref editFiles 1",
        "pref username me",
        "pref password secret",
        "pref authorize 1",
    ] {
        cli(server.cli, c);
    }
    let id = ok("GET", "/api/v1/albums".into(), None).await["items"][0]["id"]
        .as_i64()
        .unwrap();
    let q = format!("/api/v1/albums/{id}/cover-art?release={RELEASE}");

    let art = ok("GET", q.clone(), None).await;
    assert_eq!(art["images"][0]["id"], "1", "front first: {art}");
    assert_eq!(art["images"].as_array().unwrap().len(), 2);
    assert_eq!(
        (art["files"].clone(), art["embedded"].clone()),
        (json!(2), json!(1))
    );
    assert_eq!(art["folder_image"], "folder.jpg");
    // only images the archive lists; something to do
    let (code, _) = call(
        "POST",
        q.clone(),
        Some(r#"{"image": "99", "folder": true}"#.into()),
    )
    .await
    .unwrap();
    assert_eq!(code, 400);
    let (code, _) = call("POST", q.clone(), Some(r#"{"image": "1"}"#.into()))
        .await
        .unwrap();
    assert_eq!(code, 400);
    // an image that isn't one is refused before anything is written
    let (code, _) = call(
        "POST",
        q.clone(),
        Some(r#"{"image": "2", "folder": true}"#.into()),
    )
    .await
    .unwrap();
    assert_eq!(code, 502);
    assert!(album.join("folder.jpg").exists());

    // the picture of the track without an embedded cover, as served: the folder's
    let folder_art = |http: u16| async move {
        let (_, body) = tokio::task::spawn_blocking(move || {
            http_json_as(
                http,
                Some(LOGIN),
                "GET",
                &format!("/api/v1/albums/{id}"),
                None,
            )
        })
        .await
        .unwrap();
        let al: Value = serde_json::from_str(&body).unwrap();
        let t = al["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == "Two")
            .unwrap();
        let path = t["artwork"].as_str().unwrap().to_string();
        let p = path.clone();
        let (code, _, bytes) =
            tokio::task::spawn_blocking(move || http_bytes_as(http, LOGIN, &format!("/{p}")))
                .await
                .unwrap();
        assert_eq!(code, 200, "{path}");
        (path, bytes)
    };
    let green = jpeg([0, 200, 0]);
    // only the folder's picture changes, no audio file: still shown at once, under a new id
    // (so no copy of the old picture is served)
    let (old_art, bytes) = folder_art(http).await;
    assert_eq!(bytes, red);
    let r = ok(
        "POST",
        q.clone(),
        Some(r#"{"image": "1", "folder": true}"#.into()),
    )
    .await;
    assert_eq!(r["changed"], 1, "{r}");
    let (new_art, bytes) = folder_art(http).await;
    assert_ne!(new_art, old_art);
    assert_eq!(bytes, green);
    let r = ok(
        "POST",
        format!("/api/v1/tags/edits/{}/undo", r["batch"]),
        None,
    )
    .await;
    assert_eq!(r["changed"], 1, "{r}");
    assert_eq!(std::fs::read(album.join("folder.jpg")).unwrap(), red);
    assert_eq!(folder_art(http).await.1, red);

    let r = ok(
        "POST",
        q.clone(),
        Some(r#"{"image": "1", "folder": true, "embed": true}"#.into()),
    )
    .await;
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(r["failed"], json!([]));
    assert_eq!(std::fs::read(album.join("cover.jpg")).unwrap(), green);
    assert!(
        !album.join("folder.jpg").exists(),
        "set aside: cover.jpg is the folder's picture"
    );
    for p in [&a, &b] {
        assert_eq!(
            tagwrite::read_front_cover(p).unwrap().unwrap().0,
            green,
            "{p:?}"
        );
    }
    // the library serves it, and counts it
    let al = ok("GET", format!("/api/v1/albums/{id}"), None).await;
    let artwork = al["artwork"].as_str().unwrap().to_string();
    let (code, _, bytes) =
        tokio::task::spawn_blocking(move || http_bytes_as(http, LOGIN, &format!("/{artwork}")))
            .await
            .unwrap();
    assert_eq!(code, 200);
    assert_eq!(bytes, green);
    let art = ok("GET", q.clone(), None).await;
    assert_eq!(art["embedded"], 2);
    assert_eq!(art["folder_image"], "cover.jpg");

    // and back
    let edits = ok("GET", "/api/v1/tags/edits".into(), None).await;
    assert_eq!(edits[0]["kind"], "cover", "{edits}");
    let batch = edits[0]["batch"].as_i64().unwrap();
    let r = ok("POST", format!("/api/v1/tags/edits/{batch}/undo"), None).await;
    assert_eq!(r["changed"], 3, "{r}");
    assert_eq!(std::fs::read(album.join("folder.jpg")).unwrap(), red);
    assert!(!album.join("cover.jpg").exists());
    assert_eq!(tagwrite::read_front_cover(&a).unwrap().unwrap().0, blue.0);
    assert_eq!(tagwrite::read_front_cover(&b).unwrap(), None);
}
