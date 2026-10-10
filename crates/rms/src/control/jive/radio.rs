//! Internet radio: the `radios` query (`dynamicAutoQuery` over the radio providers, as
//! `Slim::Plugin::OPMLBased::cliRadiosQuery` builds it) and the providers themselves.
//!
//! LMS's TuneIn directory needs a partner API, so RMS ships two small OPML-style providers
//! instead, each answering `<tag> items` / `<tag> playlist <method>` through
//! [`super::xmlbrowser`]:
//!
//! * `radiofavorites` — the user's favorites that are internet streams;
//! * `radiodirectory` — a built-in directory of well-known public streams ([`DIRECTORY`]).
//!
//! More providers can be added to [`providers`]; more stations to [`DIRECTORY`].

use super::xmlbrowser::{self, descend, split_item_id, Ctx, Feed, Item, Level};
use crate::control::dispatch::{DispatchTable, Handler};
use crate::control::request::{normalize, Request};
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use std::sync::Arc;

/// One station of the built-in directory.
pub struct Station {
    pub name: &'static str,
    pub url: &'static str,
    pub icon: Option<&'static str>,
    pub description: &'static str,
}

/// A directory folder.
pub struct Category {
    pub name: &'static str,
    pub stations: &'static [Station],
}

/// The built-in station directory.
pub const DIRECTORY: &[Category] = &[
    Category {
        name: "SomaFM",
        stations: &[
            Station { name: "Groove Salad", url: "http://ice2.somafm.com/groovesalad-128-mp3", icon: Some("https://somafm.com/img/groovesalad120.png"), description: "A nicely chilled plate of ambient/downtempo beats and grooves." },
            Station { name: "Drone Zone", url: "http://ice2.somafm.com/dronezone-128-mp3", icon: Some("https://somafm.com/img/dronezone120.jpg"), description: "Served best chilled, safe with most medications. Atmospheric textures with minimal beats." },
            Station { name: "Secret Agent", url: "http://ice2.somafm.com/secretagent-128-mp3", icon: Some("https://somafm.com/img/secretagent120.jpg"), description: "The soundtrack for your stylish, mysterious, dangerous life." },
        ],
    },
    Category {
        name: "Radio Paradise",
        stations: &[
            Station { name: "Radio Paradise Main Mix", url: "http://stream.radioparadise.com/mp3-192", icon: None, description: "Eclectic, listener-supported, commercial-free internet radio." },
            Station { name: "Radio Paradise Mellow Mix", url: "http://stream.radioparadise.com/mellow-192", icon: None, description: "A mellower selection from the Radio Paradise library." },
            Station { name: "Radio Paradise Rock Mix", url: "http://stream.radioparadise.com/rock-192", icon: None, description: "The rockier side of Radio Paradise." },
        ],
    },
    Category {
        name: "News",
        stations: &[Station { name: "BBC World Service", url: "http://stream.live.vc.bbcmedia.co.uk/bbc_world_service", icon: None, description: "International news, analysis and information from the BBC." }],
    },
    Category {
        name: "Public Radio",
        stations: &[Station { name: "KEXP 90.3 FM Seattle", url: "https://kexp-mp3-128.streamguys1.com/kexp128.mp3", icon: None, description: "Where the music matters." }],
    },
];

/// A radio provider (an OPML-based "plugin" in LMS terms).
pub struct Provider {
    pub tag: &'static str,
    pub name: &'static str,
    pub weight: i64,
    pub icon: &'static str,
    /// Builds the provider's feed.
    pub feed: fn(&Server) -> Feed,
    /// Whether the provider is shown (`condition`).
    pub condition: fn(&Server) -> bool,
}

/// All radio providers.
pub fn providers() -> Vec<Provider> {
    vec![
        Provider {
            tag: "radiofavorites",
            name: "Favorite Stations",
            weight: 10,
            icon: "html/images/favorites.png",
            feed: favorites_feed,
            condition: |s| !radio_favorites(s).is_empty(),
        },
        Provider {
            tag: "radiodirectory",
            name: "Popular Stations",
            weight: 20,
            icon: "html/images/radio.png",
            feed: directory_feed,
            condition: |_| true,
        },
    ]
}

/// `Slim::Plugin::OPMLBased::initJive` home items (`node => 'radios'`).
pub fn plugin_menus(server: &Server) -> Vec<Value> {
    providers()
        .into_iter()
        .filter(|p| (p.condition)(server))
        .map(|p| {
            json!({
                "text": p.name, "id": format!("opml{}", p.tag), "uuid": p.tag, "node": "radios", "weight": p.weight, "displayWhenOff": 0,
                "window": { "icon-id": p.icon, "titleStyle": "album" },
                "actions": { "go": { "player": 0, "cmd": [p.tag, "items"], "params": { "menu": p.tag } } },
            })
        })
        .collect()
}

pub fn register(t: &mut DispatchTable) {
    t.add(
        &["radios", "_index", "_quantity"],
        0,
        true,
        true,
        Handler::Sync(radios_query),
    );
    for p in providers() {
        t.add(
            &[p.tag, "items", "_index", "_quantity"],
            0,
            true,
            true,
            Handler::Async(|s, r| Box::pin(provider_query(s, r))),
        );
        t.add(
            &[p.tag, "playlist", "_method"],
            1,
            true,
            true,
            Handler::Async(|s, r| Box::pin(provider_query(s, r))),
        );
    }
}

/// A station as an audio item.
fn station_item(name: &str, url: &str, icon: Option<&str>, description: Option<&str>) -> Item {
    let mut it = Item {
        name: Some(name.to_string()),
        kind: Some("audio".into()),
        url: Some(url.to_string()),
        ..Default::default()
    };
    it.icon = Some(icon.unwrap_or("html/images/radio.png").to_string());
    if let Some(d) = description.filter(|d| !d.is_empty()) {
        it.fields.insert("description".into(), json!(d));
    }
    it
}

/// `radiodirectory`: categories of built-in stations.
pub fn directory_feed(_: &Server) -> Feed {
    let items = DIRECTORY
        .iter()
        .map(|c| Item {
            name: Some(c.name.to_string()),
            kind: Some("link".into()),
            icon: Some("html/images/radio.png".into()),
            items: Some(
                c.stations
                    .iter()
                    .map(|st| station_item(st.name, st.url, st.icon, Some(st.description)))
                    .collect(),
            ),
            ..Default::default()
        })
        .collect();
    Feed {
        title: Some("Popular Stations".into()),
        items,
        ..Default::default()
    }
}

/// Favorites that are internet streams, flattened `(title, url, icon)`.
fn radio_favorites(server: &Server) -> Vec<(String, String, Option<String>)> {
    fn walk(
        items: &[crate::plugins::favorites::Fav],
        out: &mut Vec<(String, String, Option<String>)>,
    ) {
        for f in items {
            if let Some(u) = &f.url {
                if crate::util::is_remote_url(u) {
                    out.push((f.text.clone(), u.clone(), f.icon.clone()));
                }
            }
            walk(&f.children, out);
        }
    }
    let mut out = Vec::new();
    walk(&server.favorites.all(), &mut out);
    out
}

/// `radiofavorites`: the user's radio favorites.
pub fn favorites_feed(server: &Server) -> Feed {
    let items = radio_favorites(server)
        .into_iter()
        .map(|(t, u, i)| station_item(&t, &u, i.as_deref(), None))
        .collect();
    Feed {
        title: Some("Favorite Stations".into()),
        items,
        ..Default::default()
    }
}

/// `radios <start> <count> [menu:radio] [sort:weight]` (`dynamicAutoQuery`).
pub fn radios_query(server: &Arc<Server>, r: &mut Request) {
    let menu = r.param("menu").is_some();
    let quantity = r.param_i64("_quantity").unwrap_or(0);
    if quantity == 0 {
        return r.set_done();
    }
    let mut list: Vec<Provider> = providers()
        .into_iter()
        .filter(|p| (p.condition)(server))
        .collect();
    list.sort_by_key(|p| p.weight);
    let entries: Vec<Value> = list
        .iter()
        .map(|p| {
            if menu {
                json!({ "text": p.name, "weight": p.weight, "icon-id": p.icon,
                    "actions": { "go": { "cmd": [p.tag, "items"], "params": { "menu": p.tag } } }, "window": { "titleStyle": "album" } })
            } else {
                json!({ "cmd": p.tag, "name": p.name, "type": "xmlbrowser", "icon": p.icon, "weight": p.weight })
            }
        })
        .collect();
    let index = r.param_i64("_index").unwrap_or(0);
    r.set_result_first("count", entries.len());
    let (valid, start, end) = normalize(Some(index), Some(quantity), entries.len());
    let loop_name = if menu { "item_loop" } else { "radioss_loop" };
    if valid {
        for e in entries.into_iter().take(end + 1).skip(start) {
            if let Value::Object(m) = e {
                r.add_loop_item(loop_name, m);
            }
        }
    }
    if menu {
        r.add_result("offset", index);
    }
    let _ = s;
    r.set_done();
}

/// `<tag> items|playlist` for a radio provider.
async fn provider_query(server: Arc<Server>, mut r: Request) -> Request {
    super::fix_index_quantity(&mut r);
    let tag = r.term(0).to_string();
    let Some(provider) = providers().into_iter().find(|p| p.tag == tag) else {
        r.bad_dispatch();
        return r;
    };
    let mut ctx = Ctx::from_request(&server, &r, &tag);
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let feed = (provider.feed)(&server);
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    let level = descend(feed, &path, play.is_none()).unwrap_or(Level::List(Feed::default()));
    match play {
        Some((method, pi)) => xmlbrowser::play(&server, &mut r, level, &method, pi).await,
        None => xmlbrowser::render(&ctx, level, &mut r),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::jive::xmlbrowser::{render, results_loop};

    #[test]
    fn directory_has_requested_streams() {
        let urls: Vec<&str> = DIRECTORY
            .iter()
            .flat_map(|c| c.stations.iter().map(|s| s.url))
            .collect();
        for u in [
            "http://ice2.somafm.com/groovesalad-128-mp3",
            "http://stream.radioparadise.com/mp3-192",
            "http://stream.live.vc.bbcmedia.co.uk/bbc_world_service",
            "https://kexp-mp3-128.streamguys1.com/kexp128.mp3",
        ] {
            assert!(urls.contains(&u), "{}", u);
        }
    }

    #[test]
    fn directory_browse_and_stations() {
        let feed = Feed {
            items: DIRECTORY
                .iter()
                .map(|c| Item {
                    name: Some(c.name.into()),
                    kind: Some("link".into()),
                    items: Some(
                        c.stations
                            .iter()
                            .map(|st| station_item(st.name, st.url, st.icon, Some(st.description)))
                            .collect(),
                    ),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let ctx = Ctx {
            query: "radiodirectory".into(),
            terms: vec!["radiodirectory".into(), "items".into()],
            quantity: 100,
            menu_mode: true,
            playalbum: true,
            ..Default::default()
        };
        let mut r = Request::blank();
        render(&ctx, Level::List(feed.clone()), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "SomaFM");
        assert_eq!(
            items[0]["actions"]["go"]["cmd"],
            json!(["radiodirectory", "items"])
        );
        assert_eq!(items[0]["actions"]["go"]["params"]["item_id"], "0");
        // drill into SomaFM
        let mut c2 = ctx.clone();
        c2.crumbs = vec!["0".into()];
        let mut r = Request::blank();
        render(&c2, descend(feed, &["0".into()], true).unwrap(), &mut r);
        let items = results_loop(&r, "item_loop");
        assert_eq!(items[0]["text"], "Groove Salad");
        assert_eq!(items[0]["type"], "audio");
        assert_eq!(
            items[0]["icon"],
            "/imageproxy/https%3A%2F%2Fsomafm.com%2Fimg%2Fgroovesalad120.png/image.png"
        );
        assert_eq!(items[0]["params"]["item_id"], "0.0");
        assert_eq!(
            items[0]["presetParams"]["favorites_url"],
            "http://ice2.somafm.com/groovesalad-128-mp3"
        );
        let base = r.result("base").unwrap();
        assert_eq!(
            base["actions"]["go"]["cmd"],
            json!(["radiodirectory", "playlist", "play"])
        );
        assert_eq!(r.result("title"), Some(&json!("SomaFM")));
    }
}
