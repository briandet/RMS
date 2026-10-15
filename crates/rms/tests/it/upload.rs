//! Adding music files through the native API: only with file changes on and a login; the
//! file lands in `Uploads/` of the music folder, is scanned, and comes back as a track; a
//! taken name gets a number; what isn't music is refused and removed again.

use crate::common::*;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const LOGIN: &str = "me:secret";

/// POST raw bytes (signed in when `login` is given): (status, body).
fn post_file(port: u16, login: Option<&str>, path: &str, body: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = login
        .map(|l| {
            use base64::Engine;
            format!(
                "Authorization: Basic {}\r\n",
                base64::engine::general_purpose::STANDARD.encode(l)
            )
        })
        .unwrap_or_default();
    write!(
        s,
        "POST {path} HTTP/1.0\r\n{auth}Content-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).unwrap();
    let resp = String::from_utf8_lossy(&resp).into_owned();
    let status = resp
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (
        status,
        resp[resp.find("\r\n\r\n").unwrap() + 4..].to_string(),
    )
}

fn tracks_in_library(port: u16, login: Option<&str>) -> u64 {
    let (_, body) = http_json_as(port, login, "GET", "/api/v1/server", None);
    let v: Value = serde_json::from_str(&body).unwrap_or_default();
    v["library"]["tracks"].as_u64().unwrap_or(0)
}

#[test]
fn uploads_become_tracks() {
    need_ffmpeg!();
    let music = tempfile::tempdir().unwrap();
    make_audio(
        &music.path().join("Old/01 Old.flac"),
        1,
        &[("title", "Old"), ("artist", "Someone")],
    );
    let src = tempfile::tempdir().unwrap();
    let flac = src.path().join("new.flac");
    make_audio(
        &flac,
        2,
        &[
            ("title", "Fresh Song"),
            ("artist", "New Band"),
            ("album", "Debut"),
        ],
    );
    let flac = std::fs::read(&flac).unwrap();
    let server = start_server(music.path());
    let deadline = Instant::now() + Duration::from_secs(20);
    while tracks_in_library(server.http, None) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }

    // file changes off: refused, and nothing written
    let (code, body) = post_file(server.http, None, "/api/v1/tracks?name=a.flac", &flac);
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("edit_files"), "{body}");
    assert!(!music.path().join("Uploads").exists());

    // on, but without a login: still refused
    let (code, _) = http_json(
        server.http,
        "PATCH",
        "/api/v1/settings",
        Some(r#"{"edit_files": true}"#),
    );
    assert_eq!(code, 200);
    let (code, body) = post_file(server.http, None, "/api/v1/tracks?name=a.flac", &flac);
    assert_eq!(code, 403);
    assert!(body.contains("login"), "{body}");
    cli(server.cli, "pref username me");
    cli(server.cli, "pref password secret");
    cli(server.cli, "pref authorize 1");
    let (code, _) = post_file(server.http, None, "/api/v1/tracks?name=a.flac", &flac);
    assert_eq!(code, 401, "the API itself needs the login now");

    // into a folder of the upload folder; a path in the name is only a name
    let (code, body) = post_file(
        server.http,
        Some(LOGIN),
        "/api/v1/tracks?name=..%2F..%2FFresh%20Song.flac&folder=New%20Band%2FDebut",
        &flac,
    );
    assert_eq!(code, 201, "{body}");
    let tracks: Value = serde_json::from_str(&body).unwrap();
    let t = &tracks[0];
    assert_eq!(tracks.as_array().unwrap().len(), 1);
    assert_eq!(t["title"], "Fresh Song");
    assert_eq!(t["artist"], "New Band");
    assert_eq!(t["album"], "Debut");
    assert!(t["id"].as_i64().unwrap() > 0);
    assert_eq!(t["detail"]["play_count"], 0, "comes with its detail: {t}");
    let path = music.path().join("Uploads/New Band/Debut/Fresh Song.flac");
    assert_eq!(std::fs::read(&path).unwrap(), flac);
    assert_eq!(tracks_in_library(server.http, Some(LOGIN)), 2);
    // and it is a library track like any other
    let id = t["id"].as_i64().unwrap();
    let (code, body) = http_json_as(
        server.http,
        Some(LOGIN),
        "GET",
        &format!("/api/v1/tracks/{id}"),
        None,
    );
    assert_eq!(code, 200, "{body}");

    // the same name again: numbered, never overwritten
    let (code, body) = post_file(
        server.http,
        Some(LOGIN),
        "/api/v1/tracks?name=Fresh%20Song.flac&folder=New%20Band%2FDebut",
        &flac,
    );
    assert_eq!(code, 201, "{body}");
    assert!(music
        .path()
        .join("Uploads/New Band/Debut/Fresh Song (2).flac")
        .exists());
    assert_eq!(tracks_in_library(server.http, Some(LOGIN)), 3);

    // not music: refused and removed again
    let (code, body) = post_file(
        server.http,
        Some(LOGIN),
        "/api/v1/tracks?name=junk.flac",
        b"this is not a flac file at all",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("could not be read"), "{body}");
    assert!(!music.path().join("Uploads/junk.flac").exists());
    assert_eq!(tracks_in_library(server.http, Some(LOGIN)), 3);

    // names, folders and empty bodies that can't be
    for (path, why) in [
        ("/api/v1/tracks?name=cover.jpg", "not an audio file type"),
        (
            "/api/v1/tracks?name=x.flac&folder=..%2Fescape",
            "inside Uploads",
        ),
        ("/api/v1/tracks?name=x.flac&folder=%2Fetc", "inside Uploads"),
        ("/api/v1/tracks", "name is missing"),
    ] {
        let (code, body) = post_file(server.http, Some(LOGIN), path, &flac);
        assert_eq!(code, 400, "{path}: {body}");
        assert!(body.contains(why), "{path}: {body}");
    }
    let (code, _) = post_file(server.http, Some(LOGIN), "/api/v1/tracks?name=x.flac", b"");
    assert_eq!(code, 400);
    assert!(!music.path().join("escape").exists());
}
