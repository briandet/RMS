//! A UPnP media renderer as a player: a fake renderer here serves its description and
//! SOAP control URLs, fetches the streams it is given and plays them on a clock; the
//! server finds it through the `upnpRenderers` pref and plays an album on it.

use crate::common::*;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use quick_xml::escape::escape;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long every track plays on the fake renderer.
const LENGTH: f64 = 5.0;

/// The fake renderer's state.
#[derive(Default)]
struct Fake {
    /// Whether it takes `SetNextAVTransportURI`.
    gapless: bool,
    /// How long tracks play, when not [`LENGTH`].
    length: Option<f64>,
    state: String,
    uri: String,
    next: Option<String>,
    /// The position at `since`, which is set while it plays.
    position: f64,
    since: Option<Instant>,
    /// The actions called (but not the polling ones), with their arguments.
    calls: Vec<(String, HashMap<String, String>)>,
    volume: String,
    mute: String,
    /// The streams fetched: URI and body.
    fetched: Vec<(String, Vec<u8>)>,
}

impl Fake {
    fn position(&self) -> f64 {
        self.position + self.since.map_or(0.0, |s| s.elapsed().as_secs_f64())
    }

    fn length(&self) -> f64 {
        self.length.unwrap_or(LENGTH)
    }

    /// Move the clock on: to the next track, or stop at the end of the last one.
    fn advance(&mut self) {
        while self.state == "PLAYING" && self.position() >= self.length() {
            let over = self.position() - self.length();
            match self.next.take() {
                Some(next) => {
                    self.uri = next;
                    self.position = over;
                    self.since = Some(Instant::now());
                }
                None => {
                    self.state = "STOPPED".into();
                    self.position = 0.0;
                    self.since = None;
                }
            }
        }
    }

    fn freeze(&mut self) {
        self.position = self.position();
        self.since = None;
    }

    fn called(&self, action: &str) -> Vec<HashMap<String, String>> {
        self.calls
            .iter()
            .filter(|(a, _)| a == action)
            .map(|(_, args)| args.clone())
            .collect()
    }
}

struct Renderer {
    fake: Mutex<Fake>,
    http: reqwest::Client,
}

impl Renderer {
    fn with<T>(&self, f: impl FnOnce(&mut Fake) -> T) -> T {
        f(&mut self.fake.lock().unwrap())
    }

    /// Fetch a stream, as a renderer does when it is given one.
    fn fetch(self: &Arc<Self>, uri: String) {
        let me = self.clone();
        tokio::spawn(async move {
            let body = match me.http.get(&uri).send().await {
                Ok(r) => r.bytes().await.map(|b| b.to_vec()).unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            me.with(|f| f.fetched.push((uri, body)));
        });
    }
}

const DESCRIPTION: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>THE_NAME</friendlyName>
    <manufacturer>Test</manufacturer>
    <modelName>Fake Renderer</modelName>
    <UDN>uuid:THE_ID</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>
        <controlURL>/ctl/AVTransport</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId>
        <controlURL>/ctl/RenderingControl</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <controlURL>/ctl/ConnectionManager</controlURL>
      </service>
    </serviceList>
  </device>
</root>"#;

fn hms(seconds: f64) -> String {
    let s = seconds as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn fault(code: u32, description: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><s:Fault>\
         <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail>\
         <UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode>\
         <errorDescription>{description}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
    );
    (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
}

async fn control(
    State(r): State<Arc<Renderer>>,
    Path(service): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let action = headers
        .get("soapaction")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim_matches('"').rsplit_once('#'))
        .map(|(_, a)| a.to_string())
        .unwrap_or_default();
    let doc = roxmltree::Document::parse(&body).expect("SOAP request is XML");
    let args: HashMap<String, String> = doc
        .descendants()
        .find(|n| n.tag_name().name() == action)
        .expect("the action's element")
        .children()
        .filter(|c| c.is_element())
        .map(|c| {
            (
                c.tag_name().name().to_string(),
                c.text().unwrap_or("").to_string(),
            )
        })
        .collect();
    let mut out: Vec<(&str, String)> = Vec::new();
    {
        let mut f = r.fake.lock().unwrap();
        f.advance();
        if !action.starts_with("Get") {
            f.calls.push((action.clone(), args.clone()));
        }
        match action.as_str() {
            "GetProtocolInfo" => {
                out.push(("Source", String::new()));
                out.push((
                    "Sink",
                    "http-get:*:audio/L16:*,http-get:*:audio/flac:*,http-get:*:audio/mpeg:*".into(),
                ));
            }
            "SetAVTransportURI" => {
                let uri = args["CurrentURI"].clone();
                f.uri = uri.clone();
                f.next = None;
                f.state = "STOPPED".into();
                f.position = 0.0;
                f.since = None;
                drop(f);
                r.fetch(uri);
            }
            "SetNextAVTransportURI" => {
                if !f.gapless {
                    return fault(401, "Invalid Action");
                }
                let uri = args["NextURI"].clone();
                f.next = Some(uri.clone());
                drop(f);
                r.fetch(uri);
            }
            "Play" => {
                if f.uri.is_empty() {
                    return fault(701, "Transition not available");
                }
                if f.state != "PLAYING" {
                    f.state = "PLAYING".into();
                    f.since = Some(Instant::now());
                }
            }
            "Pause" => {
                f.freeze();
                f.state = "PAUSED_PLAYBACK".into();
            }
            "Stop" => {
                f.state = "STOPPED".into();
                f.position = 0.0;
                f.since = None;
            }
            "GetTransportInfo" => {
                let state = if f.state.is_empty() {
                    "NO_MEDIA_PRESENT".to_string()
                } else {
                    f.state.clone()
                };
                out.push(("CurrentTransportState", state));
                out.push(("CurrentTransportStatus", "OK".into()));
                out.push(("CurrentSpeed", "1".into()));
            }
            "GetPositionInfo" => {
                out.push(("Track", "1".into()));
                out.push(("TrackDuration", hms(f.length())));
                out.push(("TrackMetaData", String::new()));
                out.push(("TrackURI", f.uri.clone()));
                out.push(("RelTime", hms(f.position())));
                out.push(("AbsTime", "NOT_IMPLEMENTED".into()));
            }
            "SetVolume" => f.volume = args["DesiredVolume"].clone(),
            "SetMute" => f.mute = args["DesiredMute"].clone(),
            _ => return fault(401, "Invalid Action"),
        }
    }
    let mut xml = format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action}Response xmlns:u=\"urn:schemas-upnp-org:service:{service}:1\">"
    );
    for (k, v) in out {
        xml.push_str(&format!("<{k}>{}</{k}>", escape(&v)));
    }
    xml.push_str(&format!("</u:{action}Response></s:Body></s:Envelope>"));
    ([("content-type", "text/xml; charset=\"utf-8\"")], xml).into_response()
}

/// Start a fake renderer; returns it and its description URL.
async fn start_renderer(name: &str, udn: &str, gapless: bool) -> (Arc<Renderer>, String) {
    let renderer = Arc::new(Renderer {
        fake: Mutex::new(Fake {
            gapless,
            ..Default::default()
        }),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    });
    let description = DESCRIPTION.replace("THE_NAME", name).replace("THE_ID", udn);
    let app = axum::Router::new()
        .route(
            "/desc.xml",
            get(move || async move { ([("content-type", "text/xml")], description) }),
        )
        .route("/ctl/{service}", post(control))
        .with_state(renderer.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    (renderer, format!("http://127.0.0.1:{port}/desc.xml"))
}

async fn get_json(http: u16, path: &str) -> Value {
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

/// Set a server pref over JSON-RPC.
async fn pref(http: u16, name: &str, value: &str) {
    let body = json!({"id": 1, "method": "slim.request", "params": ["", ["pref", name, value]]});
    tokio::task::spawn_blocking(move || {
        let (code, text) = http_json(http, "POST", "/jsonrpc.js", Some(&body.to_string()));
        assert_eq!(code, 200, "{text}");
    })
    .await
    .unwrap();
}

/// Wait until `ok` holds (up to `seconds`).
async fn wait_until(what: &str, seconds: u64, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until the player's state satisfies `ok`.
async fn wait_player(http: u16, id: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let path = format!("/api/v1/players/{}", id.replace(':', "%3A"));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let p = get_json(http, &path).await;
        if ok(&p) {
            return p;
        }
        assert!(Instant::now() < deadline, "{what}: {p}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Play a two-track album on a fake renderer that does (`gapless`) or doesn't take the
/// next track in advance.
async fn plays_an_album(gapless: bool) {
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
        if get_json(http, "/api/v1/server").await["library"]["tracks"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let name = if gapless {
        "Gapless Renderer"
    } else {
        "Plain Renderer"
    };
    let (fake, location) = start_renderer(name, &format!("test-{gapless}"), gapless).await;
    pref(http, "upnpRenderers", &json!([location]).to_string()).await;
    pref(http, "bridgeUpnp", "1").await;

    // it shows up as a player
    let deadline = Instant::now() + Duration::from_secs(15);
    let me = loop {
        let players = get_json(http, "/api/v1/players").await;
        let found = players
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == name).cloned());
        if let Some(p) = found {
            break p;
        }
        assert!(Instant::now() < deadline, "no player: {players}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(me["model"], "upnp");
    let id = me["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("02:"), "{id}");

    // play the album: the renderer is given the stream, fetches it and plays it
    let r = command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    assert_eq!(r["count"], 2);
    wait_until("Play", 10, || fake.with(|f| !f.called("Play").is_empty())).await;
    let set = fake.with(|f| f.called("SetAVTransportURI"))[0].clone();
    let first = set["CurrentURI"].clone();
    assert!(
        first.starts_with(&format!("http://127.0.0.1:{http}/stream.mp3?player=")),
        "{first}"
    );
    assert!(
        set["CurrentURIMetaData"].contains("protocolInfo=\"http-get:*:audio/flac:*\""),
        "{}",
        set["CurrentURIMetaData"]
    );
    let p = wait_player(http, &id, "playing", |p| p["mode"] == "play").await;
    assert_eq!(p["now_playing"]["title"], "One");

    // pause and volume reach the renderer
    command(http, &id, json!({"command": "pause"})).await;
    wait_until("paused", 5, || fake.with(|f| f.state == "PAUSED_PLAYBACK")).await;
    wait_player(http, &id, "paused", |p| p["mode"] == "pause").await;
    command(http, &id, json!({"command": "volume", "value": 33})).await;
    wait_until("volume", 5, || fake.with(|f| f.volume == "33")).await;
    command(http, &id, json!({"command": "play"})).await;
    wait_until("resumed", 5, || fake.with(|f| f.state == "PLAYING")).await;
    wait_player(http, &id, "resumed", |p| p["mode"] == "play").await;

    // the second track follows: taken in advance, or given when the first one ended
    let p = wait_player(http, &id, "second track", |p| p["queue"]["index"] == 1).await;
    assert_eq!(p["now_playing"]["title"], "Two");
    assert_eq!(p["mode"], "play");
    let (sets, nexts) = fake.with(|f| {
        (
            f.called("SetAVTransportURI").len(),
            f.called("SetNextAVTransportURI").len(),
        )
    });
    assert_eq!(nexts, 1);
    assert_eq!(sets, if gapless { 1 } else { 2 });

    // the last track ends: the player stops
    wait_player(http, &id, "stopped", |p| p["mode"] == "stop").await;
    assert_eq!(fake.with(|f| f.state.clone()), "STOPPED");
    // it fetched both tracks' streams, each the file's audio
    wait_until("fetched", 5, || fake.with(|f| f.fetched.len() == 2)).await;
    fake.with(|f| {
        assert_eq!(f.fetched[0].0, first);
        assert_ne!(f.fetched[1].0, first);
        for (uri, body) in &f.fetched {
            assert!(body.starts_with(b"fLaC"), "{uri}: {} bytes", body.len());
        }
    });

    // turning the protocol off disconnects it
    pref(http, "bridgeUpnp", "0").await;
    wait_player(http, &id, "disconnected", |p| p["connected"] == false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renderer_plays_the_queue_gaplessly() {
    need_ffmpeg!();
    plays_an_album(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renderer_without_next_uri_plays_the_next_track_after() {
    need_ffmpeg!();
    plays_an_album(false).await;
}

/// The names of the players the server lists.
async fn player_names(http: u16) -> Vec<String> {
    get_json(http, "/api/v1/players")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap_or("").to_string())
        .collect()
}

/// Wait until the listed players satisfy `ok`.
async fn wait_names(http: u16, what: &str, ok: impl Fn(&[String]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let names = player_names(http).await;
        if ok(&names) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {names:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renderer_that_is_also_a_squeezebox_player_is_one_player() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let http = server.http;
    // the renderer and the "Squeezebox" are both at 127.0.0.1: one device
    let (_fake, location) = start_renderer("Speaker (UPnP)", "test-dedupe", true).await;
    pref(http, "upnpRenderers", &json!([location]).to_string()).await;
    pref(http, "bridgeUpnp", "1").await;
    wait_names(http, "the renderer is a player", |n| {
        n.iter().any(|n| n == "Speaker (UPnP)")
    })
    .await;

    // the Squeezebox player connects: it wins, the renderer's player goes
    let caps = "Model=squeezelite,ModelName=SqueezeLite,MaxSampleRate=192000,flc,pcm,mp3";
    let slim = server.slim;
    let player = tokio::task::spawn_blocking(move || {
        let mut p = FakePlayer::connect_as(slim, 12, &[0, 4, 0x20, 0xdd, 0, 1], caps);
        p.expect(b"audg", Duration::from_secs(5));
        p
    })
    .await
    .unwrap();
    wait_names(http, "only the Squeezebox player", |n| {
        !n.iter().any(|n| n == "Speaker (UPnP)")
    })
    .await;

    // found again (the list changed: a search now) while the Squeezebox is connected: not added
    let elsewhere = "http://127.0.0.1:9/desc.xml";
    pref(
        http,
        "upnpRenderers",
        &json!([location, elsewhere]).to_string(),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!player_names(http)
        .await
        .iter()
        .any(|n| n == "Speaker (UPnP)"));

    // the Squeezebox goes away: the renderer is a player again on its next discovery
    drop(player);
    tokio::time::sleep(Duration::from_secs(1)).await;
    pref(http, "upnpRenderers", &json!([location]).to_string()).await;
    wait_names(http, "the renderer is back", |n| {
        n.iter().any(|n| n == "Speaker (UPnP)")
    })
    .await;
    pref(http, "bridgeUpnp", "0").await;
}

/// Renderers give the position in whole seconds; the player's is known to a fraction of a
/// second after a few polls, not up to a second behind.
#[tokio::test(flavor = "multi_thread")]
async fn the_position_is_known_to_a_fraction_of_a_second() {
    need_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("A/long.flac");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi"])
        .args(["-i", "sine=frequency=440:duration=30"])
        .args(["-metadata", "title=Long", "-metadata", "album=Album"])
        .arg(&file)
        .status()
        .unwrap();
    assert!(made.success());
    let server = start_server(dir.path());
    let http = server.http;
    for _ in 0..100 {
        if get_json(http, "/api/v1/server").await["library"]["tracks"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (fake, location) = start_renderer("Clock", "test-clock", true).await;
    fake.with(|f| f.length = Some(30.0));
    pref(http, "upnpRenderers", &json!([location]).to_string()).await;
    pref(http, "bridgeUpnp", "1").await;
    let deadline = Instant::now() + Duration::from_secs(15);
    let id = loop {
        let players = get_json(http, "/api/v1/players").await;
        let found = players
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == "Clock").cloned());
        if let Some(p) = found {
            break p["id"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "no player: {players}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    wait_until("Play", 10, || fake.with(|f| f.state == "PLAYING")).await;

    tokio::time::sleep(Duration::from_secs(4)).await;
    for _ in 0..4 {
        let (cli_port, id2) = (server.cli, id.clone());
        let time = tokio::task::spawn_blocking(move || cli(cli_port, &format!("{id2} time ?")))
            .await
            .unwrap();
        let truth = fake.with(|f| f.position());
        let server_time: f64 = time.last().unwrap().parse().unwrap();
        assert!(
            (server_time - truth).abs() < 0.3,
            "server {server_time:.2} s, renderer {truth:.2} s"
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}
