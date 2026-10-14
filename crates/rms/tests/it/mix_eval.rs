//! `rms-mix-eval` against a stand-in ListenBrainz: seeds are chosen by what ListenBrainz knows
//! and the library holds, picks are judged by ListenBrainz's similar artists, and the chance
//! rate, the summaries and the cache come out right.

use crate::common::*;
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const A: &str = "aaaaaaaa-0000-0000-0000-00000000000a";
const B: &str = "bbbbbbbb-0000-0000-0000-00000000000b";
const C: &str = "cccccccc-0000-0000-0000-00000000000c";

async fn mock(asked: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .route(
            "/similar-artists/json",
            get(|State(n): State<Arc<AtomicUsize>>, Query(q): Query<HashMap<String, String>>| async move {
                n.fetch_add(1, Ordering::SeqCst);
                let similar = match q["artist_mbids"].as_str() {
                    A => json!([{"artist_mbid": B, "name": "Artist B", "score": 900}]),
                    B => json!([{"artist_mbid": A, "name": "Artist A", "score": 900}]),
                    // found by name: Artist D's files have no MusicBrainz id
                    C => json!([{"artist_mbid": "eeeeeeee-0000-0000-0000-00000000000e", "name": "Artist D", "score": 100}]),
                    _ => json!([]),
                };
                Json(similar)
            }),
        )
        .with_state(asked);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

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

fn eval(data: &std::path::Path, url: &str, extra: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rms-mix-eval"))
        .args([
            "--data-dir",
            data.to_str().unwrap(),
            "--listenbrainz-url",
            url,
        ])
        .args(["--size", "3", "--repeats", "2", "--seeds", "10"])
        .args(extra)
        .output()
        .unwrap();
    let (o, e) = (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    );
    assert!(out.status.success(), "{o}\n{e}");
    (o, e)
}

#[tokio::test(flavor = "multi_thread")]
async fn mixes_are_judged_against_listenbrainz() {
    need_ffmpeg!();
    let asked = Arc::new(AtomicUsize::new(0));
    let url = mock(asked.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    // A (Rock): the seed.  B (Jazz): similar to A by ListenBrainz, nothing in common in the
    // tags.  C (Rock): shares A's genre, not similar.  D (Pop): similar to C, by name only.
    let mut hz = 200;
    let mut add = |artist: &str, mbid: Option<&str>, genre: &str, album: &str, n: usize| {
        for i in 0..n {
            hz += 37;
            let title = format!("{artist} {album} {i}");
            let mut meta = vec![
                ("title", title.as_str()),
                ("artist", artist),
                ("album", album),
                ("genre", genre),
            ];
            if let Some(m) = mbid {
                meta.push(("MUSICBRAINZ_ARTISTID", m));
            }
            tone(
                &m.join(artist).join(album).join(format!("{i}.flac")),
                hz,
                &meta,
            );
        }
    };
    add("Artist A", Some(A), "Rock", "A1", 2);
    add("Artist B", Some(B), "Jazz", "B1", 3);
    for album in ["C1", "C2", "C3"] {
        add("Artist C", Some(C), "Rock", album, 2);
    }
    add("Artist D", None, "Pop", "D1", 5);
    let server = start_server_with(m, &["--https-port", "0"]);
    let data = server._dir.path().to_path_buf();
    let http = server.http;
    for _ in 0..200 {
        let (_, b) =
            tokio::task::spawn_blocking(move || http_json(http, "GET", "/api/v1/server", None))
                .await
                .unwrap();
        if serde_json::from_str::<Value>(&b).unwrap()["library"]["tracks"] == 16 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let report = data.join("report.json");
    let (d2, u2, r2) = (data.clone(), url.clone(), report.clone());
    let (out, err) = tokio::task::spawn_blocking(move || {
        eval(&d2, &u2, &["--json", r2.to_str().unwrap(), "--details"])
    })
    .await
    .unwrap();
    // A and C are seeds; B has too few similar songs here (2 by A), D no MusicBrainz id
    assert!(err.contains("seeds: 2 (of 3 artists tried: 0 unknown to ListenBrainz, 1 with fewer than 3 similar songs here)"), "{err}");
    assert!(out.contains("hit rate"), "{out}");
    let r: Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap()).unwrap();
    let seeds = r["seeds"].as_array().unwrap();
    let seed = |artist: &str| seeds.iter().find(|s| s["artist"] == artist).unwrap();
    // A: 3 of the 14 songs by other artists are B's
    assert_eq!(seed("Artist A")["similar_tracks"], 3);
    assert_eq!(seed("Artist A")["others"], 14);
    // C: D's 5 songs, found by name
    assert_eq!(seed("Artist C")["similar_tracks"], 5);
    // by tags alone, A's mix takes C (the same genre), not B: no hits
    for mix in seed("Artist A")["mixes"]["tags-only"].as_array().unwrap() {
        assert_eq!(mix["hits"], 0, "{mix}");
        for p in mix["picks"].as_array().unwrap() {
            assert_ne!(p["artist"], "Artist B", "{mix}");
        }
    }
    let summaries = r["summaries"].as_array().unwrap();
    let names: Vec<&str> = summaries
        .iter()
        .map(|s| s["variant"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "current",
            "no-sound",
            "no-style",
            "sound-only",
            "style-only",
            "tags-only",
            "random"
        ]
    );
    for s in summaries {
        assert_eq!(s["mixes"], 4, "2 seeds, 2 repeats: {s}");
        let rate = s["hit_rate"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&rate), "{s}");
    }

    // the answers were cached: offline, the same seeds without asking again
    let n = asked.load(Ordering::SeqCst);
    assert_eq!(n, 3);
    let (d2, u2) = (data.clone(), url.clone());
    let (_, err) = tokio::task::spawn_blocking(move || eval(&d2, &u2, &["--offline"]))
        .await
        .unwrap();
    assert!(err.contains("seeds: 2"), "{err}");
    assert_eq!(asked.load(Ordering::SeqCst), n);
}
