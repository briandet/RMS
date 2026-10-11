//! A Chromecast as a player: a fake Cast device (TLS, Cast v2 framing, the Default Media
//! Receiver's messages) given to the server in `chromecastHosts`.  The test moves the
//! device's playback along itself, so nothing depends on timing.

#[macro_use]
mod common;

use common::*;
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

const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const TRANSPORT: &str = "web-7";
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
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
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
    let mut launched = false;
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
                    (NS_RECEIVER, kind @ ("GET_STATUS" | "LAUNCH" | "SET_VOLUME")) => {
                        if kind == "LAUNCH" {
                            assert_eq!(p["appId"], "CC1AD845");
                            launched = true;
                        }
                        if kind == "SET_VOLUME" {
                            for k in ["level", "muted"] {
                                if !p["volume"][k].is_null() {
                                    volume[k] = p["volume"][k].clone();
                                }
                            }
                        }
                        let apps = if launched {
                            json!([{"appId": "CC1AD845", "displayName": "Default Media Receiver",
                                    "sessionId": "s-1", "transportId": TRANSPORT}])
                        } else {
                            json!([])
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
                                // the device fetches the stream itself
                                let url = p["media"]["contentId"].as_str().unwrap().to_string();
                                let body = reqwest::get(&url).await.unwrap().bytes().await.unwrap();
                                let head = String::from_utf8_lossy(&body[..body.len().min(4)]);
                                let _ = log.send(json!({"fetched": head, "bytes": body.len()}));
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
