//! Scrobbling and the ListenBrainz menu, against a stand-in for the ListenBrainz API: an
//! account added by token, *now playing* from a player and from a Subsonic app, a listen
//! queued while ListenBrainz is down and sent later with its MusicBrainz id, the playlists
//! made for the user playing from the library, and recommendations in mixes.

use crate::common::*;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const REC_A: &str = "aaaaaaaa-1111-1111-1111-000000000001";
const REC_B: &str = "bbbbbbbb-2222-2222-2222-000000000002";
const REC_C: &str = "eeeeeeee-5555-5555-5555-000000000005";
const NOWHERE: &str = "cccccccc-3333-3333-3333-000000000003";
const PLAYLIST: &str = "dddddddd-4444-4444-4444-000000000004";

#[derive(Default)]
struct Mock {
    /// Bodies of `submit-listens`.
    submitted: Mutex<Vec<Value>>,
    /// Listens (not "now playing") to refuse with 503 before taking them.
    down: AtomicUsize,
}

async fn mock(m: Arc<Mock>) -> String {
    let app = Router::new()
        .route(
            "/1/validate-token",
            get(|h: HeaderMap| async move {
                let ok = h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Token good");
                Json(if ok {
                    json!({"code": 200, "valid": true, "user_name": "alice"})
                } else {
                    json!({"code": 200, "valid": false})
                })
            }),
        )
        .route(
            "/1/submit-listens",
            post(
                |State(m): State<Arc<Mock>>, h: HeaderMap, Json(b): Json<Value>| async move {
                    assert_eq!(h.get("authorization").unwrap(), "Token good");
                    if b["listen_type"] != "playing_now" && m.down.load(Ordering::SeqCst) > 0 {
                        m.down.fetch_sub(1, Ordering::SeqCst);
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error": "down for maintenance"})),
                        );
                    }
                    m.submitted.lock().push(b);
                    (StatusCode::OK, Json(json!({"status": "ok"})))
                },
            ),
        )
        .route(
            "/1/cf/recommendation/user/{user}/recording",
            get(|Path(user): Path<String>| async move {
                assert_eq!(user, "alice");
                Json(json!({"payload": {"mbids": [
                    {"recording_mbid": REC_C, "score": 0.9},
                    {"recording_mbid": NOWHERE, "score": 0.5}
                ]}}))
            }),
        )
        .route(
            "/1/user/{user}/playlists/createdfor",
            get(|| async {
                Json(json!({"playlists": [{"playlist": {
                    "identifier": format!("https://listenbrainz.org/playlist/{PLAYLIST}"),
                    "title": "Weekly Jams for alice"
                }}]}))
            }),
        )
        .route(
            "/1/playlist/{id}",
            get(|Path(id): Path<String>| async move {
                assert_eq!(id, PLAYLIST);
                Json(
                    json!({"playlist": {"title": "Weekly Jams for alice", "track": [
                        {"identifier": [format!("https://musicbrainz.org/recording/{NOWHERE}")],
                         "title": "Nowhere", "creator": "Ghost"},
                        {"identifier": [format!("https://musicbrainz.org/recording/{REC_A}")],
                         "title": "Song A", "creator": "Artist A"}
                    ]}}),
                )
            }),
        )
        .route(
            "/1/metadata/recording/",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let mut out = serde_json::Map::new();
                for m in q["recording_mbids"].split(',') {
                    let (title, artist) = match m {
                        REC_A => ("Song A", "Artist A"),
                        REC_B => ("Song B", "Artist B"),
                        REC_C => ("Song C", "Artist C"),
                        NOWHERE => ("Nowhere", "Ghost"),
                        _ => continue,
                    };
                    out.insert(
                        m.into(),
                        json!({
                            "recording": {"name": title, "length": 3000, "isrcs": []},
                            "artist": {"name": artist, "artists": [{"artist_mbid": null}]}
                        }),
                    );
                }
                Json(Value::Object(out))
            }),
        )
        .with_state(m);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn tone(path: &std::path::Path, hz: u32, meta: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency={hz}:duration=3"));
    for (k, v) in meta {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success());
}

async fn call(http: u16, method: &'static str, path: String, body: Option<Value>) -> (u16, Value) {
    tokio::task::spawn_blocking(move || {
        let b = body.map(|b| b.to_string());
        let (code, text) = http_json(http, method, &path, b.as_deref());
        (code, serde_json::from_str(&text).unwrap_or(Value::Null))
    })
    .await
    .unwrap()
}

async fn api(http: u16, method: &'static str, path: String, body: Option<Value>) -> Value {
    let (code, v) = call(http, method, path.clone(), body).await;
    assert_eq!(code, 200, "{method} {path}: {v}");
    v
}

/// Wait for `f` to hold, up to 15 s.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..150 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn listens_and_listenbrainz_playlists() {
    need_ffmpeg!();
    let m = Arc::new(Mock::default());
    let url = mock(m.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let music = dir.path();
    tone(
        &music.join("a/1.flac"),
        300,
        &[
            ("title", "Song A"),
            ("artist", "Artist A"),
            ("album", "Album A"),
            ("track", "3"),
            ("MUSICBRAINZ_TRACKID", REC_A),
        ],
    );
    tone(
        &music.join("b/1.flac"),
        600,
        &[
            ("title", "Song B"),
            ("artist", "Artist B"),
            ("MUSICBRAINZ_TRACKID", REC_B),
        ],
    );
    tone(
        &music.join("c/1.flac"),
        900,
        &[
            ("title", "Song C"),
            ("artist", "Artist C"),
            ("MUSICBRAINZ_TRACKID", REC_C),
        ],
    );
    let server = start_server_with(
        music,
        &[
            "--https-port",
            "0",
            "--listenbrainz-api-url",
            &url,
            "--listenbrainz-url",
            &url,
        ],
    );
    let http = server.http;
    until("scanned", || {
        let (_, b) = http_json(http, "GET", "/api/v1/server", None);
        serde_json::from_str::<Value>(&b).unwrap()["library"]["tracks"] == 3
    })
    .await;

    // an account, by its token
    let (code, e) = call(
        http,
        "POST",
        "/api/v1/listenbrainz/accounts".into(),
        Some(json!({"token": "bad"})),
    )
    .await;
    assert_eq!(code, 422, "{e}");
    let a = api(
        http,
        "POST",
        "/api/v1/listenbrainz/accounts".into(),
        Some(json!({"token": "good"})),
    )
    .await;
    assert_eq!(a, json!({"user": "alice", "default": true}));
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(
        s["listenbrainz_accounts"],
        json!([{"user": "alice", "default": true}])
    );

    // a player plays Song A: ListenBrainz hears it's playing
    let mut p = tokio::task::spawn_blocking(move || FakePlayer::connect(server.slim))
        .await
        .unwrap();
    let pid = MAC_S.replace(':', "%3A");
    until("player", || {
        http_json(http, "GET", &format!("/api/v1/players/{pid}"), None).0 == 200
    })
    .await;
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["scrobble_to"], json!(["alice"]), "the default account");
    let tracks = api(http, "GET", "/api/v1/tracks?limit=10".into(), None).await;
    let id_of = |title: &str| {
        tracks["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["title"] == title)
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let (a_id, b_id) = (id_of("Song A"), id_of("Song B"));
    api(
        http,
        "POST",
        format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "enqueue", "item": {"type": "track", "id": a_id}, "mode": "play"})),
    )
    .await;
    p = tokio::task::spawn_blocking(move || {
        p.expect_strm(b's', Duration::from_secs(5));
        p.stat(b"STMc", 0);
        p.stat(b"STMs", 0);
        p
    })
    .await
    .unwrap();
    until("now playing from the player", || {
        m.submitted.lock().iter().any(|b| {
            b["listen_type"] == "playing_now"
                && b["payload"][0]["track_metadata"]["track_name"] == "Song A"
        })
    })
    .await;
    {
        let sub = m.submitted.lock();
        let np = &sub
            .iter()
            .find(|b| b["listen_type"] == "playing_now")
            .unwrap()["payload"][0]["track_metadata"];
        assert_eq!(np["artist_name"], "Artist A");
        assert_eq!(np["release_name"], "Album A");
        assert_eq!(np["additional_info"]["recording_mbid"], REC_A);
        assert_eq!(np["additional_info"]["tracknumber"], 3);
        assert_eq!(np["additional_info"]["media_player"], "Rusty Music Server");
    }

    // a Subsonic app played Song B while ListenBrainz was down: the listen waits
    m.down.store(1, Ordering::SeqCst);
    let (code, _) = tokio::task::spawn_blocking(move || {
        http_json(
            http,
            "GET",
            &format!("/rest/scrobble?id=tr-{b_id}&time=1700000000000&u=x&p=y&f=json"),
            None,
        )
    })
    .await
    .unwrap();
    assert_eq!(code, 200);
    until("refused once", || m.down.load(Ordering::SeqCst) == 0).await;
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["scrobble_pending"], 1, "{s}");
    assert_eq!(s["scrobble_error"], "down for maintenance");
    until("sent once ListenBrainz is back", || {
        m.submitted
            .lock()
            .iter()
            .any(|b| b["listen_type"] == "single")
    })
    .await;
    {
        let sub = m.submitted.lock();
        let l = &sub.iter().find(|b| b["listen_type"] == "single").unwrap()["payload"][0];
        assert_eq!(l["listened_at"], 1_700_000_000);
        assert_eq!(l["track_metadata"]["track_name"], "Song B");
        assert_eq!(
            l["track_metadata"]["additional_info"]["recording_mbid"],
            REC_B
        );
    }
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["scrobble_pending"], 0);

    // the ListenBrainz menu: recommendations and the playlists made for alice
    let top = api(http, "GET", "/api/v1/listenbrainz".into(), None).await;
    let names: Vec<&str> = top["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Recommended for alice", "Weekly Jams for alice"]);
    let pl = api(
        http,
        "GET",
        format!("/api/v1/listenbrainz?id=pl:{PLAYLIST}"),
        None,
    )
    .await;
    assert_eq!(pl["total"], 2);
    assert_eq!(pl["items"][1]["name"], "Song A — Artist A");
    // played: what the library has (Song A, by its MusicBrainz id); no Spotify for the rest
    api(http, "POST", format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "enqueue", "item": {"type": "listenbrainz", "id": format!("pl:{PLAYLIST}")}, "mode": "add"}))).await;
    let q = api(http, "GET", format!("/api/v1/players/{pid}/queue"), None).await;
    let titles: Vec<&str> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Song A", "Song A"]);
    let (code, e) = call(http, "POST", format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "enqueue", "item": {"type": "listenbrainz", "id": format!("rec:{NOWHERE}")}, "mode": "add"}))).await;
    assert_eq!(code, 422, "{e}");

    // recommendations in mixes: Song C, recommended for alice and never played (Song B was)
    api(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"listenbrainz": true})),
    )
    .await;
    api(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"adventurous": 40})),
    )
    .await;
    api(
        http,
        "POST",
        format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "mix", "seed": {"type": "track", "id": a_id}})),
    )
    .await;
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["discovered"], 1, "{s}");

    // players can scrobble to nobody; only known accounts
    let (code, _) = call(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"scrobble_to": ["bob"]})),
    )
    .await;
    assert_eq!(code, 400);
    let s = api(
        http,
        "PATCH",
        "/api/v1/settings".into(),
        Some(json!({"scrobble_to": []})),
    )
    .await;
    assert_eq!(s["scrobble_to"], json!([]));

    // removing the account
    api(
        http,
        "DELETE",
        "/api/v1/listenbrainz/accounts/alice".into(),
        None,
    )
    .await;
    let s = api(http, "GET", "/api/v1/settings".into(), None).await;
    assert_eq!(s["listenbrainz_accounts"], json!([]));
    let top = api(http, "GET", "/api/v1/listenbrainz".into(), None).await;
    assert_eq!(top["total"], 0);
    drop(p);
}
