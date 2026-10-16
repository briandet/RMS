//! A Chromecast as a player: a fake Cast device (TLS, Cast v2 framing, the Default Media
//! Receiver's messages) given to the server in `chromecastHosts`.  The test moves the
//! device's playback along itself, so nothing depends on timing.

use crate::common::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::rustls;

// ---- the Cast wire format (as in src/bridge/chromecast.rs) ---------------------------

#[derive(Debug, Default)]
struct CastMessage {
    source: String,
    destination: String,
    namespace: String,
    payload: String,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_string(out: &mut Vec<u8>, field: u64, s: &str) {
    put_varint(out, field << 3 | 2);
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_varint(b: &mut &[u8]) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = b[0];
        *b = &b[1..];
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

fn encode(m: &CastMessage) -> Vec<u8> {
    let mut pb = Vec::new();
    put_varint(&mut pb, 1 << 3);
    put_varint(&mut pb, 0);
    put_string(&mut pb, 2, &m.source);
    put_string(&mut pb, 3, &m.destination);
    put_string(&mut pb, 4, &m.namespace);
    put_varint(&mut pb, 5 << 3);
    put_varint(&mut pb, 0);
    put_string(&mut pb, 6, &m.payload);
    let mut frame = (pb.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&pb);
    frame
}

fn decode(mut b: &[u8]) -> CastMessage {
    let mut m = CastMessage::default();
    while !b.is_empty() {
        let key = get_varint(&mut b);
        match key & 7 {
            0 => {
                get_varint(&mut b);
            }
            2 => {
                let len = get_varint(&mut b) as usize;
                let s = String::from_utf8(b[..len].to_vec()).unwrap();
                b = &b[len..];
                match key >> 3 {
                    2 => m.source = s,
                    3 => m.destination = s,
                    4 => m.namespace = s,
                    6 => m.payload = s,
                    f => panic!("field {f}"),
                }
            }
            w => panic!("wire type {w}"),
        }
    }
    m
}

// ---- the fake device -----------------------------------------------------------------

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const TRANSPORT: &str = "web-7";
/// A receiver app the fake device won't start (an unpublished app, this device not added).
const REFUSED_APP: &str = "DEADBEEF";
/// The tracks' length (see `make_audio`).
const LENGTH: f64 = 2.0;

/// What the test makes the device do.
enum Ctl {
    /// Play to this position.
    At(f64),
    /// The current track plays to its end.
    Finish,
}

struct Fake {
    port: u16,
    /// Every message the device got (`ns`, `to` and the payload's fields), with
    /// `{"fetched": <first bytes>}` after it fetched a LOAD's URL and `{"closed": true}`
    /// when the connection ends.
    log: mpsc::UnboundedReceiver<Value>,
    ctl: mpsc::UnboundedSender<Ctl>,
}

impl Fake {
    async fn start() -> Fake {
        Fake::start_on("127.0.0.1").await
    }

    /// A device at `ip` (devices are told apart by address).
    async fn start_on(ip: &str) -> Fake {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cert = rcgen::generate_simple_self_signed(vec!["chromecast.local".into()]).unwrap();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind((ip, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (log_tx, log) = mpsc::unbounded_channel();
        let (ctl, mut ctl_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            // one connection at a time, like the server should make
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                let Ok(tls) = acceptor.accept(sock).await else {
                    continue;
                };
                serve(tls, &log_tx, &mut ctl_rx).await;
                let _ = log_tx.send(json!({"closed": true}));
            }
        });
        Fake { port, log, ctl }
    }

    /// The next message satisfying `ok` (earlier ones are dropped).
    async fn expect(&mut self, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
        self.expect_within(Duration::from_secs(10), what, ok).await
    }

    async fn expect_within(
        &mut self,
        within: Duration,
        what: &str,
        ok: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let v = tokio::time::timeout_at(deadline, self.log.recv())
                .await
                .unwrap_or_else(|_| panic!("the device got no {what}"))
                .unwrap();
            if ok(&v) {
                return v;
            }
        }
    }

    async fn expect_type(&mut self, ns: &str, kind: &str) -> Value {
        self.expect(kind, |v| v["ns"] == ns && v["type"] == kind)
            .await
    }
}

/// The media as the device plays it.
struct Playing {
    session: i64,
    content_id: String,
    state: &'static str,
    idle_reason: Option<&'static str>,
    time: f64,
}

async fn serve(
    mut tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    log: &mpsc::UnboundedSender<Value>,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
) {
    let mut launched: Option<String> = None;
    let mut volume = json!({"level": 1.0, "muted": false});
    let mut media: Option<Playing> = None;
    let mut sessions = 0;
    let mut buf = Vec::new();
    loop {
        // complete frames first
        if buf.len() >= 4 {
            let len = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
            if buf.len() >= 4 + len {
                let m = decode(&buf[4..4 + len]);
                buf.drain(..4 + len);
                let mut p: Value = serde_json::from_str(&m.payload).unwrap();
                assert_eq!(m.source, "sender-0");
                p["ns"] = json!(m.namespace);
                p["to"] = json!(m.destination);
                let _ = log.send(p.clone());
                let request = p["requestId"].clone();
                let reply = |ns: &str, payload: Value| CastMessage {
                    source: m.destination.clone(),
                    destination: m.source.clone(),
                    namespace: ns.to_string(),
                    payload: payload.to_string(),
                };
                let mut out = Vec::new();
                match (m.namespace.as_str(), p["type"].as_str().unwrap()) {
                    (_, "PING") => out.push(reply(&m.namespace, json!({"type": "PONG"}))),
                    (NS_RECEIVER, "LAUNCH") if p["appId"] == REFUSED_APP => {
                        out.push(reply(
                            NS_RECEIVER,
                            json!({"type": "LAUNCH_ERROR", "requestId": request,
                                   "reason": "NOT_FOUND"}),
                        ));
                    }
                    (NS_RECEIVER, kind @ ("GET_STATUS" | "LAUNCH" | "SET_VOLUME")) => {
                        if kind == "LAUNCH" {
                            // (the app that ran gives way to it)
                            launched = Some(p["appId"].as_str().unwrap().to_string());
                            media = None;
                        }
                        if kind == "SET_VOLUME" {
                            for k in ["level", "muted"] {
                                if !p["volume"][k].is_null() {
                                    volume[k] = p["volume"][k].clone();
                                }
                            }
                        }
                        let apps = match &launched {
                            Some(app) => json!([{"appId": app, "displayName": "Receiver",
                                    "sessionId": "s-1", "transportId": TRANSPORT}]),
                            None => json!([]),
                        };
                        out.push(reply(
                            NS_RECEIVER,
                            json!({"type": "RECEIVER_STATUS", "requestId": request,
                                   "status": {"applications": apps, "volume": volume}}),
                        ));
                    }
                    (NS_MEDIA, kind) => {
                        assert_eq!(m.destination, TRANSPORT, "{kind} to the app");
                        match kind {
                            "LOAD" => {
                                // the device fetches the stream itself, as it plays
                                let url = p["media"]["contentId"].as_str().unwrap().to_string();
                                let fetch_log = log.clone();
                                let fetch_url = url.clone();
                                tokio::spawn(async move {
                                    let body = reqwest::get(&fetch_url)
                                        .await
                                        .unwrap()
                                        .bytes()
                                        .await
                                        .unwrap();
                                    let head = String::from_utf8_lossy(&body[..body.len().min(4)]);
                                    let boxes =
                                        String::from_utf8_lossy(&body[..body.len().min(64)])
                                            .contains("ftyp");
                                    // kept for the test to look at
                                    let path = std::env::temp_dir().join(format!(
                                        "fake-cast-{}-{}.bin",
                                        std::process::id(),
                                        rand_suffix()
                                    ));
                                    std::fs::write(&path, &body).unwrap();
                                    let _ = fetch_log.send(json!({"fetched": head,
                                        "bytes": body.len(), "mp4": boxes,
                                        "path": path.display().to_string()}));
                                });
                                sessions += 1;
                                let autoplay = p["autoplay"] != false;
                                media = Some(Playing {
                                    session: sessions,
                                    content_id: url,
                                    state: if autoplay { "PLAYING" } else { "PAUSED" },
                                    idle_reason: None,
                                    time: 0.0,
                                });
                            }
                            "PAUSE" | "PLAY" | "STOP" => {
                                let me = media.as_mut().unwrap();
                                assert_eq!(p["mediaSessionId"], me.session);
                                me.state = match kind {
                                    "PAUSE" => "PAUSED",
                                    "PLAY" => "PLAYING",
                                    _ => "IDLE",
                                };
                                if kind == "STOP" {
                                    me.idle_reason = Some("CANCELLED");
                                }
                            }
                            _ => {} // GET_STATUS
                        }
                        out.push(reply(NS_MEDIA, media_status(&media, request)));
                    }
                    _ => {} // CONNECT
                }
                for o in out {
                    if tls.write_all(&encode(&o)).await.is_err() {
                        return;
                    }
                }
                continue;
            }
        }
        let mut chunk = [0u8; 4096];
        tokio::select! {
            n = tls.read(&mut chunk) => match n {
                Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => return,
            },
            c = ctl.recv() => {
                let Some(me) = media.as_mut() else { continue };
                match c.unwrap() {
                    Ctl::At(t) => me.time = t,
                    Ctl::Finish => {
                        me.time = LENGTH;
                        me.state = "IDLE";
                        me.idle_reason = Some("FINISHED");
                    }
                }
                // an unsolicited status, to everyone
                let m = CastMessage {
                    source: TRANSPORT.into(),
                    destination: "*".into(),
                    namespace: NS_MEDIA.into(),
                    payload: media_status(&media, json!(0)).to_string(),
                };
                if tls.write_all(&encode(&m)).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn media_status(media: &Option<Playing>, request: Value) -> Value {
    let status: Vec<Value> = media
        .iter()
        .map(|m| {
            let mut s = json!({
                "mediaSessionId": m.session,
                "playerState": m.state,
                "currentTime": m.time,
                "media": {"contentId": m.content_id, "duration": LENGTH},
            });
            if let Some(r) = m.idle_reason {
                s["idleReason"] = json!(r);
            }
            s
        })
        .collect();
    json!({"type": "MEDIA_STATUS", "requestId": request, "status": status})
}

// ---- the server ----------------------------------------------------------------------

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
    let body = json!({"id": 1, "method": "slim.request", "params": ["", ["pref", name, value]]});
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "POST", "/jsonrpc.js", Some(&body.to_string()));
        assert_eq!(code, 200, "{text}");
    })
    .await
    .unwrap()
}

/// A player's pref.
async fn player_pref(http: u16, player: &str, name: &str, value: &str) {
    let body = json!({"id": 1, "method": "slim.request",
                      "params": [player, ["playerpref", name, value]]});
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "POST", "/jsonrpc.js", Some(&body.to_string()));
        assert_eq!(code, 200, "{text}");
    })
    .await
    .unwrap()
}

/// Wait until the player's state satisfies `ok`.
async fn wait_player(http: u16, id: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let path = format!("/api/v1/players/{}", id.replace(':', "%3A"));
    for _ in 0..100 {
        let p = get(http, &path).await;
        if ok(&p) {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what}: {}", get(http, &path).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chromecast_plays_the_queue() {
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
    let server = start_server(m);
    let http = server.http;
    for _ in 0..100 {
        if get(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // the device, given by address, shows up as a player once the protocol is on
    let mut dev = Fake::start().await;
    let hosts = json!([format!("127.0.0.1:{}", dev.port)]).to_string();
    pref(http, "chromecastHosts", &hosts).await;
    pref(http, "bridgeChromecast", "1").await;
    let mut me = None;
    for _ in 0..100 {
        let players = get(http, "/api/v1/players").await;
        me = players
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["model"] == "chromecast")
            .cloned();
        if me.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let me = me.expect("a chromecast player");
    let id = me["id"].as_str().unwrap().to_string();
    assert_eq!(me["name"], "127.0.0.1");
    assert!(id.starts_with("02:"), "{id}");
    dev.expect("CONNECT", |v| {
        v["type"] == "CONNECT" && v["to"] == "receiver-0"
    })
    .await;

    // play the album: the device starts the media receiver and loads the stream
    let r = command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    assert_eq!(r["count"], 2);
    dev.expect_type(NS_RECEIVER, "LAUNCH").await;
    dev.expect("CONNECT to the app", |v| {
        v["type"] == "CONNECT" && v["to"] == TRANSPORT
    })
    .await;
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    let url = format!("http://127.0.0.1:{http}/stream.mp3?player={id}");
    assert_eq!(load["media"]["contentId"], url);
    assert_eq!(load["media"]["contentType"], "audio/flac");
    assert_eq!(load["media"]["streamType"], "BUFFERED");
    assert_eq!(load["media"]["metadata"]["title"], "One");
    assert_eq!(load["sessionId"], "s-1");
    assert_eq!(load["autoplay"], true);
    let fetched = dev.expect("the stream", |v| v["fetched"].is_string()).await;
    assert_eq!(fetched["fetched"], "fLaC", "{fetched}");

    let p = wait_player(http, &id, "playing", |p| p["mode"] == "play").await;
    assert_eq!(p["now_playing"]["title"], "One");
    dev.ctl.send(Ctl::At(0.5)).unwrap();
    wait_player(http, &id, "the position", |p| {
        p["elapsed"].as_f64().is_some_and(|e| e >= 0.5)
    })
    .await;

    // pause and volume reach the device
    command(http, &id, json!({"command": "pause"})).await;
    dev.expect_type(NS_MEDIA, "PAUSE").await;
    wait_player(http, &id, "paused", |p| p["mode"] == "pause").await;
    command(http, &id, json!({"command": "volume", "value": 33})).await;
    dev.expect("volume 33", |v| {
        v["type"] == "SET_VOLUME" && v["volume"]["level"].as_f64() == Some(0.33)
    })
    .await;
    command(http, &id, json!({"command": "play"})).await;
    dev.expect_type(NS_MEDIA, "PLAY").await;
    wait_player(http, &id, "resumed", |p| p["mode"] == "play").await;

    // the second track is loaded when the first one finishes
    dev.ctl.send(Ctl::Finish).unwrap();
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    assert_eq!(load["media"]["contentId"], url);
    assert_eq!(load["media"]["metadata"]["title"], "Two");
    let fetched = dev.expect("the stream", |v| v["fetched"].is_string()).await;
    assert_eq!(fetched["fetched"], "fLaC", "{fetched}");
    let p = wait_player(http, &id, "second track", |p| p["queue"]["index"] == 1).await;
    assert_eq!(p["now_playing"]["title"], "Two");
    assert_eq!(p["mode"], "play");

    // the last track ends: the player stops
    dev.ctl.send(Ctl::Finish).unwrap();
    wait_player(http, &id, "stopped", |p| p["mode"] == "stop").await;

    // turning the protocol off disconnects it, and the device connection closes
    pref(http, "bridgeChromecast", "0").await;
    wait_player(http, &id, "disconnected", |p| p["connected"] == false).await;
    dev.expect("the connection closed", |v| v["closed"] == true)
        .await;
}

/// The pmviz helper built in `tools/projectm-viz` (`build.sh`), if it is.
fn pmviz() -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/projectm-viz/build/pmviz");
    p.is_file().then_some(p)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chromecast_plays_the_visualiser() {
    need_ffmpeg!();
    let Some(pmviz) = pmviz() else {
        eprintln!("pmviz not built (tools/projectm-viz/build.sh) - skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
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
    // one plain preset is enough
    let presets = dir.path().join("presets");
    std::fs::create_dir_all(&presets).unwrap();
    std::fs::write(
        presets.join("plain.milk"),
        "[preset00]\nzoom=1.01\nwave_mode=2\nwave_r=1\nwave_g=0.5\nwave_b=0.2\n",
    )
    .unwrap();
    let server = start_server(&m);
    let http = server.http;
    for _ in 0..100 {
        if get(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for (k, v) in [
        ("visualizerCommand", pmviz.display().to_string()),
        ("visualizerPresets", presets.display().to_string()),
        ("visualizerSize", "320x180".into()),
        ("visualizerFps", "30".into()),
        ("visualizerEncoder", "libx264".into()),
    ] {
        pref(http, k, &v).await;
    }
    // the settings see pmviz and check what they're given
    let s = tokio::task::spawn_blocking(move || {
        let patch = |body: &str| http_json(http, "PATCH", "/api/v1/settings", Some(body));
        assert_eq!(patch(r#"{"visualizer_quality": 99}"#).0, 400);
        assert_eq!(patch(r#"{"visualizer_fps": "25"}"#).0, 400);
        let (code, text) = patch(r#"{"visualizer_quality": 28, "visualizer_preset_seconds": 20}"#);
        assert_eq!(code, 200, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(s["visualizer_available"], true, "{s}");
    assert_eq!(s["visualizer_encoder"], "libx264");
    assert_eq!(s["visualizer_fps"]["value"], "30");
    assert_eq!(s["visualizer_quality"]["value"], 28);
    assert_eq!(s["visualizer_preset_seconds"]["value"], 20);

    let mut dev = Fake::start().await;
    let hosts = json!([format!("127.0.0.1:{}", dev.port)]).to_string();
    pref(http, "chromecastHosts", &hosts).await;
    pref(http, "bridgeChromecast", "1").await;
    let mut id = None;
    for _ in 0..100 {
        let players = get(http, "/api/v1/players").await;
        id = players
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["model"] == "chromecast")
            .map(|p| p["id"].as_str().unwrap().to_string());
        if id.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let id = id.expect("a chromecast player");
    // a device given by address might have no screen: the visualiser is off until asked for
    let path = format!("/api/v1/players/{}/settings", id.replace(':', "%3A"));
    let s = get(http, &path).await;
    assert_eq!(s["visualizer"]["value"], "auto", "{s}");
    let s = tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "PATCH", &path, Some(r#"{"visualizer": "1"}"#));
        assert_eq!(code, 200, "{text}");
        serde_json::from_str::<Value>(&text).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(s["visualizer"]["value"], "1", "{s}");

    // the album plays as one video stream with the music in it
    command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    let url = load["media"]["contentId"].as_str().unwrap().to_string();
    assert!(
        url.contains("/viz/") && url.ends_with(&format!("?r={}", id.replace(':', ""))),
        "{url}"
    );
    assert_eq!(load["media"]["contentType"], "video/mp4");
    assert_eq!(load["media"]["metadata"]["title"], "One");
    // the device's place in the stream is the track and the position in it
    dev.ctl.send(Ctl::At(0.5)).unwrap();
    let p = wait_player(http, &id, "the first track", |p| {
        p["mode"] == "play" && p["elapsed"].as_f64().is_some_and(|e| e >= 0.5)
    })
    .await;
    assert_eq!(p["now_playing"]["title"], "One");
    // the tracks are 2 seconds long (see `make_audio`): 2.5 s is 0.5 s into "Two"
    dev.ctl.send(Ctl::At(2.5)).unwrap();
    let p = wait_player(http, &id, "the second track", |p| p["queue"]["index"] == 1).await;
    assert_eq!(p["now_playing"]["title"], "Two");
    let e = p["elapsed"].as_f64().unwrap();
    assert!((0.4..1.0).contains(&e), "{e}");

    // the stream is fragmented MP4, and ends with the queue
    let fetched = dev
        .expect_within(Duration::from_secs(30), "the stream", |v| {
            v["fetched"].is_string()
        })
        .await;
    assert_eq!(fetched["mp4"], true, "{fetched}");
    // both tracks' audio is in it (2 + 2 seconds), with video as long
    let path = fetched["path"].as_str().unwrap().to_string();
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,duration",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let probe = String::from_utf8_lossy(&probe.stdout).to_string();
    for kind in ["video", "audio"] {
        let d: f64 = probe
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{kind},")))
            .and_then(|d| d.trim().parse().ok())
            .unwrap_or_else(|| panic!("no {kind} in {probe}"));
        assert!((3.9..4.3).contains(&d), "{kind} {d} s: {probe}");
    }
    // the device plays it to its end: the player stops
    dev.ctl.send(Ctl::Finish).unwrap();
    wait_player(http, &id, "stopped", |p| p["mode"] == "stop").await;
}

/// The Chromecast players, by name (their address).
async fn cast_players(http: u16, n: usize) -> Vec<(String, String)> {
    for _ in 0..100 {
        let players = get(http, "/api/v1/players").await;
        let found: Vec<(String, String)> = players
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["model"] == "chromecast")
            .map(|p| {
                (
                    p["name"].as_str().unwrap_or_default().to_string(),
                    p["id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        if found.len() == n {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("not {n} chromecast players");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_groups_chromecasts_share_one_visualiser() {
    need_ffmpeg!();
    let Some(pmviz) = pmviz() else {
        eprintln!("pmviz not built (tools/projectm-viz/build.sh) - skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path().join("music");
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
    let presets = dir.path().join("presets");
    std::fs::create_dir_all(&presets).unwrap();
    std::fs::write(presets.join("plain.milk"), "[preset00]\nzoom=1.01\n").unwrap();
    let server = start_server(&m);
    let http = server.http;
    for _ in 0..100 {
        if get(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for (k, v) in [
        ("visualizerCommand", pmviz.display().to_string()),
        ("visualizerPresets", presets.display().to_string()),
        ("visualizerSize", "320x180".into()),
        ("visualizerFps", "30".into()),
        ("visualizerEncoder", "libx264".into()),
    ] {
        pref(http, k, &v).await;
    }
    let mut a = Fake::start_on("127.0.0.1").await;
    let mut b = Fake::start_on("127.0.0.2").await;
    let hosts = json!([
        format!("127.0.0.1:{}", a.port),
        format!("127.0.0.2:{}", b.port)
    ])
    .to_string();
    pref(http, "chromecastHosts", &hosts).await;
    pref(http, "bridgeChromecast", "1").await;
    let players = cast_players(http, 2).await;
    let id = |name: &str| {
        players
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| id.clone())
            .unwrap()
    };
    let (ida, idb) = (id("127.0.0.1"), id("127.0.0.2"));
    for p in [&ida, &idb] {
        player_pref(http, p, "visualizer", "1").await;
    }

    // B joins A's group; the album plays on both
    command(http, &idb, json!({"command": "sync", "with": ida})).await;
    command(
        http,
        &ida,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    let la = a.expect_type(NS_MEDIA, "LOAD").await;
    let lb = b.expect_type(NS_MEDIA, "LOAD").await;
    let (ua, ub) = (
        la["media"]["contentId"].as_str().unwrap().to_string(),
        lb["media"]["contentId"].as_str().unwrap().to_string(),
    );
    // one stream: the same session, read by each TV under its own name
    let session = |u: &str| u.split('?').next().unwrap().to_string();
    assert!(ua.contains("/viz/"), "{ua}");
    assert_eq!(session(&ua), session(&ub), "{ua} / {ub}");
    assert_ne!(ua, ub);

    // both follow the stream to the second track
    for at in [0.5, 2.5] {
        a.ctl.send(Ctl::At(at)).unwrap();
        b.ctl.send(Ctl::At(at)).unwrap();
    }
    for p in [&ida, &idb] {
        let s = wait_player(http, p, "the second track", |s| s["queue"]["index"] == 1).await;
        assert_eq!(s["now_playing"]["title"], "Two");
    }
    for dev in [&mut a, &mut b] {
        let f = dev
            .expect_within(Duration::from_secs(30), "the stream", |v| {
                v["fetched"].is_string()
            })
            .await;
        assert_eq!(f["mp4"], true, "{f}");
        let _ = std::fs::remove_file(f["path"].as_str().unwrap());
    }
    a.ctl.send(Ctl::Finish).unwrap();
    b.ctl.send(Ctl::Finish).unwrap();
    wait_player(http, &ida, "stopped", |p| p["mode"] == "stop").await;
}

/// A server with an album of two tracks and the fake Cast device as a player, and the
/// player's id.
async fn one_cast(m: &std::path::Path) -> (Server, Fake, String) {
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
    let server = start_server(m);
    let http = server.http;
    for _ in 0..100 {
        if get(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let dev = Fake::start().await;
    let hosts = json!([format!("127.0.0.1:{}", dev.port)]).to_string();
    pref(http, "chromecastHosts", &hosts).await;
    pref(http, "bridgeChromecast", "1").await;
    let id = cast_players(http, 1).await.remove(0).1;
    (server, dev, id)
}

async fn patch(http: u16, path: &str, body: Value) -> (u16, Value) {
    let path = path.replace(':', "%3A");
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "PATCH", &path, Some(&body.to_string()));
        (code, serde_json::from_str(&text).unwrap_or(Value::Null))
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chromecast_draws_the_visualiser_itself() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let (server, mut dev, id) = one_cast(dir.path()).await;
    let http = server.http;
    let settings = format!("/api/v1/players/{id}/settings");

    // drawn on the TV needs the receiver app set up
    let s = get(http, &settings.replace(':', "%3A")).await;
    let tv = s["visualizer"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["value"] == "tv");
    assert_eq!(
        tv.unwrap()["label"],
        "Drawn on the TV (set up the receiver app first)"
    );
    let (code, _) = patch(
        http,
        "/api/v1/settings",
        json!({"visualizer_receiver_app": "nope"}),
    )
    .await;
    assert_eq!(code, 400);
    let (code, s) = patch(
        http,
        "/api/v1/settings",
        json!({"visualizer_receiver_app": "1a2b3c4d", "visualizer_preset_seconds": 20}),
    )
    .await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["visualizer_receiver_app"], "1A2B3C4D");
    let (code, s) = patch(http, &settings, json!({"visualizer": "tv"})).await;
    assert_eq!(code, 200, "{s}");
    assert_eq!(s["visualizer"]["value"], "tv");

    // the album plays as audio in RMS's receiver app, with the visualiser settings
    command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    let launch = dev.expect_type(NS_RECEIVER, "LAUNCH").await;
    assert_eq!(launch["appId"], "1A2B3C4D");
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    assert_eq!(load["media"]["contentType"], "audio/flac");
    assert_eq!(load["media"]["metadata"]["metadataType"], 3);
    assert_eq!(load["media"]["metadata"]["title"], "One");
    assert_eq!(load["media"]["metadata"]["artist"], "A");
    let rms = &load["customData"]["rms"];
    assert_eq!(rms["version"], 1, "{load}");
    assert_eq!(rms["presetSeconds"], 20);
    assert_eq!(rms["fps"], 30);
    assert_eq!(rms["renderSize"], "1280x720");
    wait_player(http, &id, "playing", |p| p["mode"] == "play").await;

    // the next track the same way
    dev.ctl.send(Ctl::Finish).unwrap();
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    assert_eq!(load["media"]["metadata"]["title"], "Two");
    assert_eq!(load["customData"]["rms"]["version"], 1);
    dev.ctl.send(Ctl::Finish).unwrap();
    wait_player(http, &id, "stopped", |p| p["mode"] == "stop").await;

    // turned off: back to the Default Media Receiver, plain audio
    patch(http, &settings, json!({"visualizer": "0"})).await;
    command(http, &id, json!({"command": "jump", "index": 0})).await;
    let launch = dev.expect_type(NS_RECEIVER, "LAUNCH").await;
    assert_eq!(launch["appId"], "CC1AD845");
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    assert_eq!(load["media"]["metadata"]["title"], "One");
    assert!(load["customData"].is_null(), "{load}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chromecast_that_wont_start_the_receiver_plays_audio() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let (server, mut dev, id) = one_cast(dir.path()).await;
    let http = server.http;
    patch(
        http,
        "/api/v1/settings",
        json!({"visualizer_receiver_app": REFUSED_APP}),
    )
    .await;
    patch(
        http,
        &format!("/api/v1/players/{id}/settings"),
        json!({"visualizer": "tv"}),
    )
    .await;

    command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    let launch = dev.expect_type(NS_RECEIVER, "LAUNCH").await;
    assert_eq!(launch["appId"], REFUSED_APP);
    // the Default Media Receiver instead, and the track plays
    let launch = dev.expect_type(NS_RECEIVER, "LAUNCH").await;
    assert_eq!(launch["appId"], "CC1AD845");
    let load = dev.expect_type(NS_MEDIA, "LOAD").await;
    assert_eq!(load["media"]["metadata"]["title"], "One");
    assert!(load["customData"].is_null(), "{load}");
    wait_player(http, &id, "playing", |p| p["mode"] == "play").await;

    // not tried again on the next track
    dev.ctl.send(Ctl::Finish).unwrap();
    let load = dev
        .expect("the next track", |v| {
            v["type"] == "LOAD" || v["type"] == "LAUNCH"
        })
        .await;
    assert_eq!(load["type"], "LOAD", "{load}");
    assert_eq!(load["media"]["metadata"]["title"], "Two");
}
