//! Turning on the EffNet analysis: the models are downloaded (here from a stand-in that serves
//! files which aren't models), kept, and a failure to load them shows in the settings.  Off
//! stops it; on again reuses the files already downloaded.

use crate::common::*;
use axum::extract::State;
use axum::http::Uri;
use axum::Router;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

async fn mock(asked: Arc<AtomicUsize>) -> String {
    let app = Router::new()
        .fallback(|State(n): State<Arc<AtomicUsize>>, uri: Uri| async move {
            n.fetch_add(1, Ordering::SeqCst);
            format!("not a model: {}", uri.path())
        })
        .with_state(asked);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}/models/")
}

async fn api(http: u16, method: &'static str, body: Option<Value>) -> Value {
    tokio::task::spawn_blocking(move || {
        let b = body.map(|b| b.to_string());
        let (code, text) = http_json(http, method, "/api/v1/settings", b.as_deref());
        assert_eq!(code, 200, "{method}: {text}");
        serde_json::from_str(&text).unwrap()
    })
    .await
    .unwrap()
}

async fn settled(http: u16) -> Value {
    for _ in 0..200 {
        let s = api(http, "GET", None).await;
        if s["effnet_status"] != "downloading" {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("still downloading");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_models_are_downloaded_when_turned_on() {
    let asked = Arc::new(AtomicUsize::new(0));
    let url = mock(asked.clone()).await;
    let music = tempfile::tempdir().unwrap();
    let server = start_server_with(music.path(), &["--https-port", "0", "--effnet-url", &url]);
    let http = server.http;
    let models = server._dir.path().join("models").join("effnet");

    let s = api(http, "GET", None).await;
    assert_eq!(s["effnet"], false);
    assert_eq!(s["effnet_status"], "off");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "nothing downloaded while off"
    );

    let s = api(http, "PATCH", Some(json!({"effnet": true}))).await;
    assert_eq!(s["effnet"], true);
    let s = settled(http).await;
    assert_eq!(s["effnet_status"], "failed", "{s}");
    assert!(
        s["effnet_error"]
            .as_str()
            .unwrap()
            .contains("loading the models"),
        "{s}"
    );
    let n = asked.load(Ordering::SeqCst);
    assert!(n > 2, "the model and its heads: {n}");
    let onnx = std::fs::read_to_string(models.join("discogs-effnet-bsdynamic-1.onnx")).unwrap();
    assert_eq!(
        onnx,
        "not a model: /models/feature-extractors/discogs-effnet/discogs-effnet-bsdynamic-1.onnx"
    );
    assert_eq!(
        std::fs::read_dir(&models).unwrap().count(),
        n,
        "no .part files left"
    );

    let s = api(http, "PATCH", Some(json!({"effnet": false}))).await;
    assert_eq!(s["effnet_status"], "off");

    // on again: the files are there already
    api(http, "PATCH", Some(json!({"effnet": true}))).await;
    let s = settled(http).await;
    assert_eq!(s["effnet_status"], "failed", "{s}");
    assert_eq!(asked.load(Ordering::SeqCst), n);
}
