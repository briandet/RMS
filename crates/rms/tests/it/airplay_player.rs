//! An AirPlay speaker as a player: a fake RAOP receiver (RTSP, RTP audio in ALAC frames,
//! the control and timing ports) given to the server in `airplayHosts`.

use crate::common::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// the fake receiver
// ---------------------------------------------------------------------------------------------

/// What the receiver got.
#[derive(Default)]
struct Received {
    sdp: String,
    records: usize,
    volumes: Vec<f64>,
    flushes: usize,
    teardowns: usize,
    /// Audio packets kept (not flushed before they were heard): RTP time and samples.
    packets: Vec<(u32, Vec<i16>)>,
    /// The first packet, as it came, to ask for again.
    first_packet: Option<(u16, Vec<u8>)>,
    asked_resend: bool,
    /// Packets with the marker bit: the stream (re)starts.
    starts: usize,
    resend_ok: bool,
    timing_ok: bool,
    syncs: usize,
    /// The last sync: when it came, and the frame heard then.
    last_sync: Option<(Instant, u32)>,
}

impl Received {
    /// Frames heard since the first packet's, by the sync packets.
    fn heard_of_first(&self) -> i64 {
        let (Some((at, frame)), Some((_, first))) = (self.last_sync, &self.first_packet) else {
            return 0;
        };
        let first = u32::from_be_bytes([first[4], first[5], first[6], first[7]]);
        let heard = frame.wrapping_add((at.elapsed().as_secs_f64() * 44100.0) as u32);
        i64::from(heard.wrapping_sub(first) as i32)
    }
}

type Shared = Arc<Mutex<Received>>;

/// An RAOP receiver on 127.0.0.1 without encryption; returns its RTSP port.
fn fake_receiver(rx: Shared) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { return };
            let rx = rx.clone();
            std::thread::spawn(move || serve_rtsp(conn, rx));
        }
    });
    port
}

fn serve_rtsp(conn: TcpStream, rx: Shared) {
    let mut out = conn.try_clone().unwrap();
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let method = line.split(' ').next().unwrap_or("").to_string();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let header = |k: &str| {
            headers
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let mut body = vec![0u8; header("content-length").parse().unwrap_or(0)];
        r.read_exact(&mut body).unwrap();
        let body = String::from_utf8_lossy(&body).to_string();
        assert!(!header("user-agent").is_empty() && !header("dacp-id").is_empty());

        let mut extra = String::new();
        match method.as_str() {
            "OPTIONS" => extra.push_str(
                "Public: ANNOUNCE, SETUP, RECORD, FLUSH, TEARDOWN, OPTIONS, SET_PARAMETER\r\n",
            ),
            "ANNOUNCE" => rx.lock().unwrap().sdp = body,
            "SETUP" => {
                let transport = header("transport");
                let port = |name: &str| -> u16 {
                    transport
                        .split(';')
                        .find_map(|p| p.strip_prefix(&format!("{name}=")))
                        .unwrap()
                        .parse()
                        .unwrap()
                };
                let client = |p: u16| SocketAddr::from(([127, 0, 0, 1], p));
                let (control_to, timing_to) =
                    (client(port("control_port")), client(port("timing_port")));
                let audio = UdpSocket::bind("127.0.0.1:0").unwrap();
                let control = UdpSocket::bind("127.0.0.1:0").unwrap();
                let timing = UdpSocket::bind("127.0.0.1:0").unwrap();
                extra.push_str(&format!(
                    "Transport: RTP/AVP/UDP;unicast;mode=record;server_port={};control_port={};timing_port={}\r\nSession: DEADBEEF\r\n",
                    audio.local_addr().unwrap().port(),
                    control.local_addr().unwrap().port(),
                    timing.local_addr().unwrap().port()
                ));
                let control2 = control.try_clone().unwrap();
                let rx2 = rx.clone();
                std::thread::spawn(move || receive_audio(audio, control2, control_to, rx2));
                let rx2 = rx.clone();
                std::thread::spawn(move || receive_control(control, rx2));
                let rx2 = rx.clone();
                std::thread::spawn(move || ask_time(timing, timing_to, rx2));
            }
            "RECORD" => {
                assert!(header("rtp-info").contains("rtptime="));
                rx.lock().unwrap().records += 1;
                extra.push_str("Audio-Latency: 11025\r\n");
            }
            "SET_PARAMETER" => {
                if let Some(v) = body.trim().strip_prefix("volume:") {
                    rx.lock().unwrap().volumes.push(v.trim().parse().unwrap());
                }
            }
            "FLUSH" => {
                // what was not heard yet is dropped: the sender sends it again on resume
                let mut g = rx.lock().unwrap();
                g.flushes += 1;
                if let Some((at, frame)) = g.last_sync {
                    let heard = frame.wrapping_add((at.elapsed().as_secs_f64() * 44100.0) as u32);
                    g.packets
                        .retain(|(rtp, _)| (rtp.wrapping_add(352).wrapping_sub(heard) as i32) <= 0);
                }
            }
            "TEARDOWN" => rx.lock().unwrap().teardowns += 1,
            _ => {}
        }
        let cseq = header("cseq");
        write!(out, "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n{extra}\r\n").unwrap();
    }
}

/// Audio packets: RTP with payload type 96, each an ALAC frame.
fn receive_audio(sock: UdpSocket, control: UdpSocket, control_to: SocketAddr, rx: Shared) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = sock.recv(&mut buf) {
        let p = &buf[..n];
        assert_eq!(p[0], 0x80);
        assert_eq!(p[1] & 0x7f, 96);
        let seq = u16::from_be_bytes([p[2], p[3]]);
        let rtp = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
        let samples = decode_alac(&p[12..]);
        assert_eq!(samples.len(), 352 * 2);
        let mut g = rx.lock().unwrap();
        g.starts += usize::from(p[1] & 0x80 != 0);
        g.packets.push((rtp, samples));
        if g.first_packet.is_none() {
            g.first_packet = Some((seq, p.to_vec()));
        }
        // once, ask for the first packet again
        if g.packets.len() == 50 && !g.asked_resend {
            g.asked_resend = true;
            let seq = g.first_packet.as_ref().unwrap().0.to_be_bytes();
            let req = [0x80, 0xd5, 0, 1, seq[0], seq[1], 0, 1];
            control.send_to(&req, control_to).unwrap();
        }
    }
}

/// Sync packets, and packets sent again.
fn receive_control(sock: UdpSocket, rx: Shared) {
    let mut buf = [0u8; 4096];
    while let Ok(n) = sock.recv(&mut buf) {
        let p = &buf[..n];
        let mut g = rx.lock().unwrap();
        match p[1] & 0x7f {
            0x54 => {
                assert_eq!(n, 20);
                let heard = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
                let newest = u32::from_be_bytes([p[16], p[17], p[18], p[19]]);
                assert_eq!(newest.wrapping_sub(heard), 88_200, "two seconds of latency");
                g.syncs += 1;
                g.last_sync = Some((Instant::now(), heard));
            }
            0x56 => {
                let (seq, first) = g.first_packet.clone().unwrap();
                assert_eq!(u16::from_be_bytes([p[2], p[3]]), seq);
                assert_eq!(&p[4..], &first[..]);
                g.resend_ok = true;
            }
            other => panic!("unexpected control packet {other:#x}"),
        }
    }
}

/// Ask the sender for the time once, as receivers do to sync their clocks.
fn ask_time(sock: UdpSocket, to: SocketAddr, rx: Shared) {
    let mut req = [0u8; 32];
    req[..4].copy_from_slice(&[0x80, 0xd2, 0, 7]);
    req[24..32].copy_from_slice(&12345u64.to_be_bytes());
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.send_to(&req, to).unwrap();
    let mut buf = [0u8; 64];
    if let Ok(n) = sock.recv(&mut buf) {
        let ok = n == 32 && buf[1] == 0xd3 && buf[8..16] == 12345u64.to_be_bytes();
        rx.lock().unwrap().timing_ok = ok;
    }
}

/// An ALAC frame of 16-bit stereo as senders write it uncompressed: the channel-pair
/// element header with the "not compressed" flag, the samples verbatim, the end tag.
fn decode_alac(frame: &[u8]) -> Vec<i16> {
    let mut pos = 0usize;
    let mut bits = |n: usize| -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let bit = (frame[pos / 8] >> (7 - pos % 8)) & 1;
            v = (v << 1) | u32::from(bit);
            pos += 1;
        }
        v
    };
    assert_eq!(bits(3), 1, "channel pair element");
    bits(4); // element instance
    bits(12); // unused
    let has_size = bits(1) == 1;
    assert_eq!(bits(2), 0, "no extra bits");
    assert_eq!(bits(1), 1, "uncompressed");
    let frames = if has_size { bits(32) as usize } else { 352 };
    let samples: Vec<i16> = (0..frames * 2).map(|_| bits(16) as u16 as i16).collect();
    assert_eq!(bits(3), 7, "end of frame");
    samples
}

// ---------------------------------------------------------------------------------------------
// the server
// ---------------------------------------------------------------------------------------------

async fn get(http: u16, path: &str) -> Value {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        let (code, body) = http_json(http, "GET", &path, None);
        assert_eq!(code, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap()
    })
    .await
    .unwrap()
}

async fn command(http: u16, player: &str, body: Value) -> Value {
    let path = format!("/api/v1/players/{}/command", player.replace(':', "%3A"));
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "POST", &path, Some(&body.to_string()));
        assert_eq!(code, 200, "{body}: {text}");
        serde_json::from_str(&text).unwrap()
    })
    .await
    .unwrap()
}

async fn pref(http: u16, name: &str, value: &str) {
    let body = json!({"id": 1, "method": "slim.request", "params": ["", ["pref", name, value]]})
        .to_string();
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "POST", "/jsonrpc.js", Some(&body));
        assert_eq!(code, 200, "{text}");
    })
    .await
    .unwrap()
}

/// Wait until the player's state satisfies `ok`.
async fn wait_player(
    http: u16,
    id: &str,
    what: &str,
    secs: u64,
    ok: impl Fn(&Value) -> bool,
) -> Value {
    let path = format!("/api/v1/players/{}", id.replace(':', "%3A"));
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let p = get(http, &path).await;
        if ok(&p) {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what}: {}", get(http, &path).await);
}

/// Wait until what the receiver got satisfies `ok`.
async fn wait_rx(rx: &Shared, what: &str, secs: u64, ok: impl Fn(&Received) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if ok(&rx.lock().unwrap()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the receiver never got {what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_airplay_speaker_plays_the_queue() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    for (i, t) in ["One", "Two"].iter().enumerate() {
        make_audio(
            &m.join(format!("A/Album/0{} {t}.flac", i + 1)),
            i as u32 + 1,
            &[
                ("title", t),
                ("artist", "A"),
                ("album", "Album"),
                ("track", &(i + 1).to_string()),
            ],
        );
    }
    let rx: Shared = Arc::default();
    let rtsp_port = fake_receiver(rx.clone());
    let server = start_server(m);
    let http = server.http;
    for _ in 0..100 {
        if get(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // the speaker, given by hand, shows up as a player
    pref(
        http,
        "airplayHosts",
        &format!("[\"127.0.0.1:{rtsp_port}\"]"),
    )
    .await;
    pref(http, "bridgeAirplay", "1").await;
    let deadline = Instant::now() + Duration::from_secs(10);
    let me = loop {
        let players = get(http, "/api/v1/players").await;
        if let Some(p) = players
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["model"] == "airplay")
        {
            break p.clone();
        }
        assert!(Instant::now() < deadline, "no AirPlay player: {players}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let id = me["id"].as_str().unwrap().to_string();
    assert_eq!(me["name"], "AirPlay 127.0.0.1");

    // play the album: a session in ALAC, then the first track
    let r = command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    assert_eq!(r["count"], 2);
    wait_rx(&rx, "a third of a second of audio", 10, |g| {
        g.heard_of_first() > 44_100 / 3
    })
    .await;
    let p = wait_player(http, &id, "playing", 5, |p| p["mode"] == "play").await;
    assert_eq!(p["now_playing"]["title"], "One");
    {
        let g = rx.lock().unwrap();
        assert!(g.sdp.contains("a=rtpmap:96 AppleLossless"), "{}", g.sdp);
        assert!(
            g.sdp
                .contains("a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100"),
            "{}",
            g.sdp
        );
        assert!(!g.sdp.contains("rsaaeskey"), "no encryption asked for");
        assert_eq!(g.records, 1);
        assert!(!g.packets.is_empty());
    }

    // pause flushes the speaker
    let flushes = rx.lock().unwrap().flushes;
    command(http, &id, json!({"command": "pause"})).await;
    wait_rx(&rx, "a FLUSH", 5, |g| g.flushes > flushes).await;
    wait_player(http, &id, "paused", 5, |p| p["mode"] == "pause").await;

    // the volume goes to the speaker in dB
    command(http, &id, json!({"command": "volume", "value": 33})).await;
    wait_rx(&rx, "the volume", 5, |g| {
        g.volumes.last().is_some_and(|v| (v - -20.1).abs() < 0.01)
    })
    .await;

    // it plays on, and the second track follows
    command(http, &id, json!({"command": "play"})).await;
    wait_player(http, &id, "playing again", 5, |p| p["mode"] == "play").await;
    let p = wait_player(http, &id, "second track", 15, |p| p["queue"]["index"] == 1).await;
    assert_eq!(p["now_playing"]["title"], "Two");

    // the queue ends: the player stops
    wait_player(http, &id, "stopped", 15, |p| p["mode"] == "stop").await;
    // (the last frames are heard just after the player reports the end)
    tokio::time::sleep(Duration::from_millis(800)).await;
    {
        let g = rx.lock().unwrap();
        // both tracks (2 s each), each frame once (what the pause flushed came again);
        // padding ends each track's last packet
        let frames: usize = g.packets.iter().map(|(_, s)| s.len() / 2).sum();
        eprintln!("{frames} frames; the stream started {} times", g.starts);
        let expected = 2 * 88_200;
        assert!(
            frames.abs_diff(expected) <= 3 * 352,
            "{frames} frames, expected about {expected}"
        );
        let energy: f64 = g
            .packets
            .iter()
            .flat_map(|(_, s)| s.iter())
            .map(|&s| f64::from(s).powi(2))
            .sum();
        let rms = (energy / (frames * 2) as f64).sqrt();
        assert!(rms > 1000.0, "the tones are heard (rms {rms})");
        assert_eq!(g.records, 1, "one session for both tracks");
        assert!(g.syncs > 2, "sync packets");
        assert!(g.timing_ok, "the timing port answers");
        assert!(g.resend_ok, "lost packets are sent again");
    }

    // turning AirPlay off disconnects the player, and the session ends
    pref(http, "bridgeAirplay", "0").await;
    wait_player(http, &id, "disconnected", 10, |p| p["connected"] == false).await;
    wait_rx(&rx, "a TEARDOWN", 5, |g| g.teardowns > 0).await;
}
