//! HTTPS: the server's own certificate authority, and the same routes as over HTTP.

#[macro_use]
mod common;

use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn the_web_ui_is_served_over_https_with_a_local_authority() {
    let music = tempfile::tempdir().unwrap();
    let port = free_port();
    let server = start_server_with(music.path(), &["--https-port", &port.to_string()]);
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // the authority's certificate comes from the server
    let (code, ct, pem) = tokio::task::spawn_blocking({
        let http = server.http;
        move || http_bytes(http, "/tls/rms-ca.pem")
    })
    .await
    .unwrap();
    assert_eq!(code, 200);
    assert!(
        ct.to_ascii_lowercase()
            .contains("content-type: application/x-pem-file"),
        "{ct}"
    );
    let (_, ct, der) = tokio::task::spawn_blocking({
        let http = server.http;
        move || http_bytes(http, "/tls/rms-ca.crt")
    })
    .await
    .unwrap();
    assert!(
        ct.to_ascii_lowercase()
            .contains("content-type: application/x-x509-ca-cert"),
        "{ct}"
    );
    assert!(der.len() > 300 && der[0] == 0x30, "DER");

    // a client that trusts it reaches the server by name and by address
    let ca = reqwest::Certificate::from_pem(&pem).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(ca)
        .tls_built_in_root_certs(false)
        .build()
        .unwrap();
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        let url = format!("https://{host}:{port}/api/v1/server");
        let r = client.get(&url).send().await;
        if host == "[::1]" && r.as_ref().is_err_and(|e| e.is_connect()) {
            continue; // no IPv6 here
        }
        let info: serde_json::Value = r
            .unwrap_or_else(|e| panic!("{url}: {e:?}"))
            .json()
            .await
            .unwrap();
        assert_eq!(info["https_port"], port, "{info}");
        assert_eq!(info["local_ca"], true);
    }
    let page = client
        .get(format!("https://localhost:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("<html"));

    // one that doesn't is refused: it is a real certificate check
    let plain = reqwest::Client::builder().build().unwrap();
    assert!(plain
        .get(format!("https://localhost:{port}/api/v1/server"))
        .send()
        .await
        .is_err());
}
