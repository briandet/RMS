//! What the analyses made of a track, and how far they have got over the library, through the
//! native API: after the background analysis, bliss features, rhythm and the rest are there,
//! with nearest neighbours by sound.

use crate::common::*;
use serde_json::Value;
use std::time::{Duration, Instant};

fn get(port: u16, path: &str) -> (u16, Value) {
    let (code, body) = http_json(port, "GET", path, None);
    (code, serde_json::from_str(&body).unwrap_or_default())
}

#[test]
fn analysis_of_a_track_and_the_library() {
    need_ffmpeg!();
    let music = tempfile::tempdir().unwrap();
    for (i, title) in ["One", "Two", "Three"].iter().enumerate() {
        make_audio(
            &music.path().join(format!("Band/0{} {title}.flac", i + 1)),
            i as u32 + 1,
            &[("title", title), ("artist", "Band"), ("album", "Album")],
        );
    }
    let server = start_server(music.path());

    // wait for the scan and the background analysis
    let deadline = Instant::now() + Duration::from_secs(90);
    let progress = loop {
        let (code, p) = get(server.http, "/api/v1/library/analysis");
        assert_eq!(code, 200, "{p}");
        if p["bliss"]["total"] == 3 && p["bliss"]["pending"] == 0 && p["running"] == false {
            break p;
        }
        assert!(Instant::now() < deadline, "analysis never finished: {p}");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(progress["bliss"]["enabled"], true);
    assert_eq!(progress["bliss"]["done"], 3, "{progress}");
    assert_eq!(progress["rhythm"]["total"], 3);
    assert_eq!(progress["effnet"]["enabled"], false);

    let (_, tracks) = get(server.http, "/api/v1/tracks?sort=title");
    let id = tracks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "One")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let (code, a) = get(server.http, &format!("/api/v1/tracks/{id}/analysis"));
    assert_eq!(code, 200, "{a}");
    assert_eq!(a["track_id"], id);
    assert_eq!(a["title"], "One");
    assert_eq!(a["artist"], "Band");
    assert_eq!(a["file"]["format"], "flc");
    assert!(a["file"]["path"].as_str().unwrap().ends_with("01 One.flac"));
    assert!(a["file"]["fingerprint"].is_string());

    let bliss = &a["bliss"];
    assert_eq!(bliss["status"], "done", "{bliss}");
    assert_eq!(bliss["version"], bliss["current_version"]);
    assert!(bliss["analyzed_at"].as_i64().unwrap() > 0);
    let features = bliss["result"]["features"].as_array().unwrap();
    assert_eq!(features[0]["name"], "tempo");
    assert!(features.len() >= 20);
    assert!(bliss["result"]["loudness"].is_number());
    // the other two tracks, nearest first
    let nearest = bliss["result"]["nearest"].as_array().unwrap();
    assert_eq!(nearest.len(), 2, "{bliss}");
    assert!(nearest[0]["score"].as_f64() <= nearest[1]["score"].as_f64());
    assert_eq!(nearest[0]["artist"], "Band");

    let rhythm = &a["rhythm"];
    assert_eq!(rhythm["status"], "done", "{rhythm}");
    assert!(rhythm["result"]["key"].is_string());
    assert!(rhythm["result"]["energy"].is_array());
    // two seconds of a tone may be too little to fingerprint, but it was tried
    assert!(
        ["done", "failed"].contains(&a["chromaprint"]["status"].as_str().unwrap()),
        "{}",
        a["chromaprint"]
    );
    assert_eq!(a["effnet"]["status"], "off");
    assert!(a["musicbrainz"]["status"].is_string());

    let (code, e) = get(server.http, "/api/v1/tracks/999/analysis");
    assert_eq!(code, 404, "{e}");

    // with the sonic analysis off, the results stay
    cli(server.cli, "pref sonicAnalysis 0");
    let (_, p) = get(server.http, "/api/v1/library/analysis");
    assert_eq!(p["bliss"]["enabled"], false);
    assert_eq!(p["bliss"]["done"], 3);
}
