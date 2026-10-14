//! Identifying recordings with MusicBrainz, against a stand-in for its web service: by the
//! MusicBrainz id in the tags, by ISRC, and by search (taking the studio recording over a
//! live one); genres fill in for tracks without a genre tag, tags win otherwise.

use crate::common::*;
use axum::extract::{Path, Query};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const TAGGED: &str = "11111111-1111-1111-1111-111111111111";
const BY_ISRC: &str = "22222222-2222-2222-2222-222222222222";
const STUDIO: &str = "33333333-3333-3333-3333-333333333333";
const LIVE: &str = "44444444-4444-4444-4444-444444444444";

fn recording(id: &str) -> Value {
    let (genres, artist_genres, isrcs) = match id {
        TAGGED => (
            json!([{"name": "jazz", "count": 3}]),
            json!([]),
            json!(["USAAA0000001"]),
        ),
        BY_ISRC => (
            json!([]),
            json!([{"name": "dream pop", "count": 9}, {"name": "shoegaze", "count": 5}, {"name": "rock", "count": 1}]),
            json!(["GBBBB0000002"]),
        ),
        STUDIO => (
            json!([{"name": "grunge", "count": 10}, {"name": "alternative rock", "count": 4}]),
            json!([]),
            json!(["USGF19942501"]),
        ),
        _ => (json!([]), json!([]), json!([])),
    };
    json!({
        "id": id, "title": "x", "genres": genres, "isrcs": isrcs,
        "artist-credit": [{"name": "A", "artist": {"id": "aaaaaaaa-0000-0000-0000-000000000000", "genres": artist_genres}}]
    })
}

async fn mock() -> String {
    let app = Router::new()
        .route(
            "/ws/2/recording/{id}",
            get(|Path(id): Path<String>| async move { Json(recording(&id)) }),
        )
        .route(
            "/ws/2/recording",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let query = q.get("query").cloned().unwrap_or_default();
                if !query.contains("Teen Spirit") {
                    return Json(json!({"recordings": []}));
                }
                // a live recording of about the same length scores as high
                Json(json!({"recordings": [
                    {"id": LIVE, "score": 100, "length": 300500,
                     "releases": [{"release-group": {"primary-type": "Album", "secondary-types": ["Live"]}}]},
                    {"id": STUDIO, "score": 100, "length": 301133,
                     "releases": [{"release-group": {"primary-type": "Album", "secondary-types": []}}]}
                ]}))
            }),
        )
        .route(
            "/ws/2/isrc/{isrc}",
            get(|Path(isrc): Path<String>| async move {
                if isrc == "GBBBB0000002" {
                    Json(json!({"isrc": isrc, "recordings": [{"id": BY_ISRC, "length": 200000}]}))
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
async fn recordings_are_identified_and_their_genres_filled_in() {
    need_ffmpeg!();
    let url = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    tone(
        &m.join("a/1.flac"),
        3,
        &[
            ("title", "Tagged"),
            ("artist", "A"),
            ("MUSICBRAINZ_TRACKID", TAGGED),
        ],
    );
    tone(
        &m.join("b/1.flac"),
        200,
        &[("title", "Isrc"), ("artist", "B"), ("ISRC", "GBBBB0000002")],
    );
    tone(
        &m.join("c/1.flac"),
        301,
        &[("title", "Smells Like Teen Spirit"), ("artist", "Nirvana")],
    );
    // tags win: a genre tag stays
    tone(
        &m.join("d/1.flac"),
        302,
        &[
            ("title", "Smells Like Teen Spirit"),
            ("artist", "Nirvana"),
            ("album", "Live"),
            ("genre", "Punk"),
        ],
    );
    tone(
        &m.join("e/1.flac"),
        5,
        &[("title", "Nothing Like It"), ("artist", "Nobody")],
    );
    let server = start_server_with(m, &["--https-port", "0", "--musicbrainz-url", &url]);
    let http = server.http;
    let get = |path: &'static str| {
        tokio::task::spawn_blocking(move || {
            let (code, body) = http_json(http, "GET", path, None);
            assert_eq!(code, 200, "{path}: {body}");
            serde_json::from_str::<Value>(&body).unwrap()
        })
    };
    for _ in 0..100 {
        if get("/api/v1/server").await.unwrap()["library"]["tracks"] == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let off = get("/api/v1/settings").await.unwrap();
    assert_eq!(off["musicbrainz"], false);
    tokio::task::spawn_blocking(move || {
        let (code, body) = http_json(
            http,
            "PATCH",
            "/api/v1/settings",
            Some(r#"{"musicbrainz": true}"#),
        );
        assert_eq!(code, 200, "{body}");
    })
    .await
    .unwrap();
    let mut s = Value::Null;
    for _ in 0..200 {
        s = get("/api/v1/settings").await.unwrap();
        if s["musicbrainz_pending"] == 0 && s["musicbrainz_identified"].as_i64() >= Some(3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // (the two Nirvana files are different audio: both identified)
    assert_eq!(s["musicbrainz_identified"], 4, "{s}");

    // genres: filled in where there was none, the tag kept where there was one
    let genres = cli(server.cli, "genres 0 50");
    let names: Vec<String> = genres
        .iter()
        .filter_map(|t| t.strip_prefix("genre:").map(str::to_string))
        .collect();
    for g in [
        "Jazz",
        "Dream Pop",
        "Shoegaze",
        "Grunge",
        "Alternative Rock",
        "Punk",
        "No Genre",
    ] {
        assert!(names.iter().any(|n| n == g), "{g} in {names:?}");
    }
    assert!(
        !names.iter().any(|n| n == "Rock"),
        "too few votes: {names:?}"
    );
    let in_genre = |g: &str| {
        let id = genres
            .windows(2)
            .find(|w| w[1] == format!("genre:{g}"))
            .and_then(|w| w[0].strip_prefix("id:").map(str::to_string))
            .unwrap();
        cli(server.cli, &format!("titles 0 50 genre_id:{id}"))
            .iter()
            .filter_map(|t| t.strip_prefix("title:").map(str::to_string))
            .collect::<Vec<_>>()
    };
    assert_eq!(in_genre("Grunge"), ["Smells Like Teen Spirit"]);
    assert_eq!(in_genre("Punk"), ["Smells Like Teen Spirit"]);
    assert_eq!(in_genre("No Genre"), ["Nothing Like It"]);
}
