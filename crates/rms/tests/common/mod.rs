//! Shared harness for the end-to-end tests: start the `rms` binary on free ports, talk to
//! it as a slimproto player, over the CLI and over HTTP.
#![allow(dead_code)]

use bytes::{BufMut, Bytes, BytesMut};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const MAC: [u8; 6] = [0x00, 0x04, 0x20, 0xfe, 0xed, 0x01];
pub const MAC_S: &str = "00:04:20:fe:ed:01";

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub struct Server {
    pub child: Child,
    pub http: u16,
    pub cli: u16,
    pub slim: u16,
    pub _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn start_server(music: &Path) -> Server {
    start_server_with(music, &["--https-port", "0"])
}

/// A server started with more arguments.
pub fn start_server_with(music: &Path, args: &[&str]) -> Server {
    // A port from `free_port` can be taken by another test's server before this one binds
    // it; then rms exits and we try again on other ports.
    for _ in 0..5 {
        if let Some(server) = try_start_server(music, args) {
            return server;
        }
    }
    panic!("server did not start");
}

fn try_start_server(music: &Path, args: &[&str]) -> Option<Server> {
    let dir = tempfile::tempdir().unwrap();
    let (http, cli, slim) = (free_port(), free_port(), free_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_rms"))
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
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start rms");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return None;
        }
        if TcpStream::connect(("127.0.0.1", http)).is_ok()
            && TcpStream::connect(("127.0.0.1", cli)).is_ok()
            && TcpStream::connect(("127.0.0.1", slim)).is_ok()
        {
            return Some(Server {
                child,
                http,
                cli,
                slim,
                _dir: dir,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// One CLI round trip; returns the unescaped response tokens.
pub fn cli(port: u16, line: &str) -> Vec<String> {
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

pub fn field(tokens: &[String], key: &str) -> Option<String> {
    tokens
        .iter()
        .find_map(|t| t.strip_prefix(&format!("{}:", key)).map(str::to_string))
}

pub struct FakePlayer {
    pub sock: TcpStream,
    pub buf: Vec<u8>,
    pub jiffies_start: Instant,
}

impl FakePlayer {
    pub fn connect(port: u16) -> FakePlayer {
        Self::connect_as(port, 12, &MAC, "Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,Firmware=v2.0.0-test,ModelName=SqueezeLite,MaxSampleRate=192000,flc,pcm,mp3")
    }

    pub fn connect_as(port: u16, device_id: u8, mac: &[u8; 6], caps: &str) -> FakePlayer {
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

    pub fn send(&mut self, op: &[u8; 4], payload: &[u8]) {
        let mut f = Vec::with_capacity(8 + payload.len());
        f.extend_from_slice(op);
        f.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        f.extend_from_slice(payload);
        self.sock.write_all(&f).unwrap();
    }

    pub fn stat(&mut self, event: &[u8; 4], elapsed_ms: u32) {
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
    pub fn expect(&mut self, op: &[u8; 4], timeout: Duration) -> Bytes {
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
    pub fn expect_strm(&mut self, cmd: u8, timeout: Duration) -> Bytes {
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
pub fn fetch_stream(http: u16, strm: &[u8]) -> Vec<u8> {
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

/// Minimal HTTP POST returning the body (HTTP/1.0, connection closes).
pub fn http_post(port: u16, path: &str, body: &str) -> String {
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

pub fn http_post_status(port: u16, path: &str, body: &str, auth: Option<&str>) -> (u16, String) {
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

/// HTTP/1.0 request with a JSON body; returns status and body.
pub fn http_json(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = body.unwrap_or("");
    write!(
        s,
        "{} {} HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        method,
        path,
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
    let body = resp[resp.find("\r\n\r\n").unwrap() + 4..].to_string();
    (status, body)
}

/// A GET returning (status, header block, body bytes).
pub fn http_bytes(port: u16, path: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "GET {} HTTP/1.0\r\n\r\n", path).unwrap();
    let mut resp = Vec::new();
    s.read_to_end(&mut resp).unwrap();
    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("end of headers");
    let head = String::from_utf8_lossy(&resp[..split]).to_string();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, head, resp[split + 4..].to_vec())
}

// ---------------------------------------------------------------------------------------------
// audio files
// ---------------------------------------------------------------------------------------------

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[allow(unused_macros)]
macro_rules! need_ffmpeg {
    () => {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available - skipping");
            return;
        }
    };
}

/// A short tagged audio file.  `tone` picks the sound, so no two tracks sound alike.
pub fn make_audio(path: &Path, tone: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ext = path.extension().unwrap().to_string_lossy().to_string();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
    ]);
    cmd.arg(format!("sine=frequency={}:duration=2", 200 + 110 * tone));
    // deterministic output: no encoder version or creation time in the file
    cmd.args(["-fflags", "+bitexact", "-flags:a", "+bitexact"]);
    match ext.as_str() {
        "flac" => cmd.args(["-c:a", "flac"]),
        "mp3" => cmd.args(["-c:a", "libmp3lame", "-b:a", "128k", "-id3v2_version", "3"]),
        _ => &mut cmd,
    };
    for (k, v) in meta {
        cmd.arg("-metadata").arg(format!("{}={}", k, v));
    }
    cmd.arg(path);
    let out = cmd.output().expect("ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    // a fixed time per file: "added" order and cover ids (made from mtimes) stay the same
    let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000 + 86_400 * tone as u64);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(mtime))
        .unwrap();
}
