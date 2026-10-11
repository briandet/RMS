//! A browser as a player: the WebSocket of `/api/v1/browser-player`, driven here like the
//! web UI's `<audio>` element would drive it.

#[macro_use]
mod common;

use common::*;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

/// The socket and the messages a `recv` skipped, which later ones still get.
struct Browser {
    ws: Ws,
    skipped: Vec<Value>,
}

/// The next message of type `want` from the server.
async fn recv(b: &mut Browser, want: &str) -> Value {
    if let Some(i) = b.skipped.iter().position(|v| v["type"] == want) {
        return b.skipped.remove(i);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, b.ws.next())
            .await
            .unwrap_or_else(|_| panic!("no {want} message (skipped {:?})", b.skipped))
            .expect("socket open")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["type"] == want {
                return v;
            }
            b.skipped.push(v);
        }
    }
}

async fn send(b: &mut Browser, v: Value) {
    b.ws.send(Message::Text(v.to_string().into()))
        .await
        .unwrap();
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
async fn a_browser_plays_the_queue() {
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

    let url = format!(
        "ws://127.0.0.1:{http}/api/v1/browser-player?id=test-browser&name=Test%20Browser&formats=flc,mp3,xyz"
    );
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut ws = Browser {
        ws,
        skipped: Vec::new(),
    };

    // it shows up as a player
    let players = loop {
        let p = get(http, "/api/v1/players").await;
        if p.as_array()
            .is_some_and(|a| a.iter().any(|p| p["name"] == "Test Browser"))
        {
            break p;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let me = players
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Test Browser")
        .unwrap()
        .clone();
    let id = me["id"].as_str().unwrap().to_string();
    assert_eq!(me["model"], "browser");
    assert!(id.starts_with("02:"), "{id}");

    // play the album: the browser is told what to load, in a format it plays
    let r = command(
        http,
        &id,
        json!({"command": "enqueue", "item": {"type": "album", "id": 1}}),
    )
    .await;
    assert_eq!(r["count"], 2);
    let load = recv(&mut ws, "load").await;
    assert_eq!(load["format"], "flc");
    assert_eq!(load["autostart"], true);
    let stream = load["url"].as_str().unwrap().to_string();
    assert!(stream.starts_with("/stream.mp3?player="), "{stream}");
    // the stream is the file's audio
    let (code, _, body) = tokio::task::spawn_blocking(move || http_bytes(http, &stream))
        .await
        .unwrap();
    assert_eq!(code, 200);
    assert!(body.starts_with(b"fLaC"), "{} bytes", body.len());

    send(&mut ws, json!({"type": "started"})).await;
    send(
        &mut ws,
        json!({"type": "progress", "elapsed": 0.5, "duration": 2.0}),
    )
    .await;
    let p = wait_player(http, &id, "playing", |p| p["mode"] == "play").await;
    assert_eq!(p["now_playing"]["title"], "One");

    // pause and volume reach the browser
    command(http, &id, json!({"command": "pause"})).await;
    recv(&mut ws, "pause").await;
    send(&mut ws, json!({"type": "paused"})).await;
    wait_player(http, &id, "paused", |p| p["mode"] == "pause").await;
    command(http, &id, json!({"command": "volume", "value": 33})).await;
    // (earlier updates, from turning the player on, may come first)
    while recv(&mut ws, "volume").await["volume"] != 33 {}
    command(http, &id, json!({"command": "play"})).await;
    recv(&mut ws, "resume").await;
    send(&mut ws, json!({"type": "resumed"})).await;

    // near the end the next track is preloaded, and starts when the first one ends
    send(
        &mut ws,
        json!({"type": "progress", "elapsed": 1.5, "duration": 2.0}),
    )
    .await;
    let pre = recv(&mut ws, "preload").await;
    assert!(pre["url"].as_str().unwrap().starts_with("/stream.mp3"));
    send(&mut ws, json!({"type": "started"})).await;
    let p = wait_player(http, &id, "second track", |p| p["queue"]["index"] == 1).await;
    assert_eq!(p["now_playing"]["title"], "Two");

    // the last track ends: the player stops
    send(
        &mut ws,
        json!({"type": "progress", "elapsed": 1.9, "duration": 2.0}),
    )
    .await;
    send(&mut ws, json!({"type": "ended"})).await;
    wait_player(http, &id, "stopped", |p| p["mode"] == "stop").await;

    // closing the page disconnects it
    ws.ws.close(None).await.unwrap();
    wait_player(http, &id, "disconnected", |p| p["connected"] == false).await;
}
