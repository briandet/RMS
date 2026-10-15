//! Discovery with ListenBrainz, against a stand-in for its labs API: Create Mix takes some
//! songs ListenBrainz finds similar to the seed — from the library here — that tags and
//! sound alone would not, remembers them, and asks ListenBrainz only once a month.

use crate::common::*;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SEED: &str = "aaaaaaaa-0000-0000-0000-000000000001";
const SEED_ARTIST: &str = "aaaaaaaa-0000-0000-0000-0000000000a1";
const SIMILAR: &str = "bbbbbbbb-0000-0000-0000-000000000002";
const SIMILAR_ARTIST: &str = "cccccccc-0000-0000-0000-0000000000c1";
const NOWHERE: &str = "dddddddd-0000-0000-0000-000000000004";

async fn mock(hits: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/similar-recordings/json",
            get(|State(h): State<Arc<AtomicUsize>>, Query(q): Query<HashMap<String, String>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                if q.get("recording_mbids").map(String::as_str) != Some(SEED) {
                    return Json(json!([]));
                }
                Json(json!([
                    {"recording_mbid": SIMILAR, "recording_name": "Similar Song", "artist_credit_name": "Other Band", "score": 900},
                    // not in the library, and Spotify isn't signed in: nothing plays it
                    {"recording_mbid": NOWHERE, "recording_name": "Nowhere", "artist_credit_name": "Ghost", "score": 800}
                ]))
            }),
        )
        .route(
            "/similar-artists/json",
            get(|State(h): State<Arc<AtomicUsize>>, Query(q): Query<HashMap<String, String>>| async move {
                h.fetch_add(1, Ordering::SeqCst);
                if q.get("artist_mbids").map(String::as_str) != Some(SEED_ARTIST) {
                    return Json(json!([]));
                }
                Json(json!([{"artist_mbid": SIMILAR_ARTIST, "name": "Similar Artist", "score": 500}]))
            }),
        )
        .with_state(hits);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn tone(path: &std::path::Path, hz: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency={hz}:duration=3"));
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

async fn api(http: u16, method: &'static str, path: String, body: Option<Value>) -> Value {
    tokio::task::spawn_blocking(move || {
        let b = body.map(|b| b.to_string());
        let (code, text) = http_json(http, method, &path, b.as_deref());
        assert_eq!(code, 200, "{method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn mixes_take_songs_listenbrainz_finds_similar() {
    need_ffmpeg!();
    let hits = Arc::new(AtomicUsize::new(0));
    let url = mock(hits.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    tone(
        &m.join("seed/1.flac"),
        300,
        &[
            ("title", "Seed Song"),
            ("artist", "Seed Artist"),
            ("genre", "Rock"),
            ("MUSICBRAINZ_TRACKID", SEED),
            ("MUSICBRAINZ_ARTISTID", SEED_ARTIST),
        ],
    );
    // nothing in common with the seed but ListenBrainz's say-so
    tone(
        &m.join("other/1.flac"),
        500,
        &[
            ("title", "Similar Song"),
            ("artist", "Other Band"),
            ("genre", "Jazz"),
            ("MUSICBRAINZ_TRACKID", SIMILAR),
        ],
    );
    for (i, t) in ["First", "Second"].iter().enumerate() {
        tone(
            &m.join(format!("similar/{i}.flac")),
            700 + i as u32 * 100,
            &[
                ("title", t),
                ("artist", "Similar Artist"),
                ("genre", "Folk"),
                ("MUSICBRAINZ_ARTISTID", SIMILAR_ARTIST),
            ],
        );
    }
    let server = start_server_with(m, &["--https-port", "0", "--listenbrainz-url", &url]);
    let http = server.http;
    for _ in 0..100 {
        if api(http, "GET", "/api/v1/server".into(), None).await["library"]["tracks"] == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut p = tokio::task::spawn_blocking(move || FakePlayer::connect(server.slim))
        .await
        .unwrap();
    for _ in 0..100 {
        let ps = api(http, "GET", "/api/v1/players".into(), None).await;
        if ps.as_array().is_some_and(|a| !a.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let pid = MAC_S.replace(':', "%3A");
    let ps = api(http, "GET", format!("/api/v1/players/{pid}/settings"), None).await;
    assert_eq!(ps["adventurous"], 15, "the default");
    api(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"listenbrainz": true})),
    )
    .await;
    api(
        http,
        "PATCH",
        format!("/api/v1/players/{pid}/settings"),
        Some(json!({"adventurous": 40})),
    )
    .await;

    let lib = api(http, "GET", "/api/v1/tracks?limit=50".into(), None).await;
    let seed = lib["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "Seed Song")
        .unwrap()["id"]
        .clone();
    api(
        http,
        "POST",
        format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "mix", "seed": {"type": "track", "id": seed}})),
    )
    .await;
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    // the similar recording, and one song by the similar artist (one per artist)
    assert_eq!(s["discovered"], 2, "{s}");
    let queue = api(http, "GET", format!("/api/v1/players/{pid}/queue"), None).await;
    let titles: Vec<String> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["title"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        titles.first().map(String::as_str),
        Some("Seed Song"),
        "{titles:?}"
    );
    assert!(titles.iter().any(|t| t == "Similar Song"), "{titles:?}");
    assert!(
        titles.iter().any(|t| t == "First" || t == "Second"),
        "{titles:?}"
    );

    // the answers are cached: another mix doesn't ask again
    let asked = hits.load(Ordering::SeqCst);
    assert_eq!(asked, 2);
    api(
        http,
        "POST",
        format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "mix", "seed": {"type": "track", "id": seed}})),
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), asked);

    // off: no discovery
    api(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"listenbrainz": false})),
    )
    .await;
    api(
        http,
        "POST",
        format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "mix", "seed": {"type": "track", "id": seed}})),
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), asked);
    let _ = tokio::task::spawn_blocking(move || p.stat(b"STMt", 0)).await;
}
