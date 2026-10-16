//! Browsing without the Web API: the Web API paths this plugin uses, answered through the
//! librespot session (Spotify's own client endpoints, as the official apps use them).
//!
//! The Web API needs a token for a registered client id; the shared desktop id is
//! rate-limited by Spotify to the point of being unusable, and a per-user app is too much
//! to ask.  The session needs nothing beyond signing in, so [`Spotify::api`] falls back to
//! [`session_get`] when the Web API fails.  Results have the Web API's JSON shape (only the
//! fields this plugin reads), so the menus work unchanged.  Saved albums and followed artists
//! come from the collection service the apps sync with.  Not available this way: search,
//! recently played, top tracks/artists, new releases.

use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message};
use serde_json::{json, Value};

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
                "images": images(&a.portraits),
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
                    "images": [],
                }));
            }
        }
        return page(items, |p| p);
    }
    let items = list
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
            Some(json!({
                "id": id,
                "name": meta.attributes.name(),
                "owner": { "display_name": who },
                "images": [],
            }))
        })
        .collect::<Vec<_>>();
    page(items, |p| p)
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
