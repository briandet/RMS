//! What clients see: the requests that Squeezebox hardware, SqueezePlay (Touch, Radio,
//! Controller), Squeezer and Home Assistant send, replayed against a fixed library, with the
//! responses compared to the files in `tests/golden/`.
//!
//! A test fails when any response changes, however slightly.  When a change is intended, run
//! `UPDATE_GOLDEN=1 cargo test -p rms --test conformance` and review the diff of
//! `tests/golden/` like code: it is exactly what those clients will see differently.
//!
//! Values that differ from run to run (temporary paths, ports, timestamps, cover ids derived
//! from file times) are replaced by placeholders before comparing.

#[macro_use]
mod common;

use common::*;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------------------------

/// ```text
/// Alpha Band/First Light/01 Morning.flac, 02 Noon.flac     (rock, 1999)
/// Beta Singer/Second Wind/01 Breeze.mp3                    (jazz, 2005, composer)
/// Various/Mixed Bag/01 One.mp3, 02 Two.mp3                 (compilation, two artists)
/// ```
fn make_library() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let m = dir.path();
    let band = [
        ("artist", "Alpha Band"),
        ("album", "First Light"),
        ("genre", "Rock"),
        ("date", "1999"),
    ];
    make_audio(
        &m.join("Alpha Band/First Light/01 Morning.flac"),
        0,
        &[&band[..], &[("title", "Morning"), ("TRACKNUMBER", "1")]].concat(),
    );
    make_audio(
        &m.join("Alpha Band/First Light/02 Noon.flac"),
        1,
        &[&band[..], &[("title", "Noon"), ("TRACKNUMBER", "2")]].concat(),
    );
    make_audio(
        &m.join("Beta Singer/Second Wind/01 Breeze.mp3"),
        2,
        &[
            ("title", "Breeze"),
            ("artist", "Beta Singer"),
            ("album", "Second Wind"),
            ("track", "1"),
            ("genre", "Jazz"),
            ("date", "2005"),
            ("composer", "Gamma Writer"),
        ],
    );
    for (i, artist) in ["Delta", "Epsilon"].iter().enumerate() {
        make_audio(
            &m.join(format!(
                "Various/Mixed Bag/0{} {}.mp3",
                i + 1,
                ["One", "Two"][i]
            )),
            3 + i as u32,
            &[
                ("title", ["One", "Two"][i]),
                ("artist", artist),
                ("album", "Mixed Bag"),
                ("track", &(i + 1).to_string()),
                ("genre", "Pop"),
            ],
        );
    }
    dir
}

/// A server with the fixture library scanned and a Squeezebox 3 (`baby` firmware family is
/// the Radio; device 4 is the SB2/3) and a squeezelite player connected.
struct Fixture {
    server: Server,
    music: tempfile::TempDir,
    players: Vec<FakePlayer>,
}

const SB3_MAC: [u8; 6] = [0x00, 0x04, 0x20, 0x12, 0x34, 0x56];
const SB3: &str = "00:04:20:12:34:56";

fn fixture() -> Fixture {
    let music = make_library();
    let server = start_server(music.path());
    // wait for the startup scan
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let r = rpc(&server, "", json!(["serverstatus", 0, 0]));
        if r.get("rescan").is_none() && r["info total songs"] == 5 {
            break;
        }
        assert!(Instant::now() < deadline, "scan did not finish: {r}");
        std::thread::sleep(Duration::from_millis(100));
    }
    // no rescans by themselves while clients look (tests/watch.rs tests those)
    rpc(&server, "", json!(["pref", "autorescan", "0"]));
    // every track added at once: tracks are stamped as the scan stores them (and history
    // with the files' times), so a scan or a file write crossing a second would reorder
    // "newest" and "recently added" between runs
    let db = rusqlite::Connection::open(server._dir.path().join("library.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(10)).unwrap();
    db.execute_batch(
        "UPDATE tracks SET added_at = (SELECT MIN(added_at) FROM tracks);
         UPDATE track_stats SET added_at = (SELECT MIN(added_at) FROM track_stats);",
    )
    .unwrap();
    let mut squeezelite = FakePlayer::connect(server.slim);
    squeezelite.expect(b"setd", Duration::from_secs(5));
    squeezelite.send(b"SETD", b"\0Kitchen\0");
    let mut sb3 = FakePlayer::connect_as(server.slim, 4, &SB3_MAC, "");
    sb3.expect(b"audg", Duration::from_secs(5));
    // both registered
    let deadline = Instant::now() + Duration::from_secs(5);
    while rpc(&server, "", json!(["players", 0, 10]))["count"] != 2 {
        assert!(Instant::now() < deadline, "players did not connect");
        std::thread::sleep(Duration::from_millis(50));
    }
    // a favorites folder with a station, and a track
    rpc(
        &server,
        "",
        json!(["favorites", "addlevel", "title:Stations"]),
    );
    rpc(
        &server,
        "",
        json!([
            "favorites",
            "add",
            "item_id:0.0",
            "url:http://radio.example.com/live.mp3",
            "title:Example FM"
        ]),
    );
    rpc(
        &server,
        "",
        json!([
            "favorites",
            "add",
            "url:db:album.title=First%20Light",
            "title:First Light"
        ]),
    );
    std::thread::sleep(Duration::from_millis(300));
    Fixture {
        server,
        music,
        players: vec![squeezelite, sb3],
    }
}

// ---------------------------------------------------------------------------------------------
// transports
// ---------------------------------------------------------------------------------------------

/// One JSON-RPC `slim.request`; returns `result`.
fn rpc(server: &Server, player: &str, cmd: Value) -> Value {
    let body = json!({"id": 1, "method": "slim.request", "params": [player, cmd]}).to_string();
    let (code, body) = http_json(server.http, "POST", "/jsonrpc.js", Some(&body));
    assert_eq!(code, 200, "{cmd}: {body}");
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    v.get("result").cloned().unwrap_or(Value::Null)
}

/// A Bayeux (Cometd) client, as SqueezePlay and Squeezer use.
struct Cometd {
    http: u16,
    client: String,
    next_id: u64,
    /// events that arrived with other responses, not yet asked for
    pending: Vec<Value>,
}

impl Cometd {
    fn handshake(server: &Server) -> Cometd {
        let r = post(
            server.http,
            json!([{"channel": "/meta/handshake", "version": "1.0",
                    "supportedConnectionTypes": ["long-polling", "streaming"]}]),
        );
        let client = r[0]["clientId"].as_str().expect("clientId").to_string();
        Cometd {
            http: server.http,
            client,
            next_id: 1,
            pending: Vec::new(),
        }
    }

    /// POST and keep every event of the response for later.
    fn exchange(&mut self, body: Value) -> Vec<Value> {
        let r = post(self.http, body);
        let events = r.as_array().cloned().unwrap_or_default();
        self.pending.extend(events.iter().cloned());
        events
    }

    fn take(&mut self, channel: &str) -> Option<Value> {
        let i = self.pending.iter().position(|e| e["channel"] == channel)?;
        Some(self.pending.remove(i)["data"].clone())
    }

    /// `/slim/request`: the result for this request.
    fn request(&mut self, player: &str, cmd: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let response = format!("/{}/slim/request/{}", self.client, id);
        self.exchange(
            json!([{"channel": "/slim/request", "clientId": self.client, "id": id,
                    "data": {"request": [player, cmd], "response": response}}]),
        );
        self.take(&response).unwrap_or(Value::Null)
    }

    /// `/slim/subscribe` to `response` (relative to the client, like `/slim/serverstatus`).
    fn subscribe(&mut self, player: &str, cmd: Value, response: &str) {
        self.next_id += 1;
        let events = self.exchange(
            json!([{"channel": "/slim/subscribe", "clientId": self.client,
                    "id": self.next_id, "data": {"request": [player, cmd],
                    "response": format!("/{}{}", self.client, response)}}]),
        );
        assert!(
            events
                .iter()
                .any(|e| e["channel"] == "/slim/subscribe" && e["successful"] == true),
            "subscribe {cmd}: {events:?}"
        );
    }

    /// Long-poll until an event on `channel` (relative to the client) arrives.
    fn event(&mut self, channel: &str) -> Value {
        let full = format!("/{}{}", self.client, channel);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(e) = self.take(&full) {
                return e;
            }
            assert!(Instant::now() < deadline, "no event on {channel}");
            self.exchange(json!([{"channel": "/meta/connect", "clientId": self.client,
                    "connectionType": "long-polling", "advice": {"timeout": 2000}}]));
        }
    }
}

fn post(http: u16, body: Value) -> Value {
    let (code, text) = http_json(http, "POST", "/cometd", Some(&body.to_string()));
    assert_eq!(code, 200, "{body}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// golden files
// ---------------------------------------------------------------------------------------------

/// Keys whose values change from run to run.
const VOLATILE: &[&str] = &[
    "version",
    "serverVersion",
    "created",
    "changed",
    "played",
    "starred",
    "lastModified",
    "uuid",
    "ip",
    "player_ip",
    "lastscan",
    "playlist_timestamp",
    "time",
    "seq_no",
    "mac",
    "server_timestamp",
    "addedTime",
    "added",
    "last_played",
    "lastUpdated",
    "modificationTime",
    "info total duration",
    "httpport",
];

/// Keys holding cover ids (derived from file times).
const COVER_KEYS: &[&str] = &["coverid", "artwork_track_id", "icon-id", "coverArt"];

struct Golden {
    /// what to replace in strings: (from, placeholder)
    substitutions: Vec<(String, String)>,
}

impl Golden {
    fn new(f: &Fixture) -> Golden {
        let music = f.music.path();
        let canonical = std::fs::canonicalize(music).unwrap_or(music.to_path_buf());
        let mut subs = Vec::new();
        for p in [canonical.as_path(), music] {
            let url = url_of(p);
            subs.push((url.clone(), "file:///<music>".into()));
            subs.push((url.replace("file://", ""), "/<music>".into()));
            subs.push((p.display().to_string(), "/<music>".into()));
            // the same, URI-escaped inside another URL (`/imageproxy/<url>/…`, favorites)
            subs.push((uri_escape(&url), uri_escape("file:///<music>")));
        }
        subs.push((format!("127.0.0.1:{}", f.server.http), "<server>".into()));
        subs.push((format!(":{}", f.server.http), ":<port>".into()));
        Golden {
            substitutions: subs,
        }
    }

    /// Also replace `from` (an id the test made) with `to`.
    fn replacing(mut self, from: &str, to: &str) -> Golden {
        if !from.is_empty() {
            self.substitutions.push((from.to_string(), to.to_string()));
        }
        self
    }

    fn normalize(&self, v: &Value, key: Option<&str>) -> Value {
        match v {
            Value::Object(m) => {
                let mut out = Map::new();
                for (k, v) in m {
                    let scalar = v.is_string() || v.is_number();
                    let nv = if VOLATILE.contains(&k.as_str()) && scalar {
                        json!("<volatile>")
                    } else {
                        self.normalize(v, Some(k))
                    };
                    out.insert(k.clone(), nv);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.normalize(x, key)).collect()),
            Value::String(s) => {
                if key.is_some_and(|k| COVER_KEYS.contains(&k)) && is_cover_id(s) {
                    return json!("<cover>");
                }
                let mut s = s.clone();
                for (from, to) in &self.substitutions {
                    s = s.replace(from, to);
                }
                Value::String(cover_in_paths(&s))
            }
            other => other.clone(),
        }
    }

    /// Compare `v` (normalized) with `tests/golden/<name>.json`, or write it when
    /// `UPDATE_GOLDEN` is set.
    fn check(&self, name: &str, v: &Value) {
        let v = self.normalize(v, None);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "no golden file {} (run with UPDATE_GOLDEN=1 to create it)",
                path.display()
            )
        });
        if want != text {
            let first = want
                .lines()
                .zip(text.lines())
                .position(|(a, b)| a != b)
                .unwrap_or(want.lines().count().min(text.lines().count()));
            let show = |s: &str| {
                s.lines()
                    .skip(first.saturating_sub(3))
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            panic!(
                "{name}: response differs from {} at line {}\n--- expected\n{}\n--- got\n{}\n\
                 (if intended: UPDATE_GOLDEN=1 cargo test -p rms --test conformance)",
                path.display(),
                first + 1,
                show(&want),
                show(&text)
            );
        }
    }
}

fn url_of(p: &Path) -> String {
    url::Url::from_file_path(p).unwrap().to_string()
}

const URL_PATH: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn uri_escape(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, URL_PATH).to_string()
}

fn is_cover_id(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `music/<cover id>/cover…` → `music/<cover>/cover…`
fn cover_in_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("music/") {
        let (head, tail) = rest.split_at(i + "music/".len());
        out.push_str(head);
        if tail.len() >= 9 && is_cover_id(&tail[..8]) && tail.as_bytes()[8] == b'/' {
            out.push_str("<cover>");
            rest = &tail[8..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------------------------

/// Home Assistant's squeezebox integration (pysqueezebox): JSON-RPC, values as strings.
#[test]
fn home_assistant() {
    need_ffmpeg!();
    let f = fixture();
    let g = Golden::new(&f);
    let s = &f.server;
    let q = |player: &str, cmd: Value| rpc(s, player, cmd);
    let mut out = Map::new();
    let mut rec = |name: &str, v: Value| {
        out.insert(name.to_string(), v);
    };

    // setup and the coordinator poll (its stray empty array is really sent)
    rec(
        "serverstatus_libraryname",
        q("", json!(["serverstatus", "-", "-", "prefs:libraryname"])),
    );
    rec("serverstatus_bare", q("", json!(["serverstatus"])));
    rec(
        "serverstatus_poll",
        q("", json!(["serverstatus", "-", "-", []])),
    );
    rec(
        "can_restartserver",
        q("", json!(["can", "restartserver", "?"])),
    );
    rec("players_status", q("", json!(["players", "status"])));
    rec("status_exists", q(MAC_S, json!(["status"])));

    // browsing (sent with the player id)
    let browse = |cmd: Value| q(MAC_S, cmd);
    rec("apps", browse(json!(["apps", 0, 50])));
    rec("radios", browse(json!(["radios", 0, 50])));
    rec("albums_count", browse(json!(["albums", "0", "1"])));
    rec("albums", browse(json!(["albums", "0", "50", "tags:jla"])));
    rec(
        "albums_new",
        browse(json!(["albums", "0", "50", "tags:jla", "sort:new"])),
    );
    rec(
        "albums_of_artist",
        browse(json!(["albums", "0", "50", "artist_id:2", "tags:jla"])),
    );
    rec(
        "albums_search",
        browse(json!(["albums", "0", "50", "search:light", "tags:jla"])),
    );
    rec("artists", browse(json!(["artists", "0", "50"])));
    rec(
        "album_artists",
        browse(json!(["artists", "0", "50", "role_id:ALBUMARTIST"])),
    );
    rec("genres", browse(json!(["genres", "0", "50"])));
    rec(
        "titles_of_album",
        browse(json!([
            "titles",
            "0",
            "50",
            "album_id:1",
            "sort:albumtrack",
            "tags:ju"
        ])),
    );
    rec(
        "titles_of_genre",
        browse(json!([
            "titles",
            "0",
            "50",
            "genre_id:2",
            "sort:albumtrack",
            "tags:ju"
        ])),
    );
    rec("playlists", browse(json!(["playlists", "0", "50"])));
    rec(
        "favorites",
        browse(json!(["favorites", "items", "0", "50", "want_url:1"])),
    );
    rec(
        "favorites_item_null",
        browse(json!([
            "favorites",
            "items",
            "0",
            "50",
            "item_id:0",
            null,
            "want_url:1"
        ])),
    );

    // control: commands answer {} on success
    let c = |cmd: Value| q(MAC_S, cmd);
    rec("cmd_volume", c(json!(["mixer", "volume", "30"])));
    rec("cmd_volume_relative", c(json!(["mixer", "volume", "-5"])));
    rec("cmd_mute", c(json!(["mixer", "muting", "1"])));
    rec("cmd_unmute", c(json!(["mixer", "muting", "0"])));
    rec("cmd_power_off", c(json!(["power", "0"])));
    rec("cmd_power_on", c(json!(["power", "1"])));
    let first = format!(
        "{}/Alpha%20Band/First%20Light/01%20Morning.flac",
        url_of(f.music.path())
    );
    let second = format!(
        "{}/Alpha%20Band/First%20Light/02%20Noon.flac",
        url_of(f.music.path())
    );
    rec("cmd_playlist_play", c(json!(["playlist", "play", first])));
    rec("cmd_playlist_add", c(json!(["playlist", "add", second])));
    rec("cmd_shuffle", c(json!(["playlist", "shuffle", "1"])));
    rec("cmd_shuffle_off", c(json!(["playlist", "shuffle", "0"])));
    rec("cmd_repeat", c(json!(["playlist", "repeat", "2"])));
    rec("cmd_time", c(json!(["time", "1.0"])));
    rec("cmd_pause", c(json!(["pause", "1"])));
    rec("cmd_stop", c(json!(["stop"])));
    rec("cmd_sync", c(json!(["sync", SB3])));
    rec("syncgroups", q("", json!(["syncgroups", "?"])));
    rec("cmd_unsync", c(json!(["sync", "-"])));
    rec(
        "cmd_alarm_add",
        c(json!([
            "alarm",
            "add",
            "time:25200",
            "dow:1,2,3,4,5",
            "enabled:1",
            "repeat:1",
            "volume:40"
        ])),
    );
    let alarms = q(MAC_S, json!(["alarms", "0", "99", "filter:all"]));
    let alarm_id = alarms["alarms_loop"][0]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    rec("alarms", alarms);
    rec(
        "cmd_alarm_update",
        c(json!([
            "alarm",
            "update",
            "time:27000",
            "dow:0,6",
            "enabled:0",
            "repeat:1",
            format!("id:{alarm_id}")
        ])),
    );
    rec(
        "alarms_after_update",
        q(MAC_S, json!(["alarms", "0", "99", "filter:all"])),
    );
    rec(
        "cmd_alarm_delete",
        c(json!(["alarm", "delete", format!("id:{alarm_id}")])),
    );
    rec(
        "alarms_enabled",
        q(MAC_S, json!(["playerpref", "alarmsEnabled", "?"])),
    );
    rec(
        "cmd_playlist_save",
        c(json!(["playlist", "save", "tempplaylist_000420feed01"])),
    );
    rec("cmd_playlist_clear", c(json!(["playlist", "clear"])));
    rec(
        "cmd_playlist_resume",
        c(json!([
            "playlist",
            "resume",
            "tempplaylist_000420feed01",
            "noplay:1",
            "wipePlaylist"
        ])),
    );
    rec("cmd_playlist_index", c(json!(["playlist", "index", "+1"])));
    rec("cmd_stop_again", c(json!(["stop"])));

    // the player poll, after all that
    rec(
        "status",
        q(
            MAC_S,
            json!(["status", "-", "1", "tags:acdIKlNorTuxQ", "alarmData:1"]),
        ),
    );
    rec(
        "status_playlist",
        q(MAC_S, json!(["status", "0", "10", "tags:acdIKlNorTuxQ"])),
    );
    g.replacing(&alarm_id, "<alarm id>")
        .check("home_assistant", &Value::Object(out));
}

/// SqueezePlay (Squeezebox Touch, Radio, Controller): Cometd, numbers as numbers.  The
/// subscriptions it makes on connect, the home menu, then down My Music to a track's
/// context menu, following the actions the server hands out.
#[test]
fn squeezeplay() {
    need_ffmpeg!();
    let f = fixture();
    let g = Golden::new(&f);
    let mut c = Cometd::handshake(&f.server);
    let mut out = Map::new();

    c.subscribe(
        "",
        json!(["serverstatus", 0, 50, "subscribe:60"]),
        "/slim/serverstatus",
    );
    out.insert("serverstatus".into(), c.event("/slim/serverstatus"));
    let ps = format!("/slim/playerstatus/{MAC_S}");
    c.subscribe(
        MAC_S,
        json!([
            "status",
            "-",
            10,
            "menu:menu",
            "useContextMenu:1",
            "subscribe:600"
        ]),
        &ps,
    );
    out.insert("playerstatus".into(), c.event(&ps));
    c.subscribe(
        MAC_S,
        json!(["displaystatus", "subscribe:showbriefly"]),
        &format!("/slim/displaystatus/{MAC_S}"),
    );
    c.subscribe(
        MAC_S,
        json!(["menustatus"]),
        &format!("/slim/menustatus/{MAC_S}"),
    );
    c.subscribe(
        "",
        json!([
            "firmwareupgrade",
            "firmwareVersion:7.7.3 r16676",
            "inSetup:0",
            "machine:fab4",
            "subscribe:0"
        ]),
        "/slim/firmwarestatus",
    );
    out.insert("firmwarestatus".into(), c.event("/slim/firmwarestatus"));

    let home = c.request(MAC_S, json!(["menu", 0, 100, "direct:1"]));
    let ids: Vec<&str> = home["item_loop"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(ids.contains(&"myMusicArtistsAllArtists"), "{ids:?}");
    out.insert("menu".into(), home.clone());

    // follow `go` actions down: artists → an artist → an album → a track's context menu
    let artists_item = home["item_loop"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "myMusicArtistsAllArtists")
        .unwrap()
        .clone();
    let artists = c.request(MAC_S, action(&home, &artists_item, "go"));
    out.insert("artists".into(), artists.clone());
    let alpha = find_item(&artists, "Alpha Band");
    let albums = c.request(MAC_S, action(&artists, &alpha, "go"));
    out.insert("albums_of_artist".into(), albums.clone());
    let album = find_item(&albums, "First Light");
    let tracks = c.request(MAC_S, action(&albums, &album, "go"));
    out.insert("tracks_of_album".into(), tracks.clone());
    let track = find_item(&tracks, "Morning");
    out.insert(
        "track_context_menu".into(),
        c.request(MAC_S, action(&tracks, &track, "more")),
    );

    // player commands as SqueezePlay sends them (note the JSON `false`)
    out.insert(
        "cmd_power".into(),
        c.request(MAC_S, json!(["power", "1", false, "seq_no:5"])),
    );
    out.insert(
        "cmd_volume".into(),
        c.request(MAC_S, json!(["mixer", "volume", 42, "seq_no:6"])),
    );
    out.insert(
        "cmd_muting_toggle".into(),
        c.request(MAC_S, json!(["mixer", "muting", "toggle", "seq_no:7"])),
    );
    out.insert(
        "cmd_muting_toggle_back".into(),
        c.request(MAC_S, json!(["mixer", "muting"])),
    );
    out.insert(
        "cmd_mode_stop".into(),
        c.request(MAC_S, json!(["mode", "stop"])),
    );
    out.insert(
        "status_page".into(),
        c.request(
            MAC_S,
            json!(["status", 0, 10, "menu:menu", "useContextMenu:1"]),
        ),
    );
    g.check("squeezeplay", &Value::Object(out));
}

/// Squeezer (Android): Cometd, everything as strings.
#[test]
fn squeezer() {
    need_ffmpeg!();
    let f = fixture();
    let g = Golden::new(&f);
    let mut c = Cometd::handshake(&f.server);
    let mut out = Map::new();
    let serverstatus = json!(["serverstatus", "0", "255",
        "prefs:mediadirs,defeatDestructiveTouchToPlay",
        "playerprefs:alarmDefaultVolume,alarmfadeseconds,alarmSnoozeSeconds,alarmTimeoutSeconds,alarmsEnabled,playtrackalbum,defeatDestructiveTouchToPlay,syncVolume,syncPower,digitalVolumeControl"]);
    out.insert("serverstatus".into(), c.request("", serverstatus.clone()));
    let mut sub = serverstatus.as_array().unwrap().clone();
    sub.push(json!("subscribe:60"));
    c.subscribe("", Value::Array(sub), "/slim/serverstatus");
    c.subscribe(
        MAC_S,
        json!([
            "status",
            "-",
            "1",
            "menu:menu",
            "useContextMenu:1",
            "subscribe:0"
        ]),
        &format!("/slim/playerstatus/{MAC_S}"),
    );
    c.subscribe(
        MAC_S,
        json!(["displaystatus", "subscribe:showbriefly"]),
        &format!("/slim/displaystatus/{MAC_S}"),
    );
    c.subscribe(
        MAC_S,
        json!(["menustatus"]),
        &format!("/slim/menustatus/{MAC_S}"),
    );
    out.insert(
        "status_menu".into(),
        c.request(
            MAC_S,
            json!(["status", "-", "1", "menu:menu", "useContextMenu:1"]),
        ),
    );
    out.insert(
        "menu".into(),
        c.request(MAC_S, json!(["menu", "0", "512", "direct:1"])),
    );
    // song details as the now-playing screen asks for them
    c.request(MAC_S, json!(["playlistcontrol", "cmd:add", "track_id:3"]));
    out.insert(
        "status_song".into(),
        c.request(MAC_S, json!(["status", "-", "1", "tags:ABdejJKlrStTuxy"])),
    );
    out.insert(
        "alarms".into(),
        c.request(
            MAC_S,
            json!(["alarms", "0", "512", "menu:menu", "filter:all"]),
        ),
    );
    out.insert(
        "alarm_playlists".into(),
        c.request(MAC_S, json!(["alarm", "playlists", "0", "512"])),
    );
    // its quirks: an empty fade argument, a fade with a leading space
    out.insert(
        "cmd_play_empty_fade".into(),
        c.request(MAC_S, json!(["play", ""])),
    );
    out.insert("cmd_pause".into(), c.request(MAC_S, json!(["pause", "1"])));
    out.insert(
        "cmd_resume_fade".into(),
        c.request(MAC_S, json!(["pause", "0", " 5"])),
    );
    out.insert(
        "cmd_volume_up".into(),
        c.request(MAC_S, json!(["mixer", "volume", "+5"])),
    );
    out.insert(
        "volume".into(),
        c.request(MAC_S, json!(["mixer", "volume", "?"])),
    );
    out.insert("cmd_name".into(), c.request(MAC_S, json!(["name", "Den"])));
    out.insert(
        "cmd_playlist_clear".into(),
        c.request(MAC_S, json!(["playlist", "clear"])),
    );
    g.check("squeezer", &Value::Object(out));
}

/// A Squeezebox 3 driven with its remote: what its screen shows, drawn from the bitmaps the
/// server sends (`grfe`).
#[test]
fn squeezebox3_screens() {
    need_ffmpeg!();
    let mut f = fixture();
    let g = Golden::new(&f);
    let p = &mut f.players[1];
    let start = Instant::now();
    let mut screens = Map::new();
    let mut press = |p: &mut FakePlayer, name: &str, code: u32| {
        let mut b = Vec::with_capacity(10);
        b.extend_from_slice(&(start.elapsed().as_millis() as u32 + 1000).to_be_bytes());
        b.extend_from_slice(&[0xff, 32]);
        b.extend_from_slice(&code.to_be_bytes());
        p.send(b"IR  ", &b);
        // the screen after the transition the press starts
        let deadline = Instant::now() + Duration::from_secs(5);
        let frame = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let fr = p.expect(b"grfe", left);
            if fr[2] != b'c' {
                p.send(b"ANIC", &[]);
                break fr;
            }
        };
        screens.insert(
            format!("{:02} {}", screens.len() + 1, name),
            json!(render(&frame[4..])),
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    const UP: u32 = 0x7689e01f;
    const DOWN: u32 = 0x7689b04f;
    const LEFT: u32 = 0x7689906f;
    const RIGHT: u32 = 0x7689d02f;
    press(p, "down: My Music", DOWN);
    press(p, "up: Now Playing", UP);
    press(p, "down: My Music", DOWN);
    press(p, "right: into My Music", RIGHT);
    press(p, "right: first entry", RIGHT);
    press(p, "down", DOWN);
    press(p, "left", LEFT);
    press(p, "left: My Music", LEFT);
    g.check("squeezebox3_screens", &Value::Object(screens));
}

/// The native API as the web UI uses it: library browsing, search, favorites, settings.
#[test]
fn native_api() {
    need_ffmpeg!();
    let f = fixture();
    let g = Golden::new(&f);
    let http = f.server.http;
    let mut out = Map::new();
    let mut call = |name: &str, method: &str, path: &str, body: Option<Value>| -> Value {
        let body = body.map(|b| b.to_string());
        let (code, text) = http_json(http, method, path, body.as_deref());
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        let v = json!({ "status": code, "body": v });
        out.insert(format!("{method} {path} ({name})"), v.clone());
        v["body"].clone()
    };
    macro_rules! get {
        ($name:expr, $path:expr $(,)?) => {
            call($name, "GET", $path, None)
        };
    }

    get!("artists", "/api/v1/artists");
    get!("artists page", "/api/v1/artists?offset=1&limit=2",);
    get!("artist", "/api/v1/artists/2");
    get!("unknown artist", "/api/v1/artists/999");
    get!("albums", "/api/v1/albums");
    get!("albums by artist", "/api/v1/albums?sort=artist");
    get!("recently added", "/api/v1/albums?sort=added");
    get!("bad sort", "/api/v1/albums?sort=size");
    get!("albums of artist", "/api/v1/albums?artist=3");
    get!("albums of genre", "/api/v1/albums?genre=1");
    get!("albums of year", "/api/v1/albums?year=2005");
    get!("albums page", "/api/v1/albums?offset=1&limit=1&sort=artist",);
    get!("bad limit", "/api/v1/albums?limit=many");
    get!("bad id", "/api/v1/albums/first");
    get!("album", "/api/v1/albums/1");
    get!("compilation", "/api/v1/albums/3");
    get!("unknown album", "/api/v1/albums/999");
    get!("genres", "/api/v1/genres");
    get!("years", "/api/v1/years");
    get!("tracks", "/api/v1/tracks?limit=2");
    // (tempo, key and loudness come from the sonic analysis, which runs in the background
    // while these requests are made: tests/../library.rs covers filtering on them)
    get!(
        "tracks by year",
        "/api/v1/tracks?year_from=2000&sort=year&limit=3"
    );
    get!("tracks of genre", "/api/v1/tracks?genre=1&limit=3");
    get!(
        "tracks never played",
        "/api/v1/tracks?played=false&sort=title&limit=2"
    );
    get!("bad key", "/api/v1/tracks?key=13Z");
    get!("bad track sort", "/api/v1/tracks?sort=loudest");
    get!("bad genre list", "/api/v1/tracks?genre=1,rock");
    get!("unknown track", "/api/v1/tracks/999");
    get!("similar to unknown track", "/api/v1/tracks/999/similar");
    let top = get!("top folder", "/api/v1/folders");
    let alpha = top["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Alpha Band")
        .expect("Alpha Band folder")["id"]
        .clone();
    let first = get!("artist folder", &format!("/api/v1/folders?id={alpha}"),);
    let album_dir = first["items"][0]["id"].clone();
    get!("album folder", &format!("/api/v1/folders?id={album_dir}"),);
    get!("search", "/api/v1/search?q=light");
    get!("search two words", "/api/v1/search?q=beta%20wind",);
    get!("search without words", "/api/v1/search");
    get!("favorites", "/api/v1/favorites");
    get!("favorites folder", "/api/v1/favorites?id=0");
    get!("favorites page", "/api/v1/favorites?offset=1&limit=1",);
    get!("folder page", "/api/v1/folders?offset=1&limit=1");
    get!("search page", "/api/v1/search?q=a&offset=0&limit=1",);

    // playlists: save the queue, then browse it
    let pid = MAC_S.replace(':', "%3A");
    let enqueue = json!({"command": "enqueue", "item": {"type": "album", "id": 1}, "mode": "add"});
    call(
        "queue an album",
        "POST",
        &format!("/api/v1/players/{pid}/command"),
        Some(enqueue),
    );
    let playlist_dir = tempfile::tempdir().unwrap();
    let dir = playlist_dir.path().to_str().unwrap();
    rpc(&f.server, "", json!(["pref", "playlistdir", dir]));
    rpc(&f.server, MAC_S, json!(["playlist", "save", "Evening"]));
    let lists = get!("playlists", "/api/v1/playlists");
    let list = lists["items"][0]["id"].clone();
    get!("playlist", &format!("/api/v1/playlists/{list}"));
    assert!(list.is_number(), "a playlist was saved: {lists}");
    get!(
        "playlist tracks",
        &format!("/api/v1/playlists/{list}/tracks"),
    );
    get!("unknown playlist", "/api/v1/playlists/999/tracks",);

    // favorites: add, queue, delete
    let fav = json!({"url": "http://radio.example.com/other.mp3", "title": "Other FM"});
    call("add favorite", "POST", "/api/v1/favorites", Some(fav));
    call(
        "add favorite without url",
        "POST",
        "/api/v1/favorites",
        Some(json!({"url": " ", "title": "x"})),
    );
    get!("favorites after add", "/api/v1/favorites");
    let clear = json!({"command": "clear"});
    call(
        "clear",
        "POST",
        &format!("/api/v1/players/{pid}/command"),
        Some(clear),
    );
    let fav_item =
        json!({"command": "enqueue", "item": {"type": "favorite", "id": "1"}, "mode": "add"});
    call(
        "queue a favorite",
        "POST",
        &format!("/api/v1/players/{pid}/command"),
        Some(fav_item),
    );
    std::thread::sleep(Duration::from_millis(500));
    get!("queue", &format!("/api/v1/players/{pid}/queue"));
    call("delete favorite", "DELETE", "/api/v1/favorites/2", None);
    get!("favorites after delete", "/api/v1/favorites");

    // settings
    get!("settings", "/api/v1/settings");
    let patch = json!({"library_name": "Test Library", "online_artist_images": false});
    call("change settings", "PATCH", "/api/v1/settings", Some(patch));
    call(
        "bad bitrate",
        "PATCH",
        "/api/v1/settings",
        Some(json!({"spotify_bitrate": 100})),
    );
    call(
        "unknown setting",
        "PATCH",
        "/api/v1/settings",
        Some(json!({"colour": "red"})),
    );
    get!(
        "player settings",
        &format!("/api/v1/players/{pid}/settings"),
    );
    let sb3 = SB3.replace(':', "%3A");
    get!(
        "squeezebox settings",
        &format!("/api/v1/players/{sb3}/settings"),
    );
    let patch = json!({"transition_type": 1, "transition_duration": 5, "dont_stop_the_music": "0"});
    call(
        "change player settings",
        "PATCH",
        &format!("/api/v1/players/{pid}/settings"),
        Some(patch),
    );
    let patch = json!({"playing_mode": "2", "text_size": "0", "brightness_on": "3"});
    call(
        "change display settings",
        "PATCH",
        &format!("/api/v1/players/{sb3}/settings"),
        Some(patch),
    );
    let patch = json!({"text_size": "0"});
    call(
        "no display",
        "PATCH",
        &format!("/api/v1/players/{pid}/settings"),
        Some(patch),
    );
    let patch = json!({"transition_type": 9});
    call(
        "bad transition",
        "PATCH",
        &format!("/api/v1/players/{pid}/settings"),
        Some(patch),
    );
    call(
        "rescan playlists",
        "POST",
        "/api/v1/library/scan",
        Some(json!({"mode": "playlists"})),
    );
    get!("no spotify", "/api/v1/spotify");
    get!("radio providers", "/api/v1/radio");
    get!(
        "radio directory",
        "/api/v1/radio?id=radiodirectory.0&limit=2"
    );
    get!("radio item from elsewhere", "/api/v1/radio?id=pref.0");

    // a hand-picked list, in its own order
    call(
        "clear for a list",
        "POST",
        &format!("/api/v1/players/{pid}/command"),
        Some(json!({"command": "clear"})),
    );
    let list = json!({"command": "enqueue", "item": {"type": "tracks", "ids": [3, 1, 999]}, "mode": "add"});
    call(
        "queue a list of tracks",
        "POST",
        &format!("/api/v1/players/{pid}/command"),
        Some(list),
    );
    std::thread::sleep(Duration::from_millis(500));
    get!("queue of a list", &format!("/api/v1/players/{pid}/queue"));

    // details, with what the background analysis may or may not have measured yet blanked
    let mut detail = |name: &str, path: &str| {
        let (code, text) = http_json(http, "GET", path, None);
        let mut v: Value = serde_json::from_str(&text).unwrap();
        let mut blank = |t: &mut Value| {
            if let Some(d) = t.get_mut("detail").and_then(Value::as_object_mut) {
                for k in ["bpm", "key", "loudness", "analyzed"] {
                    d.remove(k);
                }
            }
        };
        match v.get_mut("items").and_then(Value::as_array_mut) {
            Some(items) => items.iter_mut().for_each(&mut blank),
            None => blank(&mut v),
        }
        out.insert(
            format!("GET {path} ({name})"),
            json!({"status": code, "body": v}),
        );
    };
    detail("tracks with detail", "/api/v1/tracks?limit=2&detail=true");
    detail("track", "/api/v1/tracks/1");

    // similar songs: which ones depends on how far the background analysis has got, so
    // record only that some come back, and that the seed and the excluded track don't
    let similar = |out: &mut Map<String, Value>, name: &str, req: Value| {
        let (code, text) = http_json(http, "POST", "/api/v1/similar", Some(&req.to_string()));
        let v: Value = serde_json::from_str(&text).unwrap();
        let ids: Vec<i64> = v
            .as_array()
            .map(|a| a.iter().filter_map(|t| t["id"].as_i64()).collect())
            .unwrap_or_default();
        out.insert(
            format!("POST /api/v1/similar ({name})"),
            json!({"status": code, "found": !ids.is_empty(), "has_seed_or_excluded":
                ids.iter().any(|i| *i == 1 || *i == 2), "error": v.get("error")}),
        );
    };
    similar(
        &mut out,
        "similar to a track",
        json!({"seeds": [{"type": "track", "id": 1}], "exclude": [2], "limit": 3}),
    );
    similar(
        &mut out,
        "similar to an album",
        json!({"seeds": [{"type": "album", "id": 1}], "limit": 2, "detail": true}),
    );
    similar(&mut out, "no seeds", json!({"seeds": []}));
    similar(
        &mut out,
        "url seed",
        json!({"seeds": [{"type": "url", "url": "http://x"}]}),
    );

    g.check("native_api", &Value::Object(out));
}

/// A Subsonic app: browsing, search, stars, ratings, plays, playlists, the jukebox, then
/// streaming and signing in.
#[test]
fn subsonic() {
    need_ffmpeg!();
    let f = fixture();
    let g = Golden::new(&f);
    let http = f.server.http;
    let playlist_dir = tempfile::tempdir().unwrap();
    rpc(
        &f.server,
        "",
        json!(["pref", "playlistdir", playlist_dir.path().to_str().unwrap()]),
    );
    let out = std::cell::RefCell::new(Map::new());
    let call = |name: &str, query: &str| -> Value {
        let path = format!(
            "/rest/{query}{}f=json",
            if query.contains('?') { "&" } else { "?" }
        );
        let (code, text) = http_json(http, "GET", &path, None);
        assert_eq!(code, 200, "{path}: {text}");
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{path}: {text}"));
        let v = v["subsonic-response"].clone();
        out.borrow_mut()
            .insert(format!("{name}: {query}"), v.clone());
        v
    };

    call("ping", "ping.view?u=x&p=y&v=1.16.1&c=test");
    call("license", "getLicense");
    call("extensions", "getOpenSubsonicExtensions");
    call("folders", "getMusicFolders");
    let idx = call("indexes", "getIndexes");
    let dir = idx["indexes"]["index"][0]["artist"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let d = call("artist folder", &format!("getMusicDirectory?id={dir}"));
    let sub = d["directory"]["child"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    call("album folder", &format!("getMusicDirectory?id={sub}"));
    call("album as directory", "getMusicDirectory?id=al-1");
    call("artists", "getArtists");
    call("artist", "getArtist?id=ar-2");
    call("album", "getAlbum?id=al-1");
    call("compilation", "getAlbum?id=al-3");
    call("song", "getSong?id=tr-3");
    call("unknown song", "getSong?id=tr-999");
    call("missing id", "getSong");
    call("genres", "getGenres");
    call("by name", "getAlbumList2?type=alphabeticalByName");
    call("by artist", "getAlbumList?type=alphabeticalByArtist");
    call("newest", "getAlbumList2?type=newest&size=2");
    call(
        "by year",
        "getAlbumList2?type=byYear&fromYear=2010&toYear=1990",
    );
    call("by genre", "getAlbumList2?type=byGenre&genre=jazz");
    let random = call("random", "getRandomSongs?size=3");
    out.borrow_mut().insert(
        "random: getRandomSongs?size=3".into(),
        json!(random["randomSongs"]["song"].as_array().map(|a| a.len())),
    );
    call("songs by genre", "getSongsByGenre?genre=Pop");
    call("search3", "search3?query=light");
    call("search3 words", "search3?query=beta%20wind");
    call(
        "search3 everything",
        "search3?query=%22%22&artistCount=1&albumCount=1&songCount=2&songOffset=1",
    );
    call("search2", "search2?query=alpha");

    // stars are favorites
    call("star", "star?id=tr-1&albumId=al-2&artistId=ar-3");
    call("starred", "getStarred2");
    call("starred (folders)", "getStarred");
    call("starred album", "getAlbum?id=al-2");
    out.borrow_mut().insert(
        "favorites after starring".into(),
        rpc(
            &f.server,
            "",
            json!(["favorites", "items", 0, 20, "want_url:1"]),
        ),
    );
    call("unstar", "unstar?id=tr-1&albumId=al-2&artistId=ar-3");
    call("starred after unstar", "getStarred2");
    call("rate", "setRating?id=tr-1&rating=4");
    call("scrobble", "scrobble?id=tr-1&submission=true");
    call("now playing notice", "scrobble?id=tr-2&submission=false");
    call("rated and played", "getSong?id=tr-1");
    call("frequent", "getAlbumList2?type=frequent");
    call("highest", "getAlbumList2?type=highest");

    // playlists
    call(
        "create playlist",
        "createPlaylist?name=Road&songId=tr-1&songId=tr-3",
    );
    let lists = call("playlists", "getPlaylists");
    let pl = lists["playlists"]["playlist"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    call(
        "update playlist",
        &format!(
            "updatePlaylist?playlistId={pl}&name=Long%20Road&songIdToAdd=tr-2&songIndexToRemove=0"
        ),
    );
    call("playlist", &format!("getPlaylist?id={pl}"));
    call("delete playlist", &format!("deletePlaylist?id={pl}"));
    call("playlists after delete", "getPlaylists");

    call("user", "getUser?username=x");
    call("scan status", "getScanStatus");
    call("lyrics", "getLyricsBySongId?id=tr-1");
    // without sonic analysis the mix is random: record how many
    let similar = call("similar", "getSimilarSongs2?id=tr-1");
    out.borrow_mut().insert(
        "similar: getSimilarSongs2?id=tr-1".into(),
        json!(similar["similarSongs2"]["song"].as_array().map(|a| a.len())),
    );
    call("radio", "getInternetRadioStations");
    call("unsupported", "getVideoInfo?id=1");

    // the jukebox is the first player (Kitchen)
    call("jukebox set", "jukeboxControl?action=set&id=tr-1&id=tr-2");
    call("jukebox gain", "jukeboxControl?action=setGain&gain=0.25");
    call("jukebox get", "jukeboxControl?action=get");

    // XML, the default format
    let (code, xml) = http_json(http, "GET", "/rest/getAlbum.view?id=al-1&u=x&p=y", None);
    assert_eq!(code, 200);
    let volatile =
        regex::Regex::new(r#"(created|played|changed|starred|serverVersion)="[^"]*""#).unwrap();
    let xml = volatile.replace_all(&xml, r#"$1="<volatile>""#);
    out.borrow_mut().insert(
        "xml: getAlbum".into(),
        json!(xml.lines().collect::<Vec<_>>()),
    );

    // media
    let flac = std::fs::metadata(
        f.music
            .path()
            .join("Alpha Band/First Light/01 Morning.flac"),
    )
    .unwrap()
    .len();
    let (code, head, body) = http_bytes(http, "/rest/stream?id=tr-1");
    assert_eq!(code, 200, "{head}");
    assert_eq!(body.len() as u64, flac, "a stream as it is is the file");
    assert!(
        head.contains("audio/flac") || head.contains("audio/x-flac"),
        "{head}"
    );
    let (code, head, body) = http_bytes(http, "/rest/stream?id=tr-1&maxBitRate=64");
    assert_eq!(code, 200, "{head}");
    assert!(head.contains("audio/mpeg"), "{head}");
    assert!(
        body.len() > 1000 && (body.starts_with(b"ID3") || body[0] == 0xff),
        "MP3: {} bytes",
        body.len()
    );
    let (code, head, body) = http_bytes(http, "/rest/stream?id=tr-3&format=opus");
    assert_eq!(code, 200, "{head}");
    assert!(
        head.contains("audio/ogg") && body.starts_with(b"OggS"),
        "{head}"
    );
    let (code, head, _) = http_bytes(http, "/rest/download?id=tr-3");
    assert_eq!(code, 200);
    assert!(head.contains("attachment"), "{head}");
    call("no cover art", "getCoverArt?id=al-1&size=64");

    // signing in, once the server has a password
    for (k, v) in [
        ("username", "me"),
        ("password", "secret"),
        ("authorize", "1"),
    ] {
        rpc(&f.server, "", json!(["pref", k, v]));
    }
    let auth = |name: &str, query: &str| {
        let (_, text) = http_json(http, "GET", &format!("/rest/ping?f=json&{query}"), None);
        let v: Value = serde_json::from_str(&text).unwrap();
        out.borrow_mut().insert(
            format!("auth {name}: {query}"),
            v["subsonic-response"].clone(),
        );
    };
    auth("none", "");
    auth("wrong password", "u=me&p=nope");
    auth("wrong user", "u=you&p=secret");
    auth("password", "u=me&p=secret");
    auth("hex password", "u=me&p=enc:736563726574");
    let token = format!("{:x}", md5::compute("secretc0ffee"));
    auth("token", &format!("u=me&t={token}&s=c0ffee"));
    auth(
        "wrong token",
        "u=me&t=0123456789abcdef0123456789abcdef&s=c0ffee",
    );
    auth("api key", "apiKey=abc");
    let (code, _) = http_json(http, "GET", "/api/v1/server", None);
    assert_eq!(code, 401, "the rest of the server still wants HTTP Basic");

    g.check("subsonic", &Value::Object(out.into_inner()));
}

/// A 320×32 one-bit Squeezebox display (columns of 4 bytes, top bit first) as text.
fn render(bitmap: &[u8]) -> Vec<String> {
    let cols = bitmap.len() / 4;
    (0..32)
        .map(|y| {
            let row: String = (0..cols)
                .map(|x| {
                    let byte = bitmap[x * 4 + y / 8];
                    if byte & (0x80 >> (y % 8)) != 0 {
                        '#'
                    } else {
                        '.'
                    }
                })
                .collect();
            row.trim_end_matches('.').to_string()
        })
        .collect()
}

/// The request an item's action makes, as SqueezePlay builds it: the item's own action, or
/// the response's `base` action with the item's `itemsParams` merged into its params.
fn action(result: &Value, item: &Value, name: &str) -> Value {
    let (def, extra) = match item["actions"].get(name) {
        Some(a) if !a.is_null() => (a.clone(), Value::Null),
        _ => {
            let base = result["base"]["actions"][name].clone();
            let extra = base["itemsParams"]
                .as_str()
                .map(|k| item[k].clone())
                .unwrap_or(Value::Null);
            (base, extra)
        }
    };
    assert!(!def.is_null(), "no {name} action for {item}");
    let mut cmd: Vec<Value> = def["cmd"].as_array().cloned().unwrap_or_default();
    cmd.extend([json!(0), json!(200)]);
    let mut params: Vec<(String, String)> = Vec::new();
    for src in [&def["params"], &extra] {
        if let Some(p) = src.as_object() {
            for (k, v) in p {
                params.retain(|(x, _)| x != k);
                params.push((k.clone(), value_text(v)));
            }
        }
    }
    params.sort();
    cmd.extend(params.into_iter().map(|(k, v)| json!(format!("{k}:{v}"))));
    cmd.push(json!("useContextMenu:1"));
    Value::Array(cmd)
}

fn find_item(result: &Value, text: &str) -> Value {
    result["item_loop"]
        .as_array()
        .unwrap_or_else(|| panic!("no item_loop in {result}"))
        .iter()
        .find(|i| i["text"].as_str().is_some_and(|t| t.contains(text)))
        .unwrap_or_else(|| panic!("no {text} in {result}"))
        .clone()
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
