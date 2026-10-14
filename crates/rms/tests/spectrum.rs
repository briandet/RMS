//! End-to-end: spectrum bars (`GET /api/v1/players/{id}/spectrum`) for a library track
//! (its stored curve) and for internet radio, which plays through the server and is
//! analysed as it streams.

#[macro_use]
mod common;

use base64::Engine;
use common::*;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// `secs` of 80 Hz, then `secs` of 5 kHz: bass, then treble.
fn bass_then_treble(path: &Path, secs: u32, codec: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=80:duration={secs}"),
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=5000:duration={secs}"),
        ])
        .args(["-filter_complex", "[0][1]concat=n=2:v=0:a=1", "-ac", "2"])
        .args(codec)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The player's spectrum, asked from `from` seconds.
fn spectrum(server: &Server, from: f64) -> Value {
    let (code, body) = http_json(
        server.http,
        "GET",
        &format!("/api/v1/players/{MAC_S}/spectrum?from={from}"),
        None,
    );
    assert_eq!(code, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// Each reading's four levels.
fn readings(s: &Value) -> Vec<[u8; 4]> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(s["curve"].as_str().unwrap())
        .unwrap();
    bytes
        .chunks_exact(2)
        .map(|b| [b[0] >> 4, b[0] & 15, b[1] >> 4, b[1] & 15])
        .collect()
}

fn connect(server: &Server) -> FakePlayer {
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.expect(b"audg", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");
    std::thread::sleep(Duration::from_millis(300));
    p
}

/// Play `item` on the fake player and take its stream; returns the `strm s` frame.
fn play(server: &Server, p: &mut FakePlayer, item: &str) -> bytes::Bytes {
    cli(server.cli, &format!("{MAC_S} playlist play {item}"));
    let strm = p.expect_strm(b's', Duration::from_secs(10));
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    strm
}

#[test]
fn a_library_track_has_its_whole_curve() {
    need_ffmpeg!();
    let music = tempfile::tempdir().unwrap();
    let file = music.path().join("tones.flac");
    bass_then_treble(&file, 3, &["-c:a", "flac"]);
    let server = start_server(music.path());
    let deadline = Instant::now() + Duration::from_secs(20);
    while field(&cli(server.cli, "titles 0 10"), "id").is_none() {
        assert!(Instant::now() < deadline, "not scanned");
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut p = connect(&server);
    play(&server, &mut p, &file.to_string_lossy().replace(' ', "%20"));

    // worked out now if the background analysis hasn't got to it yet
    let s = spectrum(&server, 0.0);
    assert_eq!(s["kind"], "track", "{s}");
    assert!(s["key"].as_str().unwrap().starts_with("track:"));
    assert_eq!(s["fps"], 20);
    assert_eq!(s["start"], 0.0);
    let r = readings(&s);
    assert!(
        (118..=122).contains(&r.len()),
        "6 s at 20 a second: {}",
        r.len()
    );
    // bass first, treble after
    let (bass, treble) = (r[30], r[90]);
    assert!(bass[0] > 10 && bass[3] < 5, "{bass:?}");
    assert!(treble[3] > 10 && treble[0] < 5, "{treble:?}");
    // the same curve again (stored)
    assert_eq!(spectrum(&server, 3.0)["curve"], s["curve"]);
}

/// A fake station sending `rounds` copies of an MP3, then closing.
fn start_station(mp3: Vec<u8>, rounds: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { continue };
            let mp3 = mp3.clone();
            std::thread::spawn(move || {
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let _ = s.write_all(
                    b"HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-br: 128\r\nicy-name: Tone FM\r\n\r\n",
                );
                for _ in 0..rounds {
                    if s.write_all(&mp3).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[test]
fn radio_plays_through_the_server_and_is_analysed_as_it_streams() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let mp3 = dir.path().join("tones.mp3");
    bass_then_treble(&mp3, 2, &["-c:a", "libmp3lame", "-b:a", "128k"]);
    let station = start_station(std::fs::read(&mp3).unwrap(), 10);
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let mut p = connect(&server);
    let strm = play(
        &server,
        &mut p,
        &format!("http://127.0.0.1:{station}/live.mp3"),
    );

    // the player is sent to the server, not the station (fetch_stream checks the port)
    let audio = fetch_stream(server.http, &strm);
    assert!(audio.len() > 9 * std::fs::metadata(&mp3).unwrap().len() as usize);

    // the readings arrive as ffmpeg decodes the copy
    let deadline = Instant::now() + Duration::from_secs(20);
    let s = loop {
        let s = spectrum(&server, 0.0);
        if s["kind"] == "live" && readings(&s).len() >= 600 {
            break s;
        }
        assert!(Instant::now() < deadline, "no live readings: {s}");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(s["start"], 0.0);
    let r = readings(&s);
    // every 4 s: 2 s of bass, 2 s of treble
    for round in 1..7 {
        let (bass, treble) = (r[round * 80 + 20], r[round * 80 + 60]);
        assert!(bass[0] > bass[3], "round {round}: {bass:?}");
        assert!(treble[3] > treble[0], "round {round}: {treble:?}");
    }
    // asked from later: from a second before
    let later = spectrum(&server, 10.0);
    assert_eq!(later["start"], 9.0);
    // the same readings (more may have come since)
    let n = (r.len() - 180).min(readings(&later).len());
    assert_eq!(readings(&later)[..n], r[180..180 + n]);
}

#[test]
fn players_can_still_fetch_radio_from_the_station() {
    let station = start_station(vec![0xff; 4096], 1);
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let (code, body) = http_json(
        server.http,
        "PATCH",
        "/api/v1/settings",
        Some(r#"{"radio_direct": true}"#),
    );
    assert_eq!(code, 200, "{body}");
    let mut p = connect(&server);
    let strm = play(
        &server,
        &mut p,
        &format!("http://127.0.0.1:{station}/live.mp3"),
    );
    // strm s: the station's address and port, its own request
    assert_eq!(u16::from_be_bytes([strm[18], strm[19]]), station);
    assert_eq!(&strm[20..24], &[127, 0, 0, 1]);
    assert!(String::from_utf8_lossy(&strm[24..]).starts_with("GET /live.mp3 "));
    assert_eq!(spectrum(&server, 0.0)["kind"], "none");
}
