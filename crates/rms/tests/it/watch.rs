//! The library follows the music folder by itself (`autorescan`): music copied in, files
//! changed, folders renamed and removed show up without a rescan.

use crate::common::*;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn get(server: &Server, path: &str) -> Value {
    let (code, body) = http_json(server.http, "GET", path, None);
    assert_eq!(code, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

fn tracks(server: &Server) -> Vec<Value> {
    get(server, "/api/v1/tracks?limit=100")["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Wait until the library's tracks satisfy `ok` (and no scan runs).
fn wait_for(server: &Server, what: &str, ok: impl Fn(&[Value]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let scanning = get(server, "/api/v1/server").get("scan").is_some();
        let t = tracks(server);
        if !scanning && ok(&t) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: tracks are {t:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn titles(t: &[Value]) -> Vec<String> {
    let mut v: Vec<String> = t
        .iter()
        .map(|t| t["title"].as_str().unwrap_or("").to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn the_library_follows_the_music_folder() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    let tags = |title: &'static str, album: &'static str| {
        vec![("title", title), ("artist", "Artist"), ("album", album)]
    };
    make_audio(&m.join("A/First/01 One.flac"), 1, &tags("One", "First"));
    let server = start_server(m);
    wait_for(&server, "initial scan", |t| titles(t) == ["One"]);

    // an album copied in
    make_audio(&m.join("B/Second/01 Two.mp3"), 2, &tags("Two", "Second"));
    make_audio(
        &m.join("B/Second/02 Three.mp3"),
        3,
        &tags("Three", "Second"),
    );
    wait_for(&server, "new album", |t| {
        titles(t) == ["One", "Three", "Two"]
    });

    // a file retagged
    make_audio(&m.join("A/First/01 One.flac"), 4, &tags("Uno", "First"));
    wait_for(&server, "changed tags", |t| {
        titles(t) == ["Three", "Two", "Uno"]
    });

    // a folder renamed
    std::fs::rename(m.join("B"), m.join("Bee")).unwrap();
    wait_for(&server, "renamed folder", |t| {
        titles(t) == ["Three", "Two", "Uno"]
            && t.iter()
                .filter(|t| t["album"] == "Second")
                .all(|t| t["url"].as_str().unwrap().contains("/Bee/Second/"))
    });

    // a file and then a folder removed
    std::fs::remove_file(m.join("Bee/Second/02 Three.mp3")).unwrap();
    wait_for(&server, "removed file", |t| titles(t) == ["Two", "Uno"]);
    std::fs::remove_dir_all(m.join("Bee")).unwrap();
    wait_for(&server, "removed folder", |t| titles(t) == ["Uno"]);

    // with autorescan off, nothing happens by itself
    let body =
        json!({"id": 1, "method": "slim.request", "params": ["", ["pref", "autorescan", "0"]]});
    let (code, _) = http_json(server.http, "POST", "/jsonrpc.js", Some(&body.to_string()));
    assert_eq!(code, 200);
    make_audio(&m.join("C/Third/01 Four.flac"), 5, &tags("Four", "Third"));
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(titles(&tracks(&server)), ["Uno"]);
}
