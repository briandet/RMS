//! End-to-end test: run the `rms` binary and drive it with a fake slimproto player.
//!
//! The fake player speaks the wire protocol through the `slimproto` crate, fetches its audio
//! from `/stream.mp3`, reports STAT events like squeezelite does, and the test checks the
//! server's reactions via the CLI and JSON-RPC.

use crate::common::*;
use bytes::{BufMut, Bytes, BytesMut};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn make_music() -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for i in 0..3 {
        // Tiny but valid-looking payloads: the server streams bytes, it does not decode them.
        let p = dir.path().join(format!("{:02} track.mp3", i + 1));
        let mut data = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
        data.extend(std::iter::repeat_n(i as u8 + 1, 50_000));
        std::fs::write(&p, data).unwrap();
        files.push(p);
    }
    (dir, files)
}

#[test]
fn play_album_with_fake_player() {
    let (music, files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);

    // connect sequence: name query and volume
    p.expect(b"setd", Duration::from_secs(5));
    p.expect(b"audg", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");
    std::thread::sleep(Duration::from_millis(300));
    let players = cli(server.cli, "players 0 10");
    assert_eq!(field(&players, "count").as_deref(), Some("1"));
    assert_eq!(field(&players, "name").as_deref(), Some("Kitchen"));
    assert_eq!(field(&players, "playerid").as_deref(), Some(MAC_S));

    // play the folder: expect strm s for track 1
    let dir = music.path().to_str().unwrap();
    cli(
        server.cli,
        &format!("{} playlist play {}", MAC_S, dir.replace(' ', "%20")),
    );
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    assert_eq!(strm[2], b'm', "mp3 format byte");
    let audio = fetch_stream(server.http, &strm);
    assert_eq!(
        audio,
        std::fs::read(&files[0]).unwrap(),
        "streamed bytes equal the file"
    );

    // player reports: connected, track started; then the server finished sending (DSCO 0)
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.send(b"DSCO", &[0]);
    std::thread::sleep(Duration::from_millis(300));
    let st = cli(server.cli, &format!("{} status - 1 tags:u", MAC_S));
    assert_eq!(field(&st, "mode").as_deref(), Some("play"));
    assert_eq!(field(&st, "playlist_tracks").as_deref(), Some("3"));
    assert_eq!(field(&st, "playlist_cur_index").as_deref(), Some("0"));

    // decoder finished track 1 -> gapless: server streams track 2 while 1 still plays
    p.stat(b"STMd", 3000);
    let strm2 = p.expect_strm(b's', Duration::from_secs(5));
    assert_eq!(
        fetch_stream(server.http, &strm2),
        std::fs::read(&files[1]).unwrap()
    );
    p.stat(b"STMc", 3500);
    let st = cli(server.cli, &format!("{} status - 1", MAC_S));
    assert_eq!(
        field(&st, "playlist_cur_index").as_deref(),
        Some("0"),
        "track 1 still playing"
    );
    p.stat(b"STMs", 0);
    std::thread::sleep(Duration::from_millis(300));
    let st = cli(server.cli, &format!("{} status - 1", MAC_S));
    assert_eq!(
        field(&st, "playlist_cur_index").as_deref(),
        Some("1"),
        "track 2 now playing"
    );

    // pause / resume / volume
    cli(server.cli, &format!("{} pause 1", MAC_S));
    p.expect_strm(b'p', Duration::from_secs(5));
    assert_eq!(
        cli(server.cli, &format!("{} mode ?", MAC_S))
            .last()
            .map(String::as_str),
        Some("pause")
    );
    cli(server.cli, &format!("{} pause 0", MAC_S));
    p.expect_strm(b'u', Duration::from_secs(5));
    cli(server.cli, &format!("{} mixer volume 25", MAC_S));
    let g = p.expect(b"audg", Duration::from_secs(5));
    assert!(g.len() >= 18);
    assert_eq!(
        cli(server.cli, &format!("{} mixer volume ?", MAC_S))
            .last()
            .map(String::as_str),
        Some("25")
    );

    // JSON-RPC sees the same state
    let body = format!(
        r#"{{"id":1,"method":"slim.request","params":["{}",["status","-",1,"tags:a"]]}}"#,
        MAC_S
    );
    let mut s = TcpStream::connect(("127.0.0.1", server.http)).unwrap();
    write!(s, "POST /jsonrpc.js HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp[resp.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(json["result"]["mode"], "play");
    assert_eq!(json["result"]["playlist_cur_index"], 1);

    // stop
    cli(server.cli, &format!("{} stop", MAC_S));
    p.expect_strm(b'q', Duration::from_secs(5));
    assert_eq!(
        cli(server.cli, &format!("{} mode ?", MAC_S))
            .last()
            .map(String::as_str),
        Some("stop")
    );
}

/// A track that cannot be opened while the one before it plays out (a Spotify track that is
/// no longer available, here a deleted file) is skipped: the next one is streamed.
#[test]
fn a_next_track_that_cannot_be_opened_is_skipped() {
    let (music, files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.expect(b"audg", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");

    let dir = music.path().to_str().unwrap();
    cli(
        server.cli,
        &format!("{} playlist play {}", MAC_S, dir.replace(' ', "%20")),
    );
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.send(b"DSCO", &[0]);
    std::thread::sleep(Duration::from_millis(300));

    // track 2 disappears; track 1's decoder finishes. Track 2 fails before anything is sent to
    // the player, and track 3 is streamed while track 1 still plays out (one strm for track 2
    // would fail to fetch here).
    std::fs::remove_file(&files[1]).unwrap();
    p.stat(b"STMd", 3000);
    let strm3 = p.expect_strm(b's', Duration::from_secs(5));
    assert_eq!(
        fetch_stream(server.http, &strm3),
        std::fs::read(&files[2]).unwrap()
    );
    p.stat(b"STMc", 3500);
    p.stat(b"STMs", 0);
    std::thread::sleep(Duration::from_millis(300));
    let st = cli(server.cli, &format!("{} status - 1", MAC_S));
    assert_eq!(field(&st, "playlist_cur_index").as_deref(), Some("2"));
}

/// A next track that opens but sends no audio at all (a Spotify track that librespot cannot
/// load, here an empty file) is skipped too, after the player has been told to stream it.
#[test]
fn a_next_track_without_audio_is_skipped() {
    let (music, files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.expect(b"audg", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");

    let dir = music.path().to_str().unwrap();
    cli(
        server.cli,
        &format!("{} playlist play {}", MAC_S, dir.replace(' ', "%20")),
    );
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.send(b"DSCO", &[0]);
    std::thread::sleep(Duration::from_millis(300));

    // track 2 is streamed while track 1 plays, and turns out empty
    std::fs::write(&files[1], b"").unwrap();
    p.stat(b"STMd", 3000);
    let strm2 = p.expect_strm(b's', Duration::from_secs(5));
    assert!(fetch_stream(server.http, &strm2).is_empty());
    p.stat(b"STMc", 3000);
    p.send(b"DSCO", &[0]);
    p.stat(b"STMd", 3000);

    // track 3 follows while track 1 still plays out
    let strm3 = p.expect_strm(b's', Duration::from_secs(5));
    assert_eq!(
        fetch_stream(server.http, &strm3),
        std::fs::read(&files[2]).unwrap()
    );
    p.stat(b"STMc", 3500);
    p.stat(b"STMs", 0);
    std::thread::sleep(Duration::from_millis(300));
    let st = cli(server.cli, &format!("{} status - 1", MAC_S));
    assert_eq!(field(&st, "playlist_cur_index").as_deref(), Some("2"));
}

#[test]
fn cometd_and_cli_notifications() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"audg", Duration::from_secs(5));

    // CLI listener sees commands issued elsewhere
    let mut listener = TcpStream::connect(("127.0.0.1", server.cli)).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    listener.write_all(b"listen 1\n").unwrap();
    let mut lr = BufReader::new(listener.try_clone().unwrap());
    let mut line = String::new();
    lr.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "listen 1");

    // Bayeux handshake
    let hs: serde_json::Value = serde_json::from_str(&http_post(server.http, "/cometd", r#"[{"channel":"/meta/handshake","version":"1.0","supportedConnectionTypes":["long-polling"]}]"#)).unwrap();
    assert_eq!(hs[0]["successful"], true);
    let clid = hs[0]["clientId"].as_str().unwrap().to_string();
    assert_eq!(clid.len(), 8);

    // /slim/request on a long-polling client: result inline
    let req = format!(
        r#"[{{"channel":"/slim/request","clientId":"{c}","id":3,"data":{{"request":["",["players",0,10]],"response":"/{c}/slim/request/3"}}}}]"#,
        c = clid
    );
    let r: serde_json::Value =
        serde_json::from_str(&http_post(server.http, "/cometd", &req)).unwrap();
    let result = r
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["channel"] == format!("/{}/slim/request/3", clid))
        .expect("result event");
    assert_eq!(result["data"]["count"], 1);
    assert_eq!(result["data"]["players_loop"][0]["playerid"], MAC_S);

    // subscribe to player status, then change the volume and long-poll for the update
    let sub = format!(
        r#"[{{"channel":"/slim/subscribe","clientId":"{c}","id":4,"data":{{"request":["{m}",["status","-",1,"subscribe:0"]],"response":"/{c}/slim/playerstatus/{m}"}}}}]"#,
        c = clid,
        m = MAC_S
    );
    let r: serde_json::Value =
        serde_json::from_str(&http_post(server.http, "/cometd", &sub)).unwrap();
    assert!(r
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["channel"] == "/slim/subscribe" && e["successful"] == true));
    cli(server.cli, &format!("{} mixer volume 33", MAC_S));
    let poll = format!(
        r#"[{{"channel":"/meta/connect","clientId":"{}","connectionType":"long-polling","advice":{{"timeout":5000}}}}]"#,
        clid
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while Instant::now() < deadline && !seen {
        let r: serde_json::Value =
            serde_json::from_str(&http_post(server.http, "/cometd", &poll)).unwrap();
        seen = r
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["data"]["mixer volume"] == 33);
    }
    assert!(seen, "status subscription delivered the new volume");

    // the CLI listener got the mixer notification
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = false;
    while Instant::now() < deadline && !got {
        line.clear();
        if lr.read_line(&mut line).is_ok() {
            got = line.contains("mixer volume 33");
        }
    }
    assert!(got, "listen output contains the mixer command");
}

/// Wait for the next `grfe` frame with an animation (not `c`), answering with `ANIC`.
fn expect_transition(p: &mut FakePlayer) -> u8 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let f = p.expect(b"grfe", left);
        if f[2] != b'c' {
            p.send(b"ANIC", &[]);
            return f[2];
        }
    }
}

#[test]
fn squeezebox_remote_navigates_menus() {
    let (music, _files) = make_music();
    let srv = start_server(music.path());
    let mac = [0x00, 0x04, 0x20, 0x12, 0x34, 0x56];
    let mut p = FakePlayer::connect_as(srv.slim, 4, &mac, "");
    // the home menu is drawn
    p.expect(b"grfe", Duration::from_secs(5));
    let start = Instant::now();
    let ir = |p: &mut FakePlayer, code: u32| {
        let mut f = BytesMut::new();
        f.put_u32(start.elapsed().as_millis() as u32 + 1000);
        f.put_u8(0xff);
        f.put_u8(32);
        f.put_u32(code);
        p.send(b"IR  ", &f);
        std::thread::sleep(Duration::from_millis(300));
    };
    // down: next home item, pushed up from below
    ir(&mut p, 0x7689b04f);
    assert_eq!(expect_transition(&mut p), b'd');
    // right: into My Music
    ir(&mut p, 0x7689d02f);
    assert_eq!(expect_transition(&mut p), b'r');
    // left: back home
    ir(&mut p, 0x7689906f);
    assert_eq!(expect_transition(&mut p), b'l');
    // left at the top of home: bump
    ir(&mut p, 0x7689906f);
    assert_eq!(expect_transition(&mut p), b'L');
    // volume up on the remote changes the volume
    let before = cli(srv.cli, "00:04:20:12:34:56 mixer volume ?")
        .last()
        .cloned();
    ir(&mut p, 0x7689807f);
    std::thread::sleep(Duration::from_millis(300));
    let after = cli(srv.cli, "00:04:20:12:34:56 mixer volume ?");
    assert_ne!(after.last(), before.as_ref(), "volume changed: {after:?}");
}

/// A fake internet radio station: every connection gets an endless-looking mp3 response
/// (no Content-Length) that closes after a few bytes.
fn start_radio() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let _ = s.write_all(
                    b"HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-br: 128\r\nicy-name: Fake FM\r\n\r\n",
                );
                let _ = s.write_all(&[0xff; 4096]);
            });
        }
    });
    port
}

impl FakePlayer {
    /// Like [`expect_strm`], but returns `None` instead of panicking on timeout.
    fn try_expect_strm(&mut self, cmd: u8, timeout: Duration) -> Option<Bytes> {
        let deadline = Instant::now() + timeout;
        loop {
            while self.buf.len() >= 6 {
                let len = u16::from_be_bytes([self.buf[0], self.buf[1]]) as usize;
                if self.buf.len() < 2 + len {
                    break;
                }
                let frame: Vec<u8> = self.buf.drain(..2 + len).collect();
                if &frame[2..6] == b"strm" && frame[6] == cmd {
                    return Some(Bytes::copy_from_slice(&frame[6..]));
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut tmp = [0u8; 4096];
            match self.sock.read(&mut tmp) {
                Ok(0) => panic!("server closed connection"),
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(_) => {}
            }
        }
    }

    fn connect_and_greet(port: u16) -> FakePlayer {
        let mut p = FakePlayer::connect(port);
        p.expect(b"setd", Duration::from_secs(5));
        p.expect(b"audg", Duration::from_secs(5));
        p.send(b"SETD", b"\0Kitchen\0");
        std::thread::sleep(Duration::from_millis(300));
        p
    }
}

/// Players connect to stations themselves (Lyrion's way) rather than through the server.
fn radio_direct(server: &Server) {
    let (code, body) = http_json(
        server.http,
        "PATCH",
        "/api/v1/settings",
        Some(r#"{"radio_direct": true}"#),
    );
    assert_eq!(code, 200, "{body}");
}

/// Play a fake radio station for 15s, let it drop, and refuse the server's re-stream:
/// the server should now have a retry pending (`_RetryOrNext` -> `_willRetry`).  The player
/// fetches the station itself, and it reports the refusal.
fn radio_reconnect_refused(server: &Server, p: &mut FakePlayer) {
    radio_direct(server);
    let radio = start_radio();
    let url = format!("http://127.0.0.1:{}/stream", radio);
    cli(server.cli, &format!("{} playlist play {}", MAC_S, url));
    p.expect_strm(b's', Duration::from_secs(10));

    // stream connects and plays for 15s
    p.stat(b"STMc", 0);
    p.send(
        b"RESP",
        b"HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\nicy-br: 128\r\n\r\n",
    );
    p.stat(b"STMs", 0);
    p.stat(b"STMt", 15_000);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        cli(server.cli, &format!("{} mode ?", MAC_S))
            .last()
            .map(String::as_str),
        Some("play")
    );

    // the station closes the connection; the decoder drains and asks for more
    p.send(b"DSCO", &[0]);
    p.stat(b"STMd", 15_500);

    // server re-streams the station (played > 10s, live) ...
    p.expect_strm(b's', Duration::from_secs(5));
    // ... but this reconnect is refused
    p.stat(b"STMc", 15_600);
    p.send(b"RESP", b"HTTP/1.0 503 Service Unavailable\r\n\r\n");
}

/// When a radio reconnect fails, Lyrion schedules a retry through TRACKWAIT and tries
/// again; a single player still playing out its buffer must get that retry.
#[test]
fn single_player_retries_failed_radio_reconnect() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect_and_greet(server.slim);
    radio_reconnect_refused(&server, &mut p);

    // first retry interval is 5s
    let retry = p.try_expect_strm(b's', Duration::from_secs(10));
    assert!(retry.is_some(), "no retry after a failed radio reconnect");
}

/// The same through the server: the server's own connection to the station is refused on
/// the re-stream, and the retry comes all the same.
#[test]
fn single_player_retries_radio_the_server_cannot_reach() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect_and_greet(server.slim);
    // a station that turns every connection away
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let radio = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { continue };
            let mut req = [0u8; 2048];
            let _ = s.read(&mut req);
            let _ = s.write_all(b"HTTP/1.0 503 Service Unavailable\r\n\r\n");
        }
    });
    let url = format!("http://127.0.0.1:{}/stream.mp3", radio);
    cli(server.cli, &format!("{} playlist play {}", MAC_S, url));
    p.expect_strm(b's', Duration::from_secs(10));
    // playing for 15s from what was buffered, then out of data
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.stat(b"STMt", 15_000);
    std::thread::sleep(Duration::from_millis(300));
    p.send(b"DSCO", &[0]);
    p.stat(b"STMd", 15_500);

    // the re-stream goes through the server, which can't reach the station
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    assert_eq!(u16::from_be_bytes([strm[18], strm[19]]), server.http);
    let mut s = TcpStream::connect(("127.0.0.1", server.http)).unwrap();
    s.write_all(&strm[24..]).unwrap();
    let mut head = Vec::new();
    let _ = s.read_to_end(&mut head);
    assert!(!String::from_utf8_lossy(&head).contains(" 200 "));

    let retry = p.try_expect_strm(b's', Duration::from_secs(10));
    assert!(
        retry.is_some(),
        "no retry after the server could not reach the station"
    );
}

/// The retry timer goes through the state table, so a user `stop` while a retry is
/// pending wins: the station must not come back by itself.
#[test]
fn stop_cancels_pending_radio_retry() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect_and_greet(server.slim);
    radio_reconnect_refused(&server, &mut p);

    // the user gives up on the station while the retry is pending
    cli(server.cli, &format!("{} stop", MAC_S));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        cli(server.cli, &format!("{} mode ?", MAC_S))
            .last()
            .map(String::as_str),
        Some("stop")
    );

    // past the first retry interval (5s): nothing may start streaming again
    let again = p.try_expect_strm(b's', Duration::from_secs(8));
    assert!(
        again.is_none(),
        "radio restarted by itself after the user pressed stop"
    );
}

/// Read a `/stream.mp3` response until it ends or stalls for `stall`; returns the body so
/// far and whether it ended.
fn read_stream(s: &mut TcpStream, buf: &mut Vec<u8>, stall: Duration) -> bool {
    s.set_read_timeout(Some(stall)).unwrap();
    let mut tmp = [0u8; 65536];
    loop {
        match s.read(&mut tmp) {
            Ok(0) => return true,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return false,
        }
    }
}

fn http_body(all: &[u8]) -> &[u8] {
    let split = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert!(all.starts_with(b"HTTP/1.") && all[..split].windows(5).any(|w| w == b" 200 "));
    &all[split..]
}

#[test]
fn synced_players_share_one_transcode() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let music = tempfile::tempdir().unwrap();
    // 40 s of stereo audio: about 7 MB as PCM, more than the shared buffer holds
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
        .args([
            "-i",
            "sine=frequency=440:duration=40",
            "-ac",
            "2",
            "-ar",
            "44100",
        ])
        .arg(music.path().join("tone.flac"))
        .status()
        .unwrap();
    assert!(ok.success());
    let server = start_server(music.path());
    // players without FLAC: the server decodes to PCM with ffmpeg
    let caps = "Model=squeezelite,ModelName=SqueezeLite,MaxSampleRate=192000,pcm,mp3";
    let (mac_a, mac_b) = ([0, 4, 0x20, 0xaa, 0, 1], [0, 4, 0x20, 0xaa, 0, 2]);
    let mut a = FakePlayer::connect_as(server.slim, 12, &mac_a, caps);
    let mut b = FakePlayer::connect_as(server.slim, 12, &mac_b, caps);
    a.expect(b"audg", Duration::from_secs(5));
    b.expect(b"audg", Duration::from_secs(5));
    cli(server.cli, "00:04:20:aa:00:01 sync 00:04:20:aa:00:02");
    let dir = music.path().to_str().unwrap().replace(' ', "%20");
    cli(
        server.cli,
        &format!("00:04:20:aa:00:01 playlist play {dir}"),
    );
    let strm_a = a.expect_strm(b's', Duration::from_secs(10));
    let strm_b = b.expect_strm(b's', Duration::from_secs(10));
    assert_eq!(strm_a[2], b'p', "pcm format byte");

    // the first player is held back while the second has not connected yet
    let mut sa = TcpStream::connect(("127.0.0.1", server.http)).unwrap();
    sa.write_all(&strm_a[24..]).unwrap();
    let mut got_a = Vec::new();
    assert!(!read_stream(&mut sa, &mut got_a, Duration::from_secs(2)));
    assert!(
        (3 << 20..6 << 20).contains(&got_a.len()),
        "stalled after {} bytes",
        got_a.len()
    );

    // ...and both get the same bytes once it has (one ffmpeg feeding both)
    let mut sb = TcpStream::connect(("127.0.0.1", server.http)).unwrap();
    sb.write_all(&strm_b[24..]).unwrap();
    let reader_b = std::thread::spawn(move || {
        let mut got = Vec::new();
        assert!(read_stream(&mut sb, &mut got, Duration::from_secs(10)));
        got
    });
    assert!(read_stream(&mut sa, &mut got_a, Duration::from_secs(10)));
    let got_b = reader_b.join().unwrap();
    let (body_a, body_b) = (http_body(&got_a), http_body(&got_b));
    assert_eq!(body_a.len(), 40 * 44100 * 4);
    assert!(body_a == body_b, "both players got the same audio");
}

#[test]
fn authorize_gates_http() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let body = r#"{"id":1,"method":"slim.request","params":["",["version","?"]]}"#;
    let (code, _) = http_post_status(server.http, "/jsonrpc.js", body, None);
    assert_eq!(code, 200, "open by default");

    cli(server.cli, "pref username alice");
    // sha1_base64("secret"), the form LMS stores
    cli(server.cli, "pref password 5en6G6MezRroT3XKqkdPOmY/BfQ");
    cli(server.cli, "pref authorize 1");

    let (code, _) = http_post_status(server.http, "/jsonrpc.js", body, None);
    assert_eq!(code, 401);
    // alice:wrong
    let (code, _) = http_post_status(server.http, "/jsonrpc.js", body, Some("YWxpY2U6d3Jvbmc="));
    assert_eq!(code, 401);
    // alice:secret
    let (code, resp) = http_post_status(server.http, "/jsonrpc.js", body, Some("YWxpY2U6c2VjcmV0"));
    assert_eq!(code, 200);
    assert!(resp.contains("_version"), "{resp}");
    let (code, _) = http_post_status(server.http, "/cometd", "[]", None);
    assert_eq!(code, 401);
}

/// Track ids by file name, once the startup scan has found all `n` files.
fn scanned_tracks(server: &Server, n: usize) -> Vec<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let r = cli(server.cli, "titles 0 100 tags:u");
        let ids: Vec<String> = r
            .iter()
            .filter_map(|t| t.strip_prefix("id:").map(str::to_string))
            .collect();
        let urls: Vec<String> = r
            .iter()
            .filter_map(|t| t.strip_prefix("url:").map(str::to_string))
            .collect();
        if ids.len() == n && urls.len() == n {
            return ids.into_iter().zip(urls).collect();
        }
        assert!(
            Instant::now() < deadline,
            "scan did not find {n} tracks: {r:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn playlist_count(server: &Server) -> usize {
    field(
        &cli(server.cli, &format!("{MAC_S} playlist tracks ?")),
        "_tracks",
    )
    .or_else(|| {
        cli(server.cli, &format!("{MAC_S} playlist tracks ?"))
            .last()
            .cloned()
    })
    .and_then(|v| v.parse().ok())
    .unwrap_or(0)
}

/// Three short tagged MP3s (ffmpeg), so the library scan finds them.  They sound different
/// (a tone, noise, a pulse) so the mixes' sound-alike check does not take them for duplicates.
fn make_tagged_music() -> Option<(tempfile::TempDir, Vec<PathBuf>)> {
    Command::new("ffmpeg").arg("-version").output().ok()?;
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    let sources = [
        "sine=frequency=300:duration=2",
        "anoisesrc=color=pink:duration=2:amplitude=0.3",
        "aevalsrc='sin(2*PI*60*t)*exp(-8*mod(t,0.5))':duration=2",
    ];
    for (i, (artist, genre)) in [("Ann", "Rock"), ("Ann", "Rock"), ("Bob", "Jazz")]
        .iter()
        .enumerate()
    {
        let p = dir.path().join(format!("{:02} song.mp3", i + 1));
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
            .args(["-i", sources[i]])
            .args(["-metadata", &format!("title=Song {}", i + 1)])
            .args([
                "-metadata",
                &format!("artist={artist}"),
                "-metadata",
                &format!("genre={genre}"),
            ])
            .args(["-metadata", "album=Album", "-metadata", "date=2001"])
            .arg(&p)
            .status()
            .ok()?;
        assert!(ok.success());
        files.push(p);
    }
    Some((dir, files))
}

#[test]
fn dont_stop_the_music_adds_songs_and_create_mix_plays_a_mix() {
    let Some((music, files)) = make_tagged_music() else {
        eprintln!("skipped: no ffmpeg");
        return;
    };
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"audg", Duration::from_secs(5));
    let tracks = scanned_tracks(&server, 3);

    // the setting, as Squeezer and the Touch show it
    cli(
        server.cli,
        &format!("{MAC_S} playerpref plugin.dontstopthemusic:provider PLUGIN_RANDOM_TRACK"),
    );
    assert_eq!(
        cli(
            server.cli,
            &format!("{MAC_S} playerpref plugin.dontstopthemusic:provider ?")
        )
        .last()
        .map(String::as_str),
        Some("PLUGIN_RANDOM_TRACK")
    );
    let menu = cli(
        server.cli,
        &format!("{MAC_S} dontstopthemusicsetting 0 100"),
    );
    let joined = menu.join(" ");
    assert!(joined.contains("text:Song Mix radio:1"), "{joined}");
    assert!(joined.contains("text:Disabled radio:0"), "{joined}");
    assert!(joined.contains("text:Similar Songs radio:0"), "{joined}");

    // play only the last song: when it starts, DSTM adds the other two (all there is)
    let last = files[2].to_str().unwrap().replace(' ', "%20");
    cli(server.cli, &format!("{MAC_S} playlist play {last}"));
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while playlist_count(&server) < 3 {
        assert!(Instant::now() < deadline, "DSTM added nothing");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(playlist_count(&server), 3, "no duplicates were added");

    // Jive: the player settings entry, and Create Mix in a track's menu
    let home = cli(server.cli, &format!("{MAC_S} menu 0 500 direct:1")).join(" ");
    assert!(home.contains("id:settingsDontStopTheMusic"), "{home}");
    let info = cli(
        server.cli,
        &format!(
            "{MAC_S} trackinfo items 0 100 track_id:{} menu:track",
            tracks[0].0
        ),
    )
    .join(" ");
    assert!(info.contains("text:Create Mix"), "{info}");

    // Create Mix from a track: the track first, then the others
    let (seed, seed_url) = &tracks[1];
    let r = cli(
        server.cli,
        &format!("{MAC_S} createmix play track_id:{seed}"),
    );
    assert_eq!(field(&r, "count").as_deref(), Some("3"), "{r:?}");
    let first = cli(server.cli, &format!("{MAC_S} playlist path 0 ?"));
    assert_eq!(first.last(), Some(seed_url), "{first:?}");
}

/// Reads `/api/v1/events` on a thread and hands over `(event, data)` pairs.
struct Events {
    rx: std::sync::mpsc::Receiver<(String, serde_json::Value)>,
}

impl Events {
    fn open(port: u16) -> Events {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        // HTTP/1.0: the body is the raw stream, no chunked encoding
        write!(
            s,
            "GET /api/v1/events HTTP/1.0\r\nAccept: text/event-stream\r\n\r\n"
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(s);
            let mut line = String::new();
            // headers
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let (mut event, mut data) = (String::new(), String::new());
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let l = line.trim_end_matches(['\r', '\n']);
                if let Some(v) = l.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = l.strip_prefix("data:") {
                    data.push_str(v.trim_start());
                } else if l.is_empty() && !data.is_empty() {
                    let json = serde_json::from_str(&data).expect("event data is JSON");
                    if tx.send((std::mem::take(&mut event), json)).is_err() {
                        return;
                    }
                    data.clear();
                }
            }
        });
        Events { rx }
    }

    fn next(&self, timeout: Duration) -> (String, serde_json::Value) {
        self.rx.recv_timeout(timeout).expect("event")
    }

    /// Wait for a `player` event matching `pred`; returns the player.
    fn player_where(&self, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (name, data) = self.next(left);
            if name == "player" && pred(&data["player"]) {
                return data["player"].clone();
            }
        }
    }
}

#[test]
fn native_api_pushes_player_state() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let events = Events::open(server.http);
    let (name, snap) = events.next(Duration::from_secs(5));
    assert_eq!(name, "snapshot");
    assert_eq!(snap["type"], "snapshot");
    assert!(snap["server"]["version"].is_string());
    assert_eq!(snap["players"], serde_json::json!([]));

    // a player connects and names itself: pushed without asking
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");
    let pl = events.player_where(|p| p["name"] == "Kitchen");
    assert_eq!(pl["id"], MAC_S);
    assert_eq!(pl["connected"], true);
    assert_eq!(pl["mode"], "stop");
    assert_eq!(pl["queue"]["length"], 0);

    // play a folder through the command endpoint
    let cmd = |body: serde_json::Value| {
        http_json(
            server.http,
            "POST",
            &format!("/api/v1/players/{MAC_S}/command"),
            Some(&body.to_string()),
        )
    };
    let dir = music.path().to_str().unwrap();
    let (code, body) = cmd(serde_json::json!({
        "command": "enqueue", "item": {"type": "url", "url": dir}, "mode": "play"
    }));
    assert_eq!(code, 200, "{body}");
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    let pl = events.player_where(|p| {
        p["mode"] == "play" && p["buffering"] == false && p["queue"]["length"] == 3
    });
    assert_eq!(pl["power"], true);
    assert_eq!(pl["queue"]["index"], 0);
    assert_eq!(pl["now_playing"]["title"], "01 track");

    // pause and volume arrive as events too
    assert_eq!(cmd(serde_json::json!({"command": "pause"})).0, 200);
    p.expect_strm(b'p', Duration::from_secs(5));
    events.player_where(|p| p["mode"] == "pause");
    assert_eq!(
        cmd(serde_json::json!({"command": "volume", "value": 25})).0,
        200
    );
    p.expect(b"audg", Duration::from_secs(5));
    events.player_where(|p| p["volume"] == 25);

    // reordering the queue changes its version
    let before = pl["queue"]["version"].as_f64().unwrap();
    assert_eq!(
        cmd(serde_json::json!({"command": "move", "from": 2, "to": 1})).0,
        200
    );
    let pl = events.player_where(|p| p["queue"]["version"].as_f64() != Some(before));
    assert_eq!(pl["queue"]["length"], 3);

    // the queue itself is a resource
    let (code, body) = http_json(
        server.http,
        "GET",
        &format!("/api/v1/players/{MAC_S}/queue"),
        None,
    );
    assert_eq!(code, 200);
    let q: serde_json::Value = serde_json::from_str(&body).unwrap();
    let titles: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["01 track", "03 track", "02 track"]);
    let (_, body) = http_json(
        server.http,
        "GET",
        &format!("/api/v1/players/{MAC_S}/queue?offset=1&limit=1"),
        None,
    );
    let q: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(q["offset"], 1);
    assert_eq!(q["items"].as_array().unwrap().len(), 1);

    // errors
    assert_eq!(
        cmd(serde_json::json!({"command": "jump", "index": 9})).0,
        400
    );
    assert_eq!(cmd(serde_json::json!({"command": "dance"})).0, 400);
    let (code, _) = http_json(
        server.http,
        "GET",
        "/api/v1/players/00:00:00:00:00:99",
        None,
    );
    assert_eq!(code, 404);
    let (code, body) = http_json(server.http, "GET", "/api/v1/players", None);
    assert_eq!(code, 200);
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list[0]["name"], "Kitchen");

    // the player going away is pushed too
    drop(p);
    events.player_where(|p| p["connected"] == false);
}

/// A second of 8 kHz mono silence as a WAV file.
fn make_wav() -> Vec<u8> {
    let data = vec![0u8; 16_000];
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&8000u32.to_le_bytes());
    w.extend_from_slice(&16_000u32.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    w
}

/// POST raw bytes with a content type; (status, body).
fn http_post_bytes(port: u16, path: &str, content_type: &str, body: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST {} HTTP/1.0\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
        path,
        content_type,
        body.len()
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
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

/// Play the clip the server just queued to its end: started, sent, decoded, played out.
fn play_out(server: &Server, p: &mut FakePlayer) {
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    assert!(
        !fetch_stream(server.http, &strm).is_empty(),
        "the clip streams"
    );
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.send(b"DSCO", &[0]);
    p.stat(b"STMd", 1000);
    std::thread::sleep(Duration::from_millis(200));
    p.stat(b"STMu", 1000);
}

#[test]
fn announcement_interrupts_and_resumes() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.expect(b"audg", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");
    std::thread::sleep(Duration::from_millis(300));
    let api = format!("/api/v1/players/{}", MAC_S.replace(':', "%3A"));

    // the album plays, track 2 at 2.5 s, volume 25, repeat all
    let dir = music.path().to_str().unwrap();
    cli(
        server.cli,
        &format!("{} playlist play {}", MAC_S, dir.replace(' ', "%20")),
    );
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    cli(server.cli, &format!("{} playlist index 1", MAC_S));
    let strm = p.expect_strm(b's', Duration::from_secs(5));
    fetch_stream(server.http, &strm);
    p.stat(b"STMc", 0);
    p.stat(b"STMs", 0);
    p.stat(b"STMt", 2500);
    cli(server.cli, &format!("{} mixer volume 25", MAC_S));
    cli(server.cli, &format!("{} playlist repeat 2", MAC_S));
    std::thread::sleep(Duration::from_millis(300));

    // not audio, and too loud: refused before anything changes
    let (code, _) = http_post_bytes(server.http, &format!("{api}/announce"), "image/png", b"x");
    assert_eq!(code, 400);
    let (code, _) = http_post_bytes(
        server.http,
        &format!("{api}/announce?volume=101"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 400);

    // the announcement at volume 60
    let (code, body) = http_post_bytes(
        server.http,
        &format!("{api}/announce?volume=60"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 202, "{body}");
    let st = cli(server.cli, &format!("{} status - 1 tags:u", MAC_S));
    assert_eq!(
        field(&st, "playlist_tracks").as_deref(),
        Some("1"),
        "{st:?}"
    );
    assert_eq!(field(&st, "mixer volume").as_deref(), Some("60"));
    // one at a time per group
    let (code, _) = http_post_bytes(
        server.http,
        &format!("{api}/announce"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 422);
    play_out(&server, &mut p);

    // back: the album, track 2 from about where it was, volume and repeat as they were
    // (resumed at a position, so transcoded for the seek)
    p.expect_strm(b's', Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(5);
    let st = loop {
        let st = cli(server.cli, &format!("{} status - 1 tags:u", MAC_S));
        if field(&st, "playlist_tracks").as_deref() == Some("3") || Instant::now() > deadline {
            break st;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        field(&st, "playlist_tracks").as_deref(),
        Some("3"),
        "{st:?}"
    );
    assert_eq!(field(&st, "playlist_cur_index").as_deref(), Some("1"));
    assert_eq!(field(&st, "mode").as_deref(), Some("play"));
    assert_eq!(field(&st, "mixer volume").as_deref(), Some("25"));
    assert_eq!(field(&st, "playlist repeat").as_deref(), Some("2"));
    let at: f64 = field(&st, "time")
        .and_then(|t| t.parse().ok())
        .unwrap_or(0.0);
    assert!((2.0..4.0).contains(&at), "resumed near 2.5 s, not {at}");
    // the clip is gone from the data dir (right after the restore)
    let left = || {
        std::fs::read_dir(server._dir.path().join("announce"))
            .map(|d| d.count())
            .unwrap_or(0)
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while left() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(left(), 0, "clip file removed");

    // an announcement interrupted by someone choosing other music: nothing restored
    let (code, _) = http_post_bytes(
        server.http,
        &format!("{api}/announce"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 202);
    p.expect_strm(b's', Duration::from_secs(5));
    cli(
        server.cli,
        &format!("{} playlist play {}", MAC_S, dir.replace(' ', "%20")),
    );
    std::thread::sleep(Duration::from_millis(800));
    let st = cli(server.cli, &format!("{} status - 1 tags:u", MAC_S));
    assert_eq!(field(&st, "playlist_tracks").as_deref(), Some("3"));
    assert_eq!(
        field(&st, "playlist_cur_index").as_deref(),
        Some("0"),
        "their choice stands"
    );
    // and the group is free for the next one
    let (code, _) = http_post_bytes(
        server.http,
        &format!("{api}/announce"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 202);
}

#[test]
fn announcement_on_a_player_that_was_off() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect(server.slim);
    p.expect(b"setd", Duration::from_secs(5));
    p.send(b"SETD", b"\0Kitchen\0");
    cli(server.cli, &format!("{} power 0", MAC_S));
    std::thread::sleep(Duration::from_millis(300));
    let api = format!("/api/v1/players/{}", MAC_S.replace(':', "%3A"));
    let (code, body) = http_post_bytes(
        server.http,
        &format!("{api}/announce"),
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 202, "{body}");
    let st = cli(server.cli, &format!("{} status - 1", MAC_S));
    assert_eq!(field(&st, "power").as_deref(), Some("1"), "on for the clip");
    play_out(&server, &mut p);
    let deadline = Instant::now() + Duration::from_secs(5);
    let st = loop {
        let st = cli(server.cli, &format!("{} status - 1", MAC_S));
        if field(&st, "power").as_deref() == Some("0") || Instant::now() > deadline {
            break st;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        field(&st, "power").as_deref(),
        Some("0"),
        "off again: {st:?}"
    );
    assert_eq!(field(&st, "playlist_tracks").as_deref(), Some("0"));
    let (code, _) = http_post_bytes(
        server.http,
        "/api/v1/players/nobody/announce",
        "audio/wav",
        &make_wav(),
    );
    assert_eq!(code, 404);
}

#[test]
fn the_last_player_left_in_a_sync_group_is_no_longer_grouped() {
    let (music, _files) = make_music();
    let server = start_server(music.path());
    let caps = "Model=squeezelite,ModelName=SqueezeLite,MaxSampleRate=192000,flc,pcm,mp3";
    let macs = [
        "00:04:20:aa:00:01",
        "00:04:20:aa:00:02",
        "00:04:20:aa:00:03",
    ];
    let mut players: Vec<FakePlayer> = (1..=3)
        .map(|i| FakePlayer::connect_as(server.slim, 12, &[0, 4, 0x20, 0xaa, 0, i], caps))
        .collect();
    for p in &mut players {
        p.expect(b"audg", Duration::from_secs(5));
    }
    let group = |mac: &str| -> i64 {
        cli(server.cli, &format!("{mac} playerpref syncgroupid ?"))
            .last()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    // `<player> sync <buddy>`: the player joins the buddy's group
    cli(server.cli, &format!("{} sync {}", macs[1], macs[0]));
    cli(server.cli, &format!("{} sync {}", macs[2], macs[0]));
    let gid = group(macs[0]);
    assert!(gid > 0);
    assert!(macs.iter().all(|m| group(m) == gid));

    // one leaves: the other two stay a group
    cli(server.cli, &format!("{} sync -", macs[2]));
    assert_eq!(group(macs[2]), 0);
    assert_eq!((group(macs[0]), group(macs[1])), (gid, gid));

    // another leaves: the one left is on its own, not a group of one
    cli(server.cli, &format!("{} sync -", macs[1]));
    assert_eq!(macs.map(group), [0, 0, 0]);
    let groups = cli(server.cli, "syncgroups ?");
    assert!(
        !groups.iter().any(|t| t.starts_with("sync_members")),
        "{groups:?}"
    );
}
