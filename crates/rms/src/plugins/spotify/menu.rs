//! Spotify menus (`spotify items|playlist`) and account commands (`spotify login|logout|status`).
//!
//! Levels are built on demand ([`super::browse`], [`super::pathfinder`]) and rendered by the XMLBrowser like any other
//! OPML feed, so they work in Squeezer/SqueezePlay, the web UI and on the player's screen.
//! Item ids are index paths; each level is fetched again (from a short cache) to descend.

use super::model::{Album, Artist, Context, Found, Playlist, SearchKind, Track};
use super::{url_for, Spotify};
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

fn track_item(sp: &Spotify, t: &Track) -> Item {
    sp.remember(t);
    let mut it = Item {
        name: Some(t.name.clone()),
        name2: Some(
            [t.artist_names(), t.album.clone()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" - "),
        ),
        kind: Some("audio".into()),
        url: Some(t.url()),
        image: t.cover.clone(),
        playall: true,
        ..Default::default()
    };
    it.fields.insert("favorites_url".into(), json!(t.url()));
    it
}

fn album_item(a: &Album) -> Item {
    container(
        &a.name,
        Some(a.artist_names()),
        "album",
        &a.id,
        a.cover.clone(),
    )
}

fn playlist_item(p: &Playlist) -> Item {
    container(
        &p.name,
        p.owner.clone(),
        "playlist",
        &p.id,
        p.picture.clone(),
    )
}

fn artist_item(a: &Artist) -> Item {
    let mut it = link(&a.name, &format!("artist:{}", a.id), a.picture.clone());
    let url = url_for("artist", &a.id);
    it.fields.insert("favorites_url".into(), json!(url));
    it.fields.insert("favorites_type".into(), json!("playlist"));
    it.play = Some(url);
    it
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
        link("Release Radar", "releaseradar", None),
        link("Top Tracks", "toptracks", None),
        link("Top Artists", "topartists", None),
        link("New Releases", "new", None),
    ]
}

/// Search results, at most.
const SEARCH_LIMIT: usize = 50;

impl Spotify {
    /// Children of a menu reference.
    async fn children(&self, sp: &str) -> Result<Vec<Item>, String> {
        let tracks =
            |v: Vec<Track>| -> Vec<Item> { v.iter().map(|t| track_item(self, t)).collect() };
        let albums = |v: Vec<Album>| -> Vec<Item> { v.iter().map(album_item).collect() };
        let artists = |v: Vec<Artist>| -> Vec<Item> { v.iter().map(artist_item).collect() };
        let (kind, arg) = sp.split_once(':').unwrap_or((sp, ""));
        Ok(match kind {
            "root" => root_items(),
            "liked" => tracks(self.liked().await?),
            "playlists" => self.playlists().await?.iter().map(playlist_item).collect(),
            "albums" => albums(self.saved_albums().await?),
            "artists" => artists(self.followed_artists().await?),
            "recent" => self
                .recently_played()
                .await?
                .iter()
                .map(|c| match c {
                    Context::Album(a) => album_item(a),
                    Context::Playlist(p) => playlist_item(p),
                    Context::Artist(a) => artist_item(a),
                })
                .collect(),
            "releaseradar" => tracks(self.playlist_tracks(&self.release_radar().await?).await?),
            "toptracks" => tracks(self.top_tracks().await?),
            "topartists" => artists(self.top_artists().await?),
            "new" => albums(self.new_releases().await?),
            "album" => tracks(self.album_tracks(arg).await?),
            "playlist" => tracks(self.playlist_tracks(arg).await?),
            "artist" => vec![
                {
                    let mut top = container("Top Tracks", None, "artist", arg, None);
                    top.fields
                        .insert("sp".into(), json!(format!("artisttop:{arg}")));
                    top
                },
                link("Albums", &format!("artistalbums:{arg}"), None),
            ],
            "artisttop" => tracks(self.artist_top_tracks(arg).await?),
            "artistalbums" => albums(self.artist_albums(arg).await?),
            "search" if !arg.is_empty() => SearchKind::ALL
                .iter()
                .map(|k| {
                    let name = match k {
                        SearchKind::Track => "Tracks",
                        SearchKind::Album => "Albums",
                        SearchKind::Artist => "Artists",
                        SearchKind::Playlist => "Playlists",
                    };
                    link(name, &format!("search{}:{arg}", k.name()), None)
                })
                .collect(),
            k if !arg.is_empty() => {
                let Some(kind) = k.strip_prefix("search").and_then(SearchKind::from_name) else {
                    return Ok(Vec::new());
                };
                match self.search(kind, arg, SEARCH_LIMIT).await? {
                    Found::Tracks(t) => tracks(t),
                    Found::Albums(a) => albums(a),
                    Found::Artists(a) => artists(a),
                    Found::Playlists(p) => p.iter().map(playlist_item).collect(),
                }
            }
            _ => Vec::new(),
        })
    }

    /// The level an item id leads to (`descend` for this dynamic tree).  An id may start at
    /// an album or artist instead of the root (`album:<id>.3`): what now playing links to.
    async fn level(&self, path: &[String], search: Option<&str>) -> Result<Level, String> {
        let start = path
            .split_first()
            .and_then(|(first, rest)| Some((first, direct(first)?, rest)));
        let (mut feed, path) = match start {
            Some((first, (kind, id), rest)) => {
                let feed = Feed {
                    items: self.children(first).await?,
                    parent: Some(Box::new(container("", None, kind, id, None))),
                    ..Default::default()
                };
                (feed, rest)
            }
            None => {
                let feed = Feed {
                    title: Some("Spotify".into()),
                    items: self.children("root").await?,
                    ..Default::default()
                };
                (feed, path)
            }
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

/// An item id component naming an album or artist (`album:<id>`): its kind and id.
fn direct(comp: &str) -> Option<(&str, &str)> {
    let (kind, id) = comp.split_once(':')?;
    (matches!(kind, "album" | "artist") && !id.is_empty()).then_some((kind, id))
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
async fn login(_server: Arc<Server>, mut r: Request) -> Request {
    let Some(sp) = super::get() else {
        r.bad_dispatch();
        return r;
    };
    match r.param("code").map(str::to_string) {
        None => {
            r.add_result("url", sp.start_login());
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

/// `spotify status`: `signed_in`, `user`, `product` (`premium`, …).
async fn status(_server: Arc<Server>, mut r: Request) -> Request {
    let Some(sp) = super::get() else {
        r.bad_dispatch();
        return r;
    };
    let signed_in = sp.signed_in();
    r.add_result("signed_in", signed_in as i32);
    if let Some(e) = super::connect::error() {
        r.add_result("connect_error", e);
    }
    if signed_in {
        match sp.account().await {
            Ok((user, product)) => {
                r.add_result("user", user);
                if let Some(p) = product {
                    r.add_result("product", p);
                }
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
    fn ids_may_start_at_an_album_or_artist() {
        assert_eq!(direct("album:a1"), Some(("album", "a1")));
        assert_eq!(direct("artist:b2"), Some(("artist", "b2")));
        assert_eq!(direct("playlist:c3"), None);
        assert_eq!(direct("album:"), None);
        assert_eq!(direct("3_term"), None);
        assert_eq!(
            split_item_id("album:a1.2"),
            vec!["album:a1".to_string(), "2".to_string()]
        );
    }

    #[test]
    fn items_from_tracks_and_albums() {
        let sp = crate::plugins::spotify::init(
            &std::env::temp_dir().join("rms-sp-test"),
            reqwest::Client::new(),
        );
        let t = Track {
            id: "t1".into(),
            name: "Song".into(),
            artists: vec![super::super::model::ArtistRef {
                id: "a".into(),
                name: "A".into(),
            }],
            album: "Al".into(),
            duration_ms: 1000,
            ..Default::default()
        };
        let it = track_item(&sp, &t);
        assert_eq!(it.url.as_deref(), Some("spotify://track:t1"));
        assert_eq!(it.name2.as_deref(), Some("A - Al"));
        assert!(sp.meta.contains_key("spotify://track:t1"));
        let a = album_item(&Album {
            id: "a1".into(),
            name: "Album".into(),
            ..Default::default()
        });
        assert_eq!(a.play.as_deref(), Some("spotify://album:a1"));
        assert_eq!(a.field("sp"), Some(json!("album:a1")));
        assert_eq!(root_items()[0].kind.as_deref(), Some("search"));
    }
}
