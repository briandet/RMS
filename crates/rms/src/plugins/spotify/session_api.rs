//! Browsing without the Web API: the Web API paths this plugin uses, answered through the
//! librespot session (Spotify's own client endpoints, as the official apps use them).
//!
//! The Web API needs a token for a registered client id; the shared desktop id is
//! rate-limited by Spotify to the point of being unusable, and a per-user app is too much
//! to ask.  The session needs nothing beyond signing in, so [`Spotify::api`] falls back to
//! [`session_get`] when the Web API fails.  Results have the Web API's JSON shape (only the
//! fields this plugin reads), so the menus work unchanged.  Saved albums and followed artists
//! come from the collection service the apps sync with.  Not available this way: search,
//! top tracks/artists, new releases.

use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Entities per extended-metadata request.
const BATCH: usize = 200;

/// Answer a Web API `path` (as passed to [`super::Spotify::api`]) through the session.
pub async fn session_get(session: &Session, path: &str) -> Result<Value, String> {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let parts: Vec<&str> = route.trim_matches('/').split('/').collect();
    let unsupported = || Err(format!("{route} needs the Spotify Web API"));
    match parts.as_slice() {
        ["me"] => {
            let user = session.username();
            Ok(json!({ "id": user, "display_name": user }))
        }
        ["tracks", id] => {
            let mut t = tracks(session, &[format!("spotify:track:{id}")]).await?;
            t.pop().ok_or_else(|| "no such track".to_string())
        }
        ["albums", id] => album(session, id).await,
        ["playlists", id, "tracks"] => {
            let uri = uri(&format!("spotify:playlist:{id}"))?;
            let pl = Playlist::get(session, &uri)
                .await
                .map_err(|e| e.to_string())?;
            let ids: Vec<String> = pl.tracks().filter_map(|u| u.to_uri().ok()).collect();
            page(tracks(session, &ids).await?, |t| json!({ "track": t }))
        }
        ["me", "tracks"] => {
            let user = session.username();
            let ids = context_tracks(session, &format!("spotify:user:{user}:collection")).await?;
            page(tracks(session, &ids).await?, |t| json!({ "track": t }))
        }
        ["me", "playlists"] => playlists(session).await,
        ["me", "albums"] => {
            let ids: Vec<String> = collection(session, "collection")
                .await?
                .into_iter()
                .filter(|u| u.starts_with("spotify:album:"))
                .collect();
            page(albums(session, &ids).await?, |a| json!({ "album": a }))
        }
        ["me", "following"] => {
            let ids = collection(session, "artist").await?;
            let items = artists(session, &ids).await?;
            Ok(json!({ "artists": { "items": items, "next": null } }))
        }
        ["artists", id, "top-tracks"] => {
            let a = artist(session, id).await?;
            let country = session.country();
            let ids: Vec<String> = a
                .top_tracks
                .for_country(&country)
                .0
                .iter()
                .filter_map(|u| u.to_uri().ok())
                .collect();
            Ok(json!({ "tracks": tracks(session, &ids).await? }))
        }
        ["artists", id, "albums"] => {
            let a = artist(session, id).await?;
            let ids: Vec<String> = a
                .albums_current()
                .chain(a.singles_current())
                .chain(a.compilations_current())
                .filter_map(|u| u.to_uri().ok())
                .collect();
            let _ = query;
            page(albums(session, &ids).await?, |a| a)
        }
        _ => unsupported(),
    }
}

fn uri(s: &str) -> Result<SpotifyUri, String> {
    SpotifyUri::from_uri(s).map_err(|e| format!("{s}: {e}"))
}

/// A one-page Web API list.
fn page(items: Vec<Value>, wrap: impl Fn(Value) -> Value) -> Result<Value, String> {
    Ok(json!({
        "items": items.into_iter().map(wrap).collect::<Vec<_>>(),
        "next": null,
    }))
}

fn id_of(u: &SpotifyUri) -> String {
    u.to_id().unwrap_or_default()
}

fn images(covers: &librespot_metadata::image::Images) -> Value {
    Value::Array(
        covers
            .0
            .iter()
            .filter_map(|i| {
                let hex = i.id.to_base16().ok()?;
                Some(json!({ "url": format!("https://i.scdn.co/image/{hex}"), "width": i.width }))
            })
            .collect(),
    )
}

fn artists_json(a: &librespot_metadata::artist::Artists) -> Value {
    Value::Array(
        a.0.iter()
            .map(|x| json!({ "id": id_of(&x.id), "name": x.name }))
            .collect(),
    )
}

/// Web API shape of a track.
pub fn track_json(t: &Track) -> Value {
    json!({
        "id": id_of(&t.id),
        "type": "track",
        "name": t.name,
        "duration_ms": t.duration,
        "artists": artists_json(&t.artists),
        "album": {
            "id": id_of(&t.album.id),
            "name": t.album.name,
            "images": images(&t.album.covers),
        },
    })
}

fn album_json(a: &Album) -> Value {
    json!({
        "id": id_of(&a.id),
        "name": a.name,
        "artists": artists_json(&a.artists),
        "images": images(&a.covers),
    })
}

/// Raw extended-metadata payloads for `uris`, in order (missing entries skipped).
async fn batch(
    session: &Session,
    uris: &[String],
    kind: ExtensionKind,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::with_capacity(uris.len());
    for chunk in uris.chunks(BATCH) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|u| EntityRequest {
                    entity_uri: u.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(kind),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = session
            .spclient()
            .get_extended_metadata(req)
            .await
            .map_err(|e| format!("Spotify metadata: {e}"))?;
        let mut by_uri = std::collections::HashMap::new();
        for e in res.extended_metadata {
            for d in e.extension_data {
                if let Some(any) = d.extension_data.into_option() {
                    by_uri.insert(d.entity_uri.clone(), any.value);
                }
            }
        }
        out.extend(
            chunk
                .iter()
                .filter_map(|u| by_uri.remove(u).map(|v| (u.clone(), v))),
        );
    }
    Ok(out)
}

/// Track objects for track URIs (episodes and unknown items are skipped).
pub async fn tracks(session: &Session, uris: &[String]) -> Result<Vec<Value>, String> {
    let uris: Vec<String> = uris
        .iter()
        .filter(|u| u.starts_with("spotify:track:"))
        .cloned()
        .collect();
    let mut out = Vec::with_capacity(uris.len());
    for (u, bytes) in batch(session, &uris, ExtensionKind::TRACK_V4).await? {
        let Ok(msg) = librespot_protocol::metadata::Track::parse_from_bytes(&bytes) else {
            continue;
        };
        if let Ok(t) = Track::parse(&msg, &uri(&u)?) {
            out.push(track_json(&t));
        }
    }
    Ok(out)
}

async fn albums(session: &Session, uris: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(uris.len());
    for (u, bytes) in batch(session, uris, ExtensionKind::ALBUM_V4).await? {
        let Ok(msg) = librespot_protocol::metadata::Album::parse_from_bytes(&bytes) else {
            continue;
        };
        if let Ok(a) = Album::parse(&msg, &uri(&u)?) {
            out.push(album_json(&a));
        }
    }
    Ok(out)
}

/// An artist's pictures: its portraits, or failing those the portrait group or its
/// biography's (where Spotify keeps them for many artists).
fn artist_images(a: &Artist) -> Value {
    let bios = a
        .biographies
        .iter()
        .flat_map(|b| std::iter::once(&b.portraits).chain(b.portrait_group.iter()));
    [&a.portraits, &a.portrait_group]
        .into_iter()
        .chain(bios)
        .find(|i| !i.is_empty())
        .map_or_else(|| json!([]), images)
}

async fn artists(session: &Session, uris: &[String]) -> Result<Vec<Value>, String> {
    let mut out = Vec::with_capacity(uris.len());
    for (u, bytes) in batch(session, uris, ExtensionKind::ARTIST_V4).await? {
        let Ok(msg) = librespot_protocol::metadata::Artist::parse_from_bytes(&bytes) else {
            continue;
        };
        if let Ok(a) = Artist::parse(&msg, &uri(&u)?) {
            out.push(json!({
                "id": id_of(&a.id),
                "name": a.name,
                "images": artist_images(&a),
            }));
        }
    }
    Ok(out)
}

async fn album(session: &Session, id: &str) -> Result<Value, String> {
    let a = Album::get(session, &uri(&format!("spotify:album:{id}"))?)
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = a.tracks().filter_map(|u| u.to_uri().ok()).collect();
    let mut v = album_json(&a);
    v["tracks"] = json!({ "items": tracks(session, &ids).await?, "next": null });
    Ok(v)
}

async fn artist(session: &Session, id: &str) -> Result<Artist, String> {
    Artist::get(session, &uri(&format!("spotify:artist:{id}"))?)
        .await
        .map_err(|e| e.to_string())
}

/// Track URIs of a context (all pages), e.g. the user's collection (Liked Songs).
async fn context_tracks(session: &Session, context: &str) -> Result<Vec<String>, String> {
    let ctx = session
        .spclient()
        .get_context(context)
        .await
        .map_err(|e| format!("Spotify {context}: {e}"))?;
    let mut ids = Vec::new();
    let mut next: Option<String> = None;
    for p in &ctx.pages {
        ids.extend(p.tracks.iter().map(|t| t.uri().to_string()));
        if p.has_next_page_url() {
            next = Some(p.next_page_url().to_string());
        }
    }
    // further pages, bounded
    let mut guard = 0;
    while let Some(url) = next.take() {
        guard += 1;
        if guard > 50 {
            break;
        }
        let bytes = session
            .spclient()
            .get_next_page(&url)
            .await
            .map_err(|e| e.to_string())?;
        let page = protobuf_json_mapping::parse_from_str::<
            librespot_protocol::context_page::ContextPage,
        >(&String::from_utf8_lossy(&bytes))
        .map_err(|e| e.to_string())?;
        ids.extend(page.tracks.iter().map(|t| t.uri().to_string()));
        if page.has_next_page_url() {
            next = Some(page.next_page_url().to_string());
        }
    }
    Ok(ids)
}

/// The albums, playlists and artists the user played from lately, newest first, as the
/// apps' "Recently played" shows them (the Web API's recently played lists tracks instead).
/// Each in its Web API shape with a `type`.
pub async fn recently_played(session: &Session) -> Result<Vec<Value>, String> {
    let user = session.username();
    let path = format!(
        "/recently-played/v3/user/{user}/recently-played?format=json&limit=50&filter=default"
    );
    let bytes = session
        .spclient()
        .request_as_json(&axum::http::Method::GET, &path, None, None)
        .await
        .map_err(|e| format!("Spotify recently played: {e}"))?;
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("Spotify recently played: {e}"))?;
    let uris = recent_contexts(&v);
    let of = |kind: &str| -> Vec<String> {
        uris.iter()
            .filter(|u| u.starts_with(&format!("spotify:{kind}:")))
            .cloned()
            .collect()
    };
    let mut found = std::collections::HashMap::new();
    for a in albums(session, &of("album")).await? {
        found.insert(
            format!("spotify:album:{}", a["id"].as_str().unwrap_or("")),
            a,
        );
    }
    for a in artists(session, &of("artist")).await? {
        found.insert(
            format!("spotify:artist:{}", a["id"].as_str().unwrap_or("")),
            a,
        );
    }
    let (mut lists, mut contents) = (Vec::new(), HashMap::new());
    for u in of("playlist") {
        // one gone or private since is left out
        if let Ok(pl) = Playlist::get(session, &uri(&u)?).await {
            let id = u.trim_start_matches("spotify:playlist:").to_string();
            lists.push(json!({
                "id": id,
                "name": pl.name(),
                "images": playlist_picture(&pl.attributes.picture, &sizes(&pl.attributes.picture_sizes)),
            }));
            contents.insert(id, first_tracks(&pl));
        }
    }
    add_mosaics(session, &mut lists, contents).await;
    for p in lists {
        found.insert(
            format!("spotify:playlist:{}", p["id"].as_str().unwrap_or("")),
            p,
        );
    }
    Ok(in_order(&uris, found))
}

/// The album, playlist and artist URIs of a `recently-played` answer, in its order, once
/// each (other contexts, such as Liked Songs or a radio station, are left out).
fn recent_contexts(v: &Value) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v["playContexts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["uri"].as_str())
        .filter(|u| {
            ["spotify:album:", "spotify:playlist:", "spotify:artist:"]
                .iter()
                .any(|k| u.starts_with(k))
        })
        .filter(|u| seen.insert(u.to_string()))
        .map(str::to_string)
        .collect()
}

/// The `found` objects for `uris`, in that order, each with its `type`; any not found are
/// left out.
fn in_order(uris: &[String], mut found: std::collections::HashMap<String, Value>) -> Vec<Value> {
    uris.iter()
        .filter_map(|u| {
            let mut v = found.remove(u)?;
            v["type"] = json!(u.split(':').nth(1)?);
            Some(v)
        })
        .collect()
}

/// Pages of a collection set, at most.
const COLLECTION_PAGES: usize = 20;

/// The URIs in one of the user's collection sets, as the apps sync them: `collection` (liked
/// tracks and saved albums) or `artist` (followed artists).  Newest first.
async fn collection(session: &Session, set: &str) -> Result<Vec<String>, String> {
    let mut headers = axum::http::HeaderMap::new();
    let proto = axum::http::HeaderValue::from_static("application/vnd.collection-v2.spotify.proto");
    headers.insert(axum::http::header::CONTENT_TYPE, proto.clone());
    headers.insert(axum::http::header::ACCEPT, proto);
    let user = session.username();
    let mut uris = Vec::new();
    let mut token = String::new();
    for _ in 0..COLLECTION_PAGES {
        let body = collection_page_request(&user, set, &token);
        let bytes = session
            .spclient()
            .request(
                &axum::http::Method::POST,
                "/collection/v2/paging",
                Some(headers.clone()),
                Some(&body),
            )
            .await
            .map_err(|e| format!("Spotify collection {set}: {e}"))?;
        let (page, next) =
            collection_page(&bytes).map_err(|e| format!("Spotify collection {set}: {e}"))?;
        uris.extend(page);
        if next.is_empty() {
            break;
        }
        token = next;
    }
    Ok(uris)
}

/// Items per collection page.
const COLLECTION_PAGE: i32 = 300;

/// A `collection2v2.PageRequest` (librespot-protocol does not compile that proto).
fn collection_page_request(user: &str, set: &str, token: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut os = protobuf::CodedOutputStream::vec(&mut out);
    let written = (|| {
        os.write_string(1, user)?;
        os.write_string(2, set)?;
        if !token.is_empty() {
            os.write_string(3, token)?;
        }
        os.write_int32(4, COLLECTION_PAGE)?;
        os.flush()
    })();
    drop(os);
    debug_assert!(written.is_ok(), "writing to a Vec cannot fail");
    out
}

/// A `collection2v2.PageResponse`: the URIs of its items that are not removed, and the token
/// of the next page (empty on the last).
fn collection_page(bytes: &[u8]) -> protobuf::Result<(Vec<String>, String)> {
    let mut is = protobuf::CodedInputStream::from_bytes(bytes);
    let (mut uris, mut next) = (Vec::new(), String::new());
    while let Some(tag) = is.read_raw_tag_or_eof()? {
        match tag >> 3 {
            1 => {
                let item = is.read_bytes()?;
                let mut it = protobuf::CodedInputStream::from_bytes(&item);
                let (mut uri, mut removed) = (String::new(), false);
                while let Some(t) = it.read_raw_tag_or_eof()? {
                    match t >> 3 {
                        1 => uri = it.read_string()?,
                        3 => removed = it.read_bool()?,
                        _ => it.skip_field(wire_type(t)?)?,
                    }
                }
                if !removed && !uri.is_empty() {
                    uris.push(uri);
                }
            }
            2 => next = is.read_string()?,
            _ => is.skip_field(wire_type(tag)?)?,
        }
    }
    Ok((uris, next))
}

fn wire_type(tag: u32) -> protobuf::Result<protobuf::rt::WireType> {
    protobuf::rt::WireType::new(tag & 7)
        .ok_or_else(|| protobuf::Error::from(std::io::Error::other("bad wire type")))
}

/// The user's playlists (rootlist), folders flattened.
async fn playlists(session: &Session) -> Result<Value, String> {
    let bytes = session
        .spclient()
        .get_rootlist(0, Some(1000))
        .await
        .map_err(|e| format!("Spotify playlists: {e}"))?;
    let list = SelectedListContent::parse_from_bytes(&bytes).map_err(|e| e.to_string())?;
    let owner = session.username();
    let mut contents = HashMap::new();
    if list.contents.meta_items.len() != list.contents.items.len() {
        // no per-item attributes to pair with: look each playlist up
        let mut items = Vec::new();
        for item in &list.contents.items {
            let Some(id) = item.uri().strip_prefix("spotify:playlist:") else {
                continue;
            };
            if let Ok(pl) = Playlist::get(session, &uri(item.uri())?).await {
                items.push(json!({
                    "id": id,
                    "name": pl.name(),
                    "owner": { "display_name": owner },
                    "images": playlist_picture(&pl.attributes.picture, &sizes(&pl.attributes.picture_sizes)),
                }));
                contents.insert(id.to_string(), first_tracks(&pl));
            }
        }
        add_mosaics(session, &mut items, contents).await;
        return page(items, |p| p);
    }
    let mut items = list
        .contents
        .items
        .iter()
        .zip(list.contents.meta_items.iter())
        .filter_map(|(item, meta)| {
            let id = item.uri().strip_prefix("spotify:playlist:")?;
            let who = if meta.has_owner_username() {
                meta.owner_username()
            } else {
                owner.as_str()
            };
            let a = &meta.attributes;
            let sizes: Vec<(String, String)> = a
                .picture_size
                .iter()
                .map(|s| (s.target_name().to_string(), s.url().to_string()))
                .collect();
            Some(json!({
                "id": id,
                "name": a.name(),
                "owner": { "display_name": who },
                "images": playlist_picture(a.picture(), &sizes),
            }))
        })
        .collect::<Vec<_>>();
    add_mosaics(session, &mut items, contents).await;
    name_unnamed(session, &mut items).await;
    page(items, |p| p)
}

/// Name the playlists that have none after their owner, as the apps do: the owner's display
/// name, else their username (what `owner.display_name` holds here).
async fn name_unnamed(session: &Session, items: &mut [Value]) {
    let mut names = HashMap::new();
    for owner in unnamed_owners(items) {
        let shown = session
            .spclient()
            .get_user_profile(&owner, Some(0), Some(0))
            .await
            .ok()
            .and_then(|b| profile_name(&b))
            .unwrap_or_else(|| owner.clone());
        names.insert(owner, shown);
    }
    name_after_owners(items, &names);
}

fn is_unnamed(p: &Value) -> bool {
    p["name"].as_str().is_some_and(|n| n.trim().is_empty())
}

/// The owners of the playlists without a name, once each.
fn unnamed_owners(items: &[Value]) -> Vec<String> {
    let mut owners: Vec<String> = items
        .iter()
        .filter(|p| is_unnamed(p))
        .filter_map(|p| p["owner"]["display_name"].as_str().map(str::to_string))
        .collect();
    owners.sort();
    owners.dedup();
    owners
}

/// Give the playlists without a name their owner's (from `names`, by username).
fn name_after_owners(items: &mut [Value], names: &HashMap<String, String>) {
    for p in items.iter_mut().filter(|p| is_unnamed(p)) {
        if let Some(n) = p["owner"]["display_name"]
            .as_str()
            .and_then(|o| names.get(o))
        {
            p["name"] = json!(n);
        }
    }
}

/// The display name in a user profile (`user-profile-view`).
fn profile_name(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(bytes).ok()?["name"]
        .as_str()
        .filter(|n| !n.trim().is_empty())
        .map(str::to_string)
}

/// A playlist's own picture: one uploaded for it, or the sizes Spotify's own playlists
/// come with.  Empty when it has neither (the apps then show a mosaic, see [`mosaic`]).
fn playlist_picture(picture: &[u8], sizes: &[(String, String)]) -> Value {
    if !picture.is_empty() {
        let hex: String = picture.iter().map(|b| format!("{b:02x}")).collect();
        return json!([{ "url": format!("https://i.scdn.co/image/{hex}") }]);
    }
    Value::Array(
        sizes
            .iter()
            .filter(|(_, url)| !url.is_empty())
            .map(|(target, url)| {
                let width = match target.as_str() {
                    "small" => 60,
                    "large" => 640,
                    "xlarge" => 1280,
                    _ => 300,
                };
                json!({ "url": url, "width": width })
            })
            .collect(),
    )
}

fn sizes(s: &librespot_metadata::image::PictureSizes) -> Vec<(String, String)> {
    s.iter()
        .map(|p| (p.target_name.clone(), p.url.clone()))
        .collect()
}

/// Tracks of a playlist looked at for its mosaic, at most.
const MOSAIC_TRACKS: usize = 50;

fn first_tracks(pl: &Playlist) -> Vec<String> {
    pl.tracks()
        .filter_map(|u| u.to_uri().ok())
        .filter(|u| u.starts_with("spotify:track:"))
        .take(MOSAIC_TRACKS)
        .collect()
}

/// The picture the apps show for a playlist without one, from its tracks (Web API shape,
/// in order): the covers of its first four albums in a grid, or the first cover when there
/// are fewer.
fn mosaic(tracks: &[Value]) -> Value {
    let mut covers: Vec<&str> = Vec::new();
    for t in tracks {
        let cover = t["album"]["images"]
            .as_array()
            .and_then(|i| i.iter().max_by_key(|i| i["width"].as_u64().unwrap_or(0)))
            .and_then(|i| i["url"].as_str()?.strip_prefix("https://i.scdn.co/image/"));
        if let Some(c) = cover.filter(|c| !covers.contains(c)) {
            covers.push(c);
            if covers.len() == 4 {
                return json!([{
                    "url": format!("https://mosaic.scdn.co/640/{}", covers.concat()),
                    "width": 640,
                }]);
            }
        }
    }
    match covers.first() {
        Some(c) => json!([{ "url": format!("https://i.scdn.co/image/{c}") }]),
        None => json!([]),
    }
}

/// Playlists looked up at once for their mosaics.
const MOSAIC_LOOKUPS: usize = 8;

/// Give each of `lists` (Web API playlists) without a picture its [`mosaic`], from its first
/// tracks: in `contents` (track URIs by playlist id), or else looked up.  Best effort: a
/// playlist that can't be looked up keeps no picture.
async fn add_mosaics(
    session: &Session,
    lists: &mut [Value],
    mut contents: HashMap<String, Vec<String>>,
) {
    use futures::StreamExt;
    let bare: Vec<String> = lists
        .iter()
        .filter(|p| p["images"].as_array().is_none_or(|i| i.is_empty()))
        .filter_map(|p| p["id"].as_str().map(str::to_string))
        .collect();
    let missing: Vec<String> = bare
        .iter()
        .filter(|id| !contents.contains_key(*id))
        .cloned()
        .collect();
    let looked_up: Vec<(String, Vec<String>)> = futures::stream::iter(missing)
        .map(|id| async move {
            let pl = match uri(&format!("spotify:playlist:{id}")) {
                Ok(u) => Playlist::get(session, &u).await.ok(),
                Err(_) => None,
            };
            (id, pl.as_ref().map(first_tracks).unwrap_or_default())
        })
        .buffered(MOSAIC_LOOKUPS)
        .collect()
        .await;
    contents.extend(looked_up);
    let mut wanted: Vec<String> = bare
        .iter()
        .flat_map(|id| contents.get(id).into_iter().flatten().cloned())
        .collect();
    wanted.sort();
    wanted.dedup();
    let Ok(found) = tracks(session, &wanted).await else {
        return;
    };
    let by_id: HashMap<&str, &Value> = found
        .iter()
        .filter_map(|t| Some((t["id"].as_str()?, t)))
        .collect();
    for p in lists.iter_mut() {
        let Some(id) = p["id"].as_str().filter(|id| bare.iter().any(|b| b == id)) else {
            continue;
        };
        let ts: Vec<Value> = contents
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|u| by_id.get(u.trim_start_matches("spotify:track:")))
            .map(|t| (*t).clone())
            .collect();
        p["images"] = mosaic(&ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::CodedOutputStream;

    fn encode(f: impl FnOnce(&mut CodedOutputStream) -> protobuf::Result<()>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut os = CodedOutputStream::vec(&mut out);
        f(&mut os).unwrap();
        os.flush().unwrap();
        drop(os);
        out
    }

    /// A `CollectionItem` as Spotify sends it.
    fn item(uri: &str, added_at: i32, removed: bool) -> Vec<u8> {
        encode(|os| {
            os.write_string(1, uri)?;
            os.write_int32(2, added_at)?;
            if removed {
                os.write_bool(3, true)?;
            }
            os.write_string(4, "spotify:context:unknown-field")
        })
    }

    fn track_on(cover: &str) -> Value {
        json!({ "album": { "images": [
            { "url": format!("https://i.scdn.co/image/small{cover}"), "width": 64 },
            { "url": format!("https://i.scdn.co/image/{cover}"), "width": 640 },
        ]}})
    }

    #[test]
    fn a_playlist_without_a_picture_gets_a_mosaic_of_its_first_four_albums() {
        let ts: Vec<Value> = ["a", "a", "b", "c", "b", "d", "e"]
            .into_iter()
            .map(track_on)
            .collect();
        assert_eq!(
            mosaic(&ts),
            json!([{ "url": "https://mosaic.scdn.co/640/abcd", "width": 640 }])
        );
    }

    #[test]
    fn a_playlist_of_fewer_than_four_albums_shows_the_first_cover() {
        let ts: Vec<Value> = ["b", "b", "c", "b", "b"]
            .into_iter()
            .map(track_on)
            .collect();
        assert_eq!(mosaic(&ts), json!([{ "url": "https://i.scdn.co/image/b" }]));
        assert_eq!(mosaic(&[json!({ "album": {} })]), json!([]));
    }

    #[test]
    fn a_playlist_shows_its_uploaded_picture_or_spotifys_sizes() {
        let sizes = [
            ("small", "s.jpg"),
            ("default", "d.jpg"),
            ("xlarge", "x.jpg"),
            ("large", ""),
        ]
        .map(|(t, u)| (t.to_string(), u.to_string()));
        assert_eq!(
            playlist_picture(&[0xab, 0x67, 0x0f], &sizes),
            json!([{ "url": "https://i.scdn.co/image/ab670f" }])
        );
        assert_eq!(
            playlist_picture(&[], &sizes),
            json!([
                { "url": "s.jpg", "width": 60 },
                { "url": "d.jpg", "width": 300 },
                { "url": "x.jpg", "width": 1280 },
            ])
        );
        assert_eq!(playlist_picture(&[], &[]), json!([]));
    }

    #[test]
    fn a_playlist_without_a_name_is_named_after_its_owner() {
        let mut items = vec![
            json!({ "id": "p1", "name": "", "owner": { "display_name": "thatpeytonkid" } }),
            json!({ "id": "p2", "name": "Bardcore Covers", "owner": { "display_name": "adaptable" } }),
            json!({ "id": "p3", "name": " ", "owner": { "display_name": "thatpeytonkid" } }),
            json!({ "id": "p4", "name": "", "owner": { "display_name": "nobody" } }),
        ];
        assert_eq!(unnamed_owners(&items), ["nobody", "thatpeytonkid"]);
        let names = [("thatpeytonkid", "Peyton Ryan White"), ("nobody", "nobody")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into_iter()
            .collect();
        name_after_owners(&mut items, &names);
        let shown: Vec<&str> = items.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(
            shown,
            [
                "Peyton Ryan White",
                "Bardcore Covers",
                "Peyton Ryan White",
                "nobody"
            ]
        );
    }

    #[test]
    fn a_profile_gives_its_display_name() {
        let profile = br#"{ "uri": "spotify:user:thatpeytonkid", "name": "Peyton Ryan White" }"#;
        assert_eq!(profile_name(profile).as_deref(), Some("Peyton Ryan White"));
        assert_eq!(profile_name(br#"{ "name": "" }"#), None);
        assert_eq!(profile_name(b"not json"), None);
    }

    #[test]
    fn recently_played_keeps_albums_playlists_and_artists_once_each_newest_first() {
        let v = json!({ "playContexts": [
            { "uri": "spotify:playlist:p1", "lastPlayedTrackUri": "spotify:track:t1" },
            { "uri": "spotify:user:me:collection" },
            { "uri": "spotify:artist:a1" },
            { "uri": "spotify:album:b1" },
            { "uri": "spotify:station:album:b1" },
            { "uri": "spotify:playlist:p1" },
            { "lastPlayedTime": 1 },
        ]});
        assert_eq!(
            recent_contexts(&v),
            [
                "spotify:playlist:p1",
                "spotify:artist:a1",
                "spotify:album:b1"
            ]
        );
        assert!(recent_contexts(&json!({})).is_empty());
    }

    #[test]
    fn recently_played_lists_what_was_found_in_the_played_order_with_its_type() {
        let uris = [
            "spotify:album:b1",
            "spotify:playlist:gone",
            "spotify:artist:a1",
        ]
        .map(String::from);
        let found = [
            ("spotify:artist:a1", json!({ "id": "a1", "name": "Auve" })),
            (
                "spotify:album:b1",
                json!({ "id": "b1", "name": "Walking on a Dream" }),
            ),
        ]
        .into_iter()
        .map(|(u, v)| (u.to_string(), v))
        .collect();
        assert_eq!(
            in_order(&uris, found),
            [
                json!({ "id": "b1", "name": "Walking on a Dream", "type": "album" }),
                json!({ "id": "a1", "name": "Auve", "type": "artist" }),
            ]
        );
    }

    #[test]
    fn collection_pages_skip_removed_items_and_carry_the_next_token() {
        let bytes = encode(|os| {
            os.write_bytes(1, &item("spotify:artist:a", 3, false))?;
            os.write_bytes(1, &item("spotify:artist:gone", 2, true))?;
            os.write_bytes(1, &item("spotify:album:b", 1, false))?;
            os.write_string(2, "next-page")?;
            os.write_string(3, "sync-token")
        });
        let (uris, next) = collection_page(&bytes).unwrap();
        assert_eq!(uris, ["spotify:artist:a", "spotify:album:b"]);
        assert_eq!(next, "next-page");

        let (uris, next) = collection_page(&[]).unwrap();
        assert!(uris.is_empty() && next.is_empty());
    }

    #[test]
    fn collection_page_requests_carry_user_set_token_and_limit() {
        let fields = |bytes: &[u8]| {
            let mut is = protobuf::CodedInputStream::from_bytes(bytes);
            let mut f = Vec::new();
            while let Some(tag) = is.read_raw_tag_or_eof().unwrap() {
                f.push(match tag >> 3 {
                    4 => (4, is.read_int32().unwrap().to_string()),
                    n => (n, is.read_string().unwrap()),
                });
            }
            f
        };
        assert_eq!(
            fields(&collection_page_request("me", "artist", "")),
            [
                (1, "me".into()),
                (2, "artist".into()),
                (4, COLLECTION_PAGE.to_string())
            ]
        );
        assert_eq!(
            fields(&collection_page_request("me", "collection", "t")),
            [
                (1, "me".to_string()),
                (2, "collection".into()),
                (3, "t".into()),
                (4, COLLECTION_PAGE.to_string())
            ]
        );
        assert!(
            collection_page(b"\xff").is_err(),
            "garbage is an error, not a panic"
        );
    }
}
