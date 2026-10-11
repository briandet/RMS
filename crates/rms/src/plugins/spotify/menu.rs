//! Spotify menus (`spotify items|playlist`) and account commands (`spotify login|logout|status`).
//!
//! Levels are built on demand from the Web API and rendered by the XMLBrowser like any other
//! OPML feed, so they work in Squeezer/SqueezePlay, the web UI and on the player's screen.
//! Item ids are index paths; each level is fetched again (from a short cache) to descend.

use super::{best_image, url_for, Spotify};
use crate::control::dispatch::{DispatchTable, Handler};
use crate::control::jive::xmlbrowser::{
    self, component_index, component_search, split_item_id, Ctx, Feed, Item, Level,
};
use crate::control::request::Request;
use crate::server::Server;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn register(t: &mut DispatchTable) {
    t.add(
        &["spotify", "items", "_index", "_quantity"],
        0,
        true,
        true,
        Handler::Async(|s, r| Box::pin(items_query(s, r))),
    );
    t.add(
        &["spotify", "playlist", "_method"],
        1,
        true,
        true,
        Handler::Async(|s, r| Box::pin(items_query(s, r))),
    );
    t.add(
        &["spotify", "login"],
        0,
        false,
        true,
        Handler::Async(|s, r| Box::pin(login(s, r))),
    );
    t.add(
        &["spotify", "logout"],
        0,
        false,
        true,
        Handler::Async(|s, r| Box::pin(logout(s, r))),
    );
    t.add(
        &["spotify", "status"],
        0,
        true,
        true,
        Handler::Async(|s, r| Box::pin(status(s, r))),
    );
}

/// The home menu entry (Spotty's place: the home menu).
pub fn home_menu() -> Value {
    json!({
        "text": "Spotify", "id": "spotify", "node": "home", "weight": 45,
        "window": { "titleStyle": "album", "icon-id": "html/images/spotify.png" },
        "actions": { "go": { "player": 0, "cmd": ["spotify", "items"], "params": { "menu": "spotify" } } },
    })
}

fn link(name: &str, sp: &str, image: Option<String>) -> Item {
    let mut it = Item {
        name: Some(name.to_string()),
        kind: Some("link".into()),
        image,
        ..Default::default()
    };
    it.fields.insert("sp".into(), json!(sp));
    it
}

/// An album / playlist / artist mix: browsable, and playable as a whole.
fn container(name: &str, sub: Option<String>, kind: &str, id: &str, image: Option<String>) -> Item {
    let url = url_for(kind, id);
    let mut it = Item {
        name: Some(name.to_string()),
        name2: sub,
        kind: Some("playlist".into()),
        play: Some(url.clone()),
        image,
        ..Default::default()
    };
    it.fields.insert("sp".into(), json!(format!("{kind}:{id}")));
    it.fields.insert("favorites_url".into(), json!(url));
    it.fields.insert("favorites_type".into(), json!("playlist"));
    it
}

fn track_item(sp: &Spotify, t: &Value) -> Option<Item> {
    let id = t["id"].as_str()?;
    if t["is_local"].as_bool().unwrap_or(false) {
        return None;
    }
    sp.remember(t);
    let m = super::meta_from_track(t);
    let mut it = Item {
        name: Some(m.title.clone()),
        name2: Some(
            [m.artist.clone(), m.album.clone()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" - "),
        ),
        kind: Some("audio".into()),
        url: Some(url_for("track", id)),
        image: m.image.clone(),
        playall: true,
        ..Default::default()
    };
    it.fields
        .insert("favorites_url".into(), json!(url_for("track", id)));
    Some(it)
}

fn album_item(a: &Value) -> Option<Item> {
    let artists: Vec<&str> = a["artists"]
        .as_array()
        .map(|x| x.iter().filter_map(|y| y["name"].as_str()).collect())
        .unwrap_or_default();
    Some(container(
        a["name"].as_str()?,
        Some(artists.join(", ")),
        "album",
        a["id"].as_str()?,
        best_image(&a["images"]),
    ))
}

fn playlist_item(p: &Value) -> Option<Item> {
    let owner = p["owner"]["display_name"].as_str().map(str::to_string);
    Some(container(
        p["name"].as_str()?,
        owner,
        "playlist",
        p["id"].as_str()?,
        best_image(&p["images"]),
    ))
}

fn artist_item(a: &Value) -> Option<Item> {
    let mut it = link(
        a["name"].as_str()?,
        &format!("artist:{}", a["id"].as_str()?),
        best_image(&a["images"]),
    );
    it.fields.insert(
        "favorites_url".into(),
        json!(url_for("artist", a["id"].as_str()?)),
    );
    it.fields.insert("favorites_type".into(), json!("playlist"));
    it.play = Some(url_for("artist", a["id"].as_str()?));
    Some(it)
}

fn root_items() -> Vec<Item> {
    let mut search = link("Search", "search", None);
    search.kind = Some("search".into());
    let mut liked = container("Liked Songs", None, "collection", "", None);
    liked.fields.insert("sp".into(), json!("liked"));
    vec![
        search,
        liked,
        link("Playlists", "playlists", None),
        link("Albums", "albums", None),
        link("Artists", "artists", None),
        link("Recently Played", "recent", None),
        link("Top Tracks", "toptracks", None),
        link("Top Artists", "topartists", None),
        link("New Releases", "new", None),
    ]
}

impl Spotify {
    /// Children of a menu reference.
    async fn children(&self, sp: &str) -> Result<Vec<Item>, String> {
        let tracks =
            |v: Vec<Value>| -> Vec<Item> { v.iter().filter_map(|t| track_item(self, t)).collect() };
        let (kind, arg) = sp.split_once(':').unwrap_or((sp, ""));
        Ok(match kind {
            "root" => root_items(),
            "liked" => tracks(
                self.api_all("/me/tracks?limit=50", 2000)
                    .await?
                    .into_iter()
                    .map(|i| i["track"].clone())
                    .collect(),
            ),
            "playlists" => self
                .api_all("/me/playlists?limit=50", 1000)
                .await?
                .iter()
                .filter_map(playlist_item)
                .collect(),
            "albums" => self
                .api_all("/me/albums?limit=50", 1000)
                .await?
                .iter()
                .filter_map(|i| album_item(&i["album"]))
                .collect(),
            "artists" => self
                .api_all("/me/following?type=artist&limit=50", 1000)
                .await?
                .iter()
                .filter_map(artist_item)
                .collect(),
            "recent" => tracks(
                self.api_all("/me/player/recently-played?limit=50", 50)
                    .await?
                    .into_iter()
                    .map(|i| i["track"].clone())
                    .collect(),
            ),
            "toptracks" => tracks(self.api_all("/me/top/tracks?limit=50", 100).await?),
            "topartists" => self
                .api_all("/me/top/artists?limit=50", 100)
                .await?
                .iter()
                .filter_map(artist_item)
                .collect(),
            "new" => self
                .api_all("/browse/new-releases?limit=50", 100)
                .await?
                .iter()
                .filter_map(album_item)
                .collect(),
            "album" | "playlist" => tracks(self.tracks(&url_for(kind, arg)).await?),
            "artist" => vec![
                {
                    let mut top = container("Top Tracks", None, "artist", arg, None);
                    top.fields
                        .insert("sp".into(), json!(format!("artisttop:{arg}")));
                    top
                },
                link("Albums", &format!("artistalbums:{arg}"), None),
            ],
            "artisttop" => tracks(
                self.api(&format!("/artists/{arg}/top-tracks?market=from_token"))
                    .await?["tracks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            ),
            "artistalbums" => self
                .api_all(
                    &format!(
                        "/artists/{arg}/albums?include_groups=album,single,compilation&limit=50"
                    ),
                    500,
                )
                .await?
                .iter()
                .filter_map(album_item)
                .collect(),
            "search" if !arg.is_empty() => vec![
                link("Tracks", &format!("searchtrack:{arg}"), None),
                link("Albums", &format!("searchalbum:{arg}"), None),
                link("Artists", &format!("searchartist:{arg}"), None),
                link("Playlists", &format!("searchplaylist:{arg}"), None),
            ],
            k if k.starts_with("search") && !arg.is_empty() => {
                let t = &k["search".len()..];
                let r = self
                    .api(&format!(
                        "/search?type={t}&limit=50&q={}",
                        xmlbrowser::escape_search(arg)
                    ))
                    .await?;
                let list = r[format!("{t}s")]["items"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                match t {
                    "track" => tracks(list),
                    "album" => list.iter().filter_map(album_item).collect(),
                    "artist" => list.iter().filter_map(artist_item).collect(),
                    _ => list
                        .iter()
                        .filter(|p| !p.is_null())
                        .filter_map(playlist_item)
                        .collect(),
                }
            }
            _ => Vec::new(),
        })
    }

    /// The level an item id leads to (`descend` for this dynamic tree).
    async fn level(&self, path: &[String], search: Option<&str>) -> Result<Level, String> {
        let mut feed = Feed {
            title: Some("Spotify".into()),
            items: self.children("root").await?,
            ..Default::default()
        };
        for (depth, comp) in path.iter().enumerate() {
            let i = component_index(comp).ok_or("bad item id")?;
            let item = feed.items.get(i).cloned().ok_or("no such item")?;
            let last = depth + 1 == path.len();
            if item.kind.as_deref() == Some("audio") {
                // a track: its info, or the track to play
                return Ok(Level::Leaf(Box::new(item), feed));
            }
            let mut sp = item
                .field("sp")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            if item.kind.as_deref() == Some("search") {
                let term = component_search(comp)
                    .or_else(|| search.filter(|_| last).map(str::to_string))
                    .unwrap_or_default();
                sp = format!("search:{term}");
            }
            feed = Feed {
                title: item.name.clone(),
                items: self.children(&sp).await?,
                parent: Some(Box::new(item)),
                ..Default::default()
            };
        }
        Ok(Level::List(feed))
    }
}

fn message_level(text: &str) -> Level {
    Level::List(Feed {
        title: Some("Spotify".into()),
        items: vec![Item::text(text)],
        ..Default::default()
    })
}

/// `spotify items|playlist`.
async fn items_query(server: Arc<Server>, mut r: Request) -> Request {
    crate::control::jive::fix_index_quantity(&mut r);
    let mut ctx = Ctx::from_request(&server, &r, "spotify");
    let play = xmlbrowser::prepare_play(&mut ctx, &mut r);
    let path = split_item_id(ctx.item_id.as_deref().unwrap_or(""));
    ctx.crumbs = path.clone();
    // keep a search term in the ids of the result levels (`3_term.0`)
    if let (Some(term), Some(last)) = (ctx.search.clone(), ctx.crumbs.last_mut()) {
        if component_search(last).is_none() && !term.is_empty() {
            *last = format!("{last}_{}", xmlbrowser::escape_search(&term));
        }
    }
    let level = match super::get() {
        Some(sp) if sp.signed_in() => match sp.level(&path, ctx.search.as_deref()).await {
            Ok(l) => l,
            Err(e) => message_level(&e),
        },
        _ => message_level("Sign in to Spotify in the web interface: Settings → Spotify."),
    };
    match play {
        Some((method, pi)) => xmlbrowser::play(&server, &mut r, level, &method, pi).await,
        None => xmlbrowser::render(&ctx, level, &mut r),
    }
    r
}

/// `spotify login` → `url` to open; `spotify login code:<redirected address>` → `user`.
async fn login(server: Arc<Server>, mut r: Request) -> Request {
    let Some(sp) = super::get() else {
        r.bad_dispatch();
        return r;
    };
    match r.param("code").map(str::to_string) {
        None => {
            let id = super::client_id_or_default(&server.prefs.server.get_str("spotifyClientId"));
            let url = sp.start_login(&id);
            r.add_result("url", url);
            r.add_result("redirect_uri", super::redirect_uri());
        }
        Some(code) => match sp.finish_login(&code).await {
            Ok(user) => r.add_result("user", user),
            Err(e) => r.add_result("error", e),
        },
    }
    r.set_done();
    r
}

async fn logout(_server: Arc<Server>, mut r: Request) -> Request {
    if let Some(sp) = super::get() {
        sp.logout().await;
    }
    r.set_done();
    r
}

/// `spotify status`: `signed_in`, `user`, `product` (from the Web API profile).
async fn status(_server: Arc<Server>, mut r: Request) -> Request {
    let Some(sp) = super::get() else {
        r.bad_dispatch();
        return r;
    };
    let signed_in = sp.signed_in();
    r.add_result("signed_in", signed_in as i32);
    if signed_in {
        match sp.api("/me").await {
            Ok(me) => {
                r.add_result(
                    "user",
                    me["display_name"]
                        .as_str()
                        .or(me["id"].as_str())
                        .unwrap_or(""),
                );
                r.add_result("product", me["product"].as_str().unwrap_or(""));
            }
            Err(e) => {
                if let Some(u) = sp.username() {
                    r.add_result("user", u);
                }
                r.add_result("error", e);
            }
        }
    }
    r.set_done();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_from_web_api_objects() {
        let sp = crate::plugins::spotify::init(
            &std::env::temp_dir().join("rms-sp-test"),
            reqwest::Client::new(),
        );
        let t = json!({ "id": "t1", "name": "Song", "duration_ms": 1000, "artists": [{ "name": "A" }], "album": { "name": "Al", "images": [] } });
        let it = track_item(&sp, &t).unwrap();
        assert_eq!(it.url.as_deref(), Some("spotify://track:t1"));
        assert_eq!(it.name2.as_deref(), Some("A - Al"));
        assert!(sp.meta.contains_key("spotify://track:t1"));
        let a = album_item(
            &json!({ "id": "a1", "name": "Album", "artists": [{ "name": "X" }], "images": [] }),
        )
        .unwrap();
        assert_eq!(a.play.as_deref(), Some("spotify://album:a1"));
        assert_eq!(a.field("sp"), Some(json!("album:a1")));
        assert_eq!(root_items()[0].kind.as_deref(), Some("search"));
    }
}
