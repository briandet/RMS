//! Radio Browser (<https://www.radio-browser.info>): the free, community-run directory of
//! internet radio stations, as the `radiobrowser` radio provider (`radiobrowser items|playlist`).
//!
//! Like the Spotify menus, levels are built on demand from the directory's JSON API (kept in a
//! short cache) and rendered by the XMLBrowser, so they work in Squeezer/SqueezePlay, on the
//! player's screen and in the web UI.  Item ids are index paths; a search term rides in its
//! component (`0_jazz`).  Playing a station tells the directory about it (`/json/url/<uuid>`),
//! which is how its popularity lists are made.

use crate::control::dispatch::{DispatchTable, Handler};
use crate::control::jive::xmlbrowser::{
    self, component_index, component_search, escape_search, split_item_id, Ctx, Feed, Item, Level,
};
use crate::control::request::Request;
use crate::server::Server;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

pub const TAG: &str = "radiobrowser";
pub const NAME: &str = "Radio Browser";

/// Asked for the list of API servers; any of them answers the same.
const DISCOVERY: &str = "https://all.api.radio-browser.info/json/servers";
/// Used when discovery fails.
const FALLBACK_SERVER: &str = "de1.api.radio-browser.info";
const CACHE_TTL: Duration = Duration::from_secs(600);
const TIMEOUT: Duration = Duration::from_secs(15);
/// Stations listed per level.
const STATIONS: usize = 300;
/// Countries / genres / languages listed.
const CATEGORIES: usize = 300;

#[derive(Default)]
struct State {
    /// The API host in use, once discovered.
    host: Option<String>,
    cache: HashMap<String, (Instant, Arc<Value>)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().get_or_insert_with(State::default))
}

pub fn register(t: &mut DispatchTable) {
    t.add(
        &[TAG, "items", "_index", "_quantity"],
        0,
        true,
        true,
        Handler::Async(|s, r| Box::pin(items_query(s, r))),
    );
    t.add(
        &[TAG, "playlist", "_method"],
        1,
        true,
        true,
        Handler::Async(|s, r| Box::pin(items_query(s, r))),
    );
}

/// The API hosts to try: the one that worked last, else those discovery lists.
async fn hosts(http: &reqwest::Client) -> Vec<String> {
    if let Some(h) = with_state(|s| s.host.clone()) {
        return vec![h];
    }
    let mut hosts: Vec<String> = match http.get(DISCOVERY).timeout(TIMEOUT).send().await {
        Ok(resp) => resp
            .json::<Vec<Value>>()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|s| s["name"].as_str().map(str::to_string))
            .collect(),
        Err(e) => {
            debug!("radio browser discovery: {e}");
            Vec::new()
        }
    };
    hosts.sort();
    hosts.dedup();
    {
        use rand::seq::SliceRandom;
        hosts.shuffle(&mut rand::rng());
    }
    if !hosts.iter().any(|h| h == FALLBACK_SERVER) {
        hosts.push(FALLBACK_SERVER.into());
    }
    hosts
}

/// `GET /json/<path>` from the directory, cached.
async fn api(http: &reqwest::Client, path: &str) -> Result<Arc<Value>, String> {
    if let Some(v) = with_state(|s| {
        s.cache
            .get(path)
            .filter(|(at, _)| at.elapsed() < CACHE_TTL)
            .map(|(_, v)| v.clone())
    }) {
        return Ok(v);
    }
    let mut last_err = String::from("Radio Browser is not reachable");
    for host in hosts(http).await {
        let url = format!("https://{host}/json/{path}");
        let res = async {
            http.get(&url)
                .timeout(TIMEOUT)
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await;
        match res {
            Ok(v) => {
                let v = Arc::new(v);
                with_state(|s| {
                    s.host = Some(host);
                    s.cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
                    s.cache
                        .insert(path.to_string(), (Instant::now(), v.clone()));
                });
                return Ok(v);
            }
            Err(e) => {
                debug!("radio browser {url}: {e}");
                last_err = format!("Radio Browser: {e}");
                with_state(|s| s.host = None);
            }
        }
    }
    Err(last_err)
}

/// A menu link to a list of stations or categories.
fn link(name: &str, rb: &str) -> Item {
    let mut it = Item {
        name: Some(name.to_string()),
        kind: Some("link".into()),
        icon: Some("html/images/radio.png".into()),
        ..Default::default()
    };
    it.fields.insert("rb".into(), json!(rb));
    it
}

fn root_items() -> Vec<Item> {
    let mut search = link("Search", "search");
    search.kind = Some("search".into());
    vec![
        search,
        link("Popular", "stations/topclick"),
        link("Top Voted", "stations/topvote"),
        link("By Country", "countries"),
        link("By Genre", "tags"),
        link("By Language", "languages"),
    ]
}

/// A station as an audio item, or `None` for one without a stream.
fn station_item(st: &Value) -> Option<Item> {
    let name = st["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let url = st["url_resolved"]
        .as_str()
        .or(st["url"].as_str())
        .map(str::trim)
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
    let mut about: Vec<String> = Vec::new();
    if let Some(c) = st["country"].as_str().filter(|s| !s.is_empty()) {
        about.push(c.to_string());
    }
    let tags: Vec<&str> = st["tags"]
        .as_str()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(3)
        .collect();
    if !tags.is_empty() {
        about.push(tags.join(", "));
    }
    let codec = st["codec"]
        .as_str()
        .filter(|c| !c.is_empty() && *c != "UNKNOWN");
    match (codec, st["bitrate"].as_i64().filter(|b| *b > 0)) {
        (Some(c), Some(b)) => about.push(format!("{c} {b} kbps")),
        (Some(c), None) => about.push(c.to_string()),
        (None, Some(b)) => about.push(format!("{b} kbps")),
        _ => {}
    }
    let icon = st["favicon"]
        .as_str()
        .map(str::trim)
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    let mut it = Item {
        name: Some(name.to_string()),
        name2: (!about.is_empty()).then(|| about.join(" · ")),
        kind: Some("audio".into()),
        url: Some(url.to_string()),
        icon: Some(icon.unwrap_or("html/images/radio.png").to_string()),
        ..Default::default()
    };
    if let Some(i) = icon {
        it.image = Some(i.to_string());
    }
    if let Some(id) = st["stationuuid"].as_str() {
        it.fields.insert("stationuuid".into(), json!(id));
    }
    it.fields.insert("favorites_url".into(), json!(url));
    it.fields.insert("favorites_type".into(), json!("audio"));
    Some(it)
}

/// A country, genre or language: a link to its stations.
fn category_item(kind: &str, c: &Value) -> Option<Item> {
    let count = c["stationcount"].as_i64().unwrap_or(0);
    if count <= 0 {
        return None;
    }
    let name = c["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let (label, rb) = match kind {
        "countries" => {
            let code = c["iso_3166_1"].as_str().filter(|s| !s.is_empty())?;
            (
                name.to_string(),
                format!("stations/bycountrycodeexact/{code}"),
            )
        }
        "tags" => (
            title_case(name),
            format!("stations/bytagexact/{}", escape_search(name)),
        ),
        _ => (
            title_case(name),
            format!("stations/bylanguageexact/{}", escape_search(name)),
        ),
    };
    let mut it = link(&label, &rb);
    it.name2 = Some(format!("{count} stations"));
    Some(it)
}

/// `"classic rock"` → `"Classic Rock"` (tags and languages are lower case).
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const STATION_QUERY: &str = "hidebroken=true";

/// The API path listing a menu reference's children.
fn query_path(rb: &str) -> Option<String> {
    let path = match rb {
        "countries" => "countries?hidebroken=true".to_string(),
        "tags" => {
            format!("tags?order=stationcount&reverse=true&hidebroken=true&limit={CATEGORIES}")
        }
        "languages" => {
            format!("languages?order=stationcount&reverse=true&hidebroken=true&limit={CATEGORIES}")
        }
        "stations/topclick" | "stations/topvote" => {
            format!("{rb}?{STATION_QUERY}&limit={STATIONS}")
        }
        _ if rb.starts_with("stations/by") => {
            format!("{rb}?{STATION_QUERY}&order=clickcount&reverse=true&limit={STATIONS}")
        }
        _ => return None,
    };
    Some(path)
}

/// Children of a menu reference.
async fn children(http: &reqwest::Client, rb: &str) -> Result<Vec<Item>, String> {
    if rb == "root" {
        return Ok(root_items());
    }
    if let Some(term) = rb.strip_prefix("search:") {
        if term.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v = api(
            http,
            &format!(
                "stations/search?name={}&{STATION_QUERY}&order=clickcount&reverse=true&limit={STATIONS}",
                escape_search(term.trim())
            ),
        )
        .await?;
        return Ok(list(&v).iter().filter_map(station_item).collect());
    }
    let Some(path) = query_path(rb) else {
        return Ok(Vec::new());
    };
    let v = api(http, &path).await?;
    Ok(match rb {
        "countries" | "tags" | "languages" => {
            let mut items: Vec<Item> = list(&v)
                .iter()
                .filter_map(|c| category_item(rb, c))
                .collect();
            if rb == "countries" {
                items.sort_by_cached_key(|it| it.name.clone().unwrap_or_default().to_lowercase());
            }
            items
        }
        _ => list(&v).iter().filter_map(station_item).collect(),
    })
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The level an item id leads to.
async fn level(
    http: &reqwest::Client,
    path: &[String],
    search: Option<&str>,
) -> Result<Level, String> {
    let mut feed = Feed {
        title: Some(NAME.into()),
        items: root_items(),
        ..Default::default()
    };
    for (depth, comp) in path.iter().enumerate() {
        let i = component_index(comp).ok_or("bad item id")?;
        let item = feed.items.get(i).cloned().ok_or("no such item")?;
        let last = depth + 1 == path.len();
        if item.kind.as_deref() == Some("audio") {
            return Ok(Level::Leaf(Box::new(item), feed));
        }
        let mut rb = item
            .field("rb")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        if item.kind.as_deref() == Some("search") {
            let term = component_search(comp)
                .or_else(|| search.filter(|_| last).map(str::to_string))
                .unwrap_or_default();
            rb = format!("search:{term}");
        }
        let mut items = children(http, &rb).await?;
        if items.is_empty() {
            items.push(Item::text("No stations found"));
        }
        feed = Feed {
            title: item.name.clone(),
            items,
            parent: Some(Box::new(item)),
            ..Default::default()
        };
    }
    Ok(Level::List(feed))
}

/// Tell the directory a station is being played (its click count).
fn count_click(http: &reqwest::Client, level: &Level, index: Option<usize>) {
    let item = match level {
        Level::Leaf(item, _) => Some(item.as_ref()),
        Level::List(feed) => index.and_then(|i| feed.items.get(i)),
    };
    let Some(uuid) = item
        .and_then(|it| it.field("stationuuid"))
        .and_then(|v| v.as_str().map(str::to_string))
    else {
        return;
    };
    let http = http.clone();
    crate::util::spawn(async move {
        if let Err(e) = api(&http, &format!("url/{uuid}")).await {
            debug!("radio browser click: {e}");
        }
    });
}

/// `radiobrowser items|playlist`.
async fn items_query(server: Arc<Server>, mut r: Request) -> Request {
    crate::control::jive::fix_index_quantity(&mut r);
    let mut ctx = Ctx::from_request(&server, &r, TAG);
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    // keep a search term in the ids of the result levels (`0_term.3`)
    if let (Some(term), Some(last)) = (ctx.search.clone(), ctx.crumbs.last_mut()) {
        if component_search(last).is_none() && !term.is_empty() {
            *last = format!("{last}_{}", escape_search(&term));
        }
    }
    let level = match level(&server.http, &path, ctx.search.as_deref()).await {
        Ok(l) => l,
        Err(e) => Level::List(Feed {
            title: Some(NAME.into()),
            items: vec![Item::text(e)],
            ..Default::default()
        }),
    };
    match play {
        Some((method, pi)) => {
            count_click(&server.http, &level, pi);
            xmlbrowser::play(&server, &mut r, level, &method, pi).await
        }
        None => xmlbrowser::render(&ctx, level, &mut r),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::jive::xmlbrowser::{descend, render, results_loop};

    fn station() -> Value {
        json!({
            "stationuuid": "1cfb151d-a341-11e9-a787-52543be04c81", "name": " France Info ",
            "url": "http://direct.franceinfo.fr/live/franceinfo-midfi.m3u",
            "url_resolved": "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3",
            "favicon": "https://www.francetvinfo.fr/icon.png", "tags": "news,talk,public radio,french",
            "country": "France", "codec": "MP3", "bitrate": 128,
        })
    }

    #[test]
    fn station_items() {
        let it = station_item(&station()).unwrap();
        assert_eq!(it.name.as_deref(), Some("France Info"));
        assert_eq!(
            it.url.as_deref(),
            Some("http://direct.franceinfo.fr/live/franceinfo-midfi.mp3")
        );
        assert_eq!(
            it.name2.as_deref(),
            Some("France · news, talk, public radio · MP3 128 kbps")
        );
        assert_eq!(
            it.icon.as_deref(),
            Some("https://www.francetvinfo.fr/icon.png")
        );
        assert_eq!(
            it.field("stationuuid"),
            Some(json!("1cfb151d-a341-11e9-a787-52543be04c81"))
        );
        // no stream, no item; no favicon, the radio icon
        assert!(station_item(&json!({ "name": "X", "url": "", "url_resolved": "" })).is_none());
        let bare =
            station_item(&json!({ "name": "X", "url_resolved": "https://x/s", "favicon": "" }))
                .unwrap();
        assert_eq!(bare.icon.as_deref(), Some("html/images/radio.png"));
        assert_eq!(bare.name2, None);
    }

    #[test]
    fn categories() {
        let c = category_item(
            "countries",
            &json!({ "name": "France", "iso_3166_1": "FR", "stationcount": 1234 }),
        )
        .unwrap();
        assert_eq!(c.name.as_deref(), Some("France"));
        assert_eq!(c.name2.as_deref(), Some("1234 stations"));
        assert_eq!(c.field("rb"), Some(json!("stations/bycountrycodeexact/FR")));
        let t = category_item(
            "tags",
            &json!({ "name": "classic rock", "stationcount": 9 }),
        )
        .unwrap();
        assert_eq!(t.name.as_deref(), Some("Classic Rock"));
        assert_eq!(
            t.field("rb"),
            Some(json!("stations/bytagexact/classic%20rock"))
        );
        assert!(category_item("tags", &json!({ "name": "x", "stationcount": 0 })).is_none());
        assert_eq!(
            query_path("stations/bytagexact/jazz").unwrap(),
            "stations/bytagexact/jazz?hidebroken=true&order=clickcount&reverse=true&limit=300"
        );
        assert!(query_path("../servers").is_none());
    }

    #[test]
    fn renders_as_jive_menu() {
        let feed = Feed {
            title: Some(NAME.into()),
            items: root_items(),
            ..Default::default()
        };
        let ctx = Ctx {
            query: TAG.into(),
            terms: vec![TAG.into(), "items".into()],
            quantity: 100,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        };
        let mut r = Request::blank();
        render(&ctx, Level::List(feed), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Search");
        assert_eq!(items[0]["input"]["len"], 1);
        assert_eq!(items[1]["text"], "Popular");
        assert_eq!(items[1]["actions"]["go"]["cmd"], json!([TAG, "items"]));

        // a list of stations: playable, and favorites keep the stream address
        let stations = Feed {
            items: vec![station_item(&station()).unwrap()],
            ..Default::default()
        };
        let mut c2 = ctx.clone();
        c2.crumbs = vec!["1".into()];
        let mut r = Request::blank();
        render(&c2, descend(stations, &[], true).unwrap(), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(
            items[0]["text"],
            "France Info\nFrance · news, talk, public radio · MP3 128 kbps"
        );
        assert_eq!(items[0]["type"], "audio");
        assert_eq!(items[0]["params"]["item_id"], "1.0");
        assert_eq!(
            items[0]["presetParams"]["favorites_url"],
            "http://direct.franceinfo.fr/live/franceinfo-midfi.mp3"
        );
    }
}
