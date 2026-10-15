//! Cometd over a streaming connection, as Squeezer (the CometD Java client) uses it: the
//! stream is one POST held open, and every other message goes in its own POST, concurrently.

use crate::common::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

fn cometd(http: u16, body: Value) -> Value {
    let (code, text) = http_json(http, "POST", "/cometd", Some(&body.to_string()));
    assert_eq!(code, 200, "{body}: {text}");
    serde_json::from_str(&text).unwrap()
}

/// A request's result reaches the stream even when it arrives before the client's
/// `/meta/subscribe` to its response channel: nothing orders those POSTs.
#[test]
fn request_before_subscribe() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let hs = cometd(
        server.http,
        json!([{"channel": "/meta/handshake", "version": "1.0", "id": "1",
                "supportedConnectionTypes": ["streaming"]}]),
    );
    let clid = hs[0]["clientId"].as_str().unwrap().to_string();

    let mut stream = TcpStream::connect(("127.0.0.1", server.http)).unwrap();
    let body = json!([{"channel": "/meta/connect", "clientId": clid, "id": "2",
                       "connectionType": "streaming"}])
    .to_string();
    write!(
        stream,
        "POST /cometd HTTP/1.1\r\nHost: x\r\nContent-Type: text/json;charset=UTF-8\r\n\
         Content-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut seen = String::new();
    let mut read_until = |stream: &mut TcpStream, needle: &str| -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 65536];
        while Instant::now() < deadline {
            if seen.contains(needle) {
                return true;
            }
            if let Ok(n) = stream.read(&mut buf) {
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        seen.contains(needle)
    };
    assert!(
        read_until(&mut stream, "/meta/connect"),
        "no connect reply: {seen}"
    );

    let response = format!("/{clid}/slim/request/0");
    let r = cometd(
        server.http,
        json!([{"channel": "/slim/request", "clientId": clid, "id": "3",
                "data": {"request": ["", ["serverstatus", "0", "255"]], "response": response}}]),
    );
    assert_eq!(r[0]["successful"], true, "{r}");
    cometd(
        server.http,
        json!([{"channel": "/meta/subscribe", "clientId": clid, "id": "4",
                "subscription": format!("/{clid}/slim/request/*")}]),
    );
    assert!(
        read_until(&mut stream, &format!("\"channel\":\"{response}\"")),
        "serverstatus result not on the stream: {seen}"
    );
}
