//! End-to-end test: run the `rms` binary and drive it with a fake slimproto player.
//!
//! The fake player speaks the wire protocol through the `slimproto` crate, fetches its audio
//! from `/stream.mp3`, reports STAT events like squeezelite does, and the test checks the
//! server's reactions via the CLI and JSON-RPC.

use bytes::{BufMut, Bytes, BytesMut};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAC: [u8; 6] = [0x00, 0x04, 0x20, 0xfe, 0xed, 0x01];
const MAC_S: &str = "00:04:20:fe:ed:01";

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server {
    child: Child,
    http: u16,
    cli: u16,
    slim: u16,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start_server(music: &Path) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let (http, cli, slim) = (free_port(), free_port(), free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_rms"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--music",
            music.to_str().unwrap(),
        ])
        .args(["--bind-addr", "127.0.0.1", "--no-discovery"])
        .args([
            "--http-port",
            &http.to_string(),
            "--cli-port",
            &cli.to_string(),
            "--slimproto-port",
            &slim.to_string(),
        ])
        .args(["--log", "warn"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start rms");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", cli)).is_ok()
            && TcpStream::connect(("127.0.0.1", slim)).is_ok()
        {
            return Server {
                child,
                http,
                cli,
                slim,
                _dir: dir,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("server did not start");
}

/// One CLI round trip; returns the unescaped response tokens.
fn cli(port: u16, line: &str) -> Vec<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(format!("{}\n", line).as_bytes()).unwrap();
    let mut r = BufReader::new(s);
    let mut resp = String::new();
    r.read_line(&mut resp).unwrap();
    resp.trim_end()
        .split(' ')
        .map(|t| {
            percent_encoding::percent_decode_str(t)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect()
}

fn field(tokens: &[String], key: &str) -> Option<String> {
    tokens
        .iter()
        .find_map(|t| t.strip_prefix(&format!("{}:", key)).map(str::to_string))
}

struct FakePlayer {
    sock: TcpStream,
    buf: Vec<u8>,
    jiffies_start: Instant,
}

impl FakePlayer {
    fn connect(port: u16) -> FakePlayer {
        Self::connect_as(port, 12, &MAC, "Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,Firmware=v2.0.0-test,ModelName=SqueezeLite,MaxSampleRate=192000,flc,pcm,mp3")
    }

    fn connect_as(port: u16, device_id: u8, mac: &[u8; 6], caps: &str) -> FakePlayer {
        let sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut p = FakePlayer {
            sock,
            buf: Vec::new(),
            jiffies_start: Instant::now(),
        };
        let mut helo = BytesMut::new();
        helo.put_u8(device_id);
        helo.put_u8(if device_id == 12 { 0 } else { 130 });
        helo.put_slice(mac);
        helo.put_slice(&[0u8; 16]);
        helo.put_u16(0);
        helo.put_u64(0);
        helo.put_slice(&[0, 0]);
        helo.put_slice(caps.as_bytes());
        p.send(b"HELO", &helo);
        p
    }

    fn send(&mut self, op: &[u8; 4], payload: &[u8]) {
        let mut f = Vec::with_capacity(8 + payload.len());
        f.extend_from_slice(op);
        f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        f.extend_from_slice(payload);
        self.sock.write_all(&f).unwrap();
    }

    fn stat(&mut self, event: &[u8; 4], elapsed_ms: u32) {
        let mut s = BytesMut::new();
        s.put_slice(event);
        s.put_u8(0);
        s.put_u8(b'm');
        s.put_u8(0);
        s.put_u32(2 * 1024 * 1024);
        s.put_u32(0);
        s.put_u64(0);
        s.put_u16(0xffff);
        s.put_u32(self.jiffies_start.elapsed().as_millis() as u32);
        s.put_u32(3_528_000);
        s.put_u32(0);
        s.put_u32(elapsed_ms / 1000);
        s.put_u16(0);
        s.put_u32(elapsed_ms);
        s.put_u32(0);
        s.put_u16(0);
        self.send(b"STAT", &s);
    }

    /// Read server frames until one with `op` arrives (returns its payload).
    fn expect(&mut self, op: &[u8; 4], timeout: Duration) -> Bytes {
        let deadline = Instant::now() + timeout;
        loop {
            while self.buf.len() >= 6 {
                let len = u16::from_be_bytes([self.buf[0], self.buf[1]]) as usize;
                if self.buf.len() < 2 + len {
                    break;
                }
                let frame: Vec<u8> = self.buf.drain(..2 + len).collect();
                if &frame[2..6] == op {
                    return Bytes::copy_from_slice(&frame[6..]);
                }
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {}",
                String::from_utf8_lossy(op)
            );
            let mut tmp = [0u8; 4096];
            match self.sock.read(&mut tmp) {
                Ok(0) => panic!("server closed connection"),
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(_) => {}
            }
        }
    }

    /// Wait for a `strm` frame with the given command byte.
    fn expect_strm(&mut self, cmd: u8, timeout: Duration) -> Bytes {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let f = self.expect(b"strm", left);
            if f[0] == cmd {
                return f;
            }
        }
    }
}

/// Fetch the audio the `strm s` frame points at.
fn fetch_stream(http: u16, strm: &[u8]) -> Vec<u8> {
    let request = &strm[24..];
    let port = u16::from_be_bytes([strm[18], strm[19]]);
    assert_eq!(port, http, "strm header: {:?}", &strm[..24]);
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(request).unwrap();
    let mut all = Vec::new();
    s.read_to_end(&mut all).unwrap();
    let split = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http headers")
        + 4;
    let head = String::from_utf8_lossy(&all[..split]).to_string();
    assert!(
        head.starts_with("HTTP/1.") && head.contains(" 200 "),
        "bad stream response: {}",
        head
    );
    all[split..].to_vec()
}

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

/// Minimal HTTP POST returning the body (HTTP/1.0, connection closes).
fn http_post(port: u16, path: &str, body: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "POST {} HTTP/1.0\r\nContent-Type: text/json\r\nContent-Length: {}\r\n\r\n{}",
        path,
        body.len(),
        body
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    resp[resp.find("\r\n\r\n").unwrap() + 4..].to_string()
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

/// Play a fake radio station for 15s, let it drop, and refuse the server's re-stream:
/// the server should now have a retry pending (`_RetryOrNext` -> `_willRetry`).
fn radio_reconnect_refused(server: &Server, p: &mut FakePlayer) {
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

fn http_post_status(port: u16, path: &str, body: &str, auth: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = auth
        .map(|a| format!("Authorization: Basic {}\r\n", a))
        .unwrap_or_default();
    write!(
        s,
        "POST {} HTTP/1.0\r\nContent-Type: text/json\r\n{}Content-Length: {}\r\n\r\n{}",
        path,
        auth,
        body.len(),
        body
    )
    .unwrap();
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
