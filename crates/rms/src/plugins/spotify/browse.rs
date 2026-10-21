//! Browsing through the librespot session: Spotify's own client endpoints, as the official
//! apps use them (metadata, the collection, the rootlist, recently played).
//!
//! The Web API is no use for this: Spotify rate-limits the shared client id to the point of
//! being unusable, and since February 2026 apps of one's own (Development Mode) lost the
//! endpoints RMS needs (an artist's top tracks, new releases, other users' playlists,
//! searches of more than 10).  The session needs nothing beyond signing in.  Top tracks and
//! artists, search, Release Radar and new releases come from [`super::pathfinder`].

use super::model::{Album, Artist, ArtistRef, Context, Playlist, Track};
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Metadata, Playlist as SessionPlaylist};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::playlist4_external::SelectedListContent;
use protobuf::{EnumOrUnknown, Message};
use serde_json::Value;
use std::collections::HashMap;

/// Entities per extended-metadata request.
const BATCH: usize = 200;
/// Liked Songs and a playlist's tracks, at most.
const TRACKS_MAX: usize = 2000;
/// Saved albums, followed artists and playlists, at most.
const LIST_MAX: usize = 1000;
/// An artist's albums, at most.
const ARTIST_ALBUMS_MAX: usize = 500;

fn uri(s: &str) -> Result<SpotifyUri, String> {
    SpotifyUri::from_uri(s).map_err(|e| format!("{s}: {e}"))
}

fn id_of(u: &SpotifyUri) -> String {
    u.to_id().unwrap_or_default()
}

fn uris(it: impl Iterator<Item = SpotifyUri>) -> Vec<String> {
    it.filter_map(|u| u.to_uri().ok()).collect()
}

/// The URL of the largest of `covers`.
fn largest(covers: &librespot_metadata::image::Images) -> Option<String> {
    covers
        .0
        .iter()
        .max_by_key(|i| i.width)
        .and_then(|i| i.id.to_base16().ok())
        .map(|hex| format!("https://i.scdn.co/image/{hex}"))
}

fn artist_refs(a: &librespot_metadata::artist::Artists) -> Vec<ArtistRef> {
    a.0.iter()
        .map(|x| ArtistRef {
            id: id_of(&x.id),
            name: x.name.clone(),
        })
        .collect()
}

fn track(t: &librespot_metadata::Track) -> Track {
    let year = t.album.date.year();
    Track {
        id: id_of(&t.id),
        name: t.name.clone(),
        artists: artist_refs(&t.artists),
        album: t.album.name.clone(),
        album_id: id_of(&t.album.id),
        cover: largest(&t.album.covers),
        year: (year > 0).then_some(year),
        duration_ms: t.duration.max(0) as u32,
        isrc: t
            .external_ids
            .iter()
            .find(|x| x.external_type.eq_ignore_ascii_case("isrc"))
            .map(|x| x.id.clone()),
    }
}

fn album(a: &librespot_metadata::Album) -> Album {
    Album {
        id: id_of(&a.id),
        name: a.name.clone(),
        artists: artist_refs(&a.artists),
        cover: largest(&a.covers),
    }
}

/// An artist's picture: its portraits, or failing those the portrait group or its
/// biography's (where Spotify keeps them for many artists).
fn artist(a: &librespot_metadata::Artist) -> Artist {
    let bios = a
        .biographies
        .iter()
        .flat_map(|b| std::iter::once(&b.portraits).chain(b.portrait_group.iter()));
    let picture = [&a.portraits, &a.portrait_group]
        .into_iter()
        .chain(bios)
        .find(|i| !i.is_empty())
        .and_then(largest);
    Artist {
        id: id_of(&a.id),
        name: a.name.clone(),
        picture,
    }
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
        let mut by_uri = HashMap::new();
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

/// The tracks of track URIs, in order (episodes and unknown items are skipped).
pub async fn tracks(session: &Session, uris: &[String]) -> Result<Vec<Track>, String> {
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
        if let Ok(t) = librespot_metadata::Track::parse(&msg, &uri(&u)?) {
            out.push(track(&t));
        }
    }
    Ok(out)
}

pub async fn albums(session: &Session, uris: &[String]) -> Result<Vec<Album>, String> {
    let mut out = Vec::with_capacity(uris.len());
    for (u, bytes) in batch(session, uris, ExtensionKind::ALBUM_V4).await? {
        let Ok(msg) = librespot_protocol::metadata::Album::parse_from_bytes(&bytes) else {
            continue;
        };
        if let Ok(a) = librespot_metadata::Album::parse(&msg, &uri(&u)?) {
            out.push(album(&a));
        }
    }
    Ok(out)
}

pub async fn artists(session: &Session, uris: &[String]) -> Result<Vec<Artist>, String> {
    let mut out = Vec::with_capacity(uris.len());
    for (u, bytes) in batch(session, uris, ExtensionKind::ARTIST_V4).await? {
        let Ok(msg) = librespot_protocol::metadata::Artist::parse_from_bytes(&bytes) else {
            continue;
        };
        if let Ok(a) = librespot_metadata::Artist::parse(&msg, &uri(&u)?) {
            out.push(artist(&a));
        }
    }
    Ok(out)
}

/// An album's tracks.
pub async fn album_tracks(session: &Session, id: &str) -> Result<Vec<Track>, String> {
    let a = librespot_metadata::Album::get(session, &uri(&format!("spotify:album:{id}"))?)
        .await
        .map_err(|e| e.to_string())?;
    tracks(session, &uris(a.tracks().cloned())).await
}

/// A playlist's tracks (any playlist, not only the user's).
pub async fn playlist_tracks(session: &Session, id: &str) -> Result<Vec<Track>, String> {
    let pl = SessionPlaylist::get(session, &uri(&format!("spotify:playlist:{id}"))?)
        .await
        .map_err(|e| e.to_string())?;
    let mut ids = uris(pl.tracks().cloned());
    ids.truncate(TRACKS_MAX);
    tracks(session, &ids).await
}

/// Liked Songs, newest first.
pub async fn liked(session: &Session) -> Result<Vec<Track>, String> {
    let user = session.username();
    let mut ids = context_tracks(session, &format!("spotify:user:{user}:collection")).await?;
    ids.truncate(TRACKS_MAX);
    tracks(session, &ids).await
}

/// Saved albums, newest first.
pub async fn saved_albums(session: &Session) -> Result<Vec<Album>, String> {
    let mut ids: Vec<String> = collection(session, "collection")
        .await?
        .into_iter()
        .filter(|u| u.starts_with("spotify:album:"))
        .collect();
    ids.truncate(LIST_MAX);
    albums(session, &ids).await
}

/// Followed artists, newest first.
pub async fn followed_artists(session: &Session) -> Result<Vec<Artist>, String> {
    let mut ids = collection(session, "artist").await?;
    ids.truncate(LIST_MAX);
    artists(session, &ids).await
}

/// An artist's most played tracks in the account's country.
pub async fn artist_top_tracks(session: &Session, id: &str) -> Result<Vec<Track>, String> {
    let a = artist_meta(session, id).await?;
    let country = session.country();
    tracks(
        session,
        &uris(a.top_tracks.for_country(&country).0.into_iter()),
    )
    .await
}

/// An artist's albums, singles and compilations.
pub async fn artist_albums(session: &Session, id: &str) -> Result<Vec<Album>, String> {
    let a = artist_meta(session, id).await?;
    let mut ids = uris(
        a.albums_current()
            .chain(a.singles_current())
            .chain(a.compilations_current())
            .cloned(),
    );
    ids.truncate(ARTIST_ALBUMS_MAX);
    albums(session, &ids).await
}

async fn artist_meta(session: &Session, id: &str) -> Result<librespot_metadata::Artist, String> {
    librespot_metadata::Artist::get(session, &uri(&format!("spotify:artist:{id}"))?)
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
/// apps' "Recently played" shows them.
pub async fn recently_played(session: &Session) -> Result<Vec<Context>, String> {
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
    let mut found = HashMap::new();
    for a in albums(session, &of("album")).await? {
        found.insert(format!("spotify:album:{}", a.id), Context::Album(a));
    }
    for a in artists(session, &of("artist")).await? {
        found.insert(format!("spotify:artist:{}", a.id), Context::Artist(a));
    }
    let (mut lists, mut contents) = (Vec::new(), HashMap::new());
    for u in of("playlist") {
        // one gone or private since is left out
        if let Ok(pl) = SessionPlaylist::get(session, &uri(&u)?).await {
            let id = u.trim_start_matches("spotify:playlist:").to_string();
            lists.push(Playlist {
                id: id.clone(),
                name: pl.name().to_string(),
                owner: None,
                picture: playlist_picture(
                    &pl.attributes.picture,
                    &sizes(&pl.attributes.picture_sizes),
                ),
            });
            contents.insert(id, first_tracks(&pl));
        }
    }
    add_mosaics(session, &mut lists, contents).await;
    for p in lists {
        found.insert(format!("spotify:playlist:{}", p.id), Context::Playlist(p));
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

/// The `found` contexts for `uris`, in that order; any not found are left out.
fn in_order(uris: &[String], mut found: HashMap<String, Context>) -> Vec<Context> {
    uris.iter().filter_map(|u| found.remove(u)).collect()
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
pub async fn playlists(session: &Session) -> Result<Vec<Playlist>, String> {
    let bytes = session
        .spclient()
        .get_rootlist(0, Some(LIST_MAX))
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
            if let Ok(pl) = SessionPlaylist::get(session, &uri(item.uri())?).await {
                items.push(Playlist {
                    id: id.to_string(),
                    name: pl.name().to_string(),
                    owner: Some(owner.clone()),
                    picture: playlist_picture(
                        &pl.attributes.picture,
                        &sizes(&pl.attributes.picture_sizes),
                    ),
                });
                contents.insert(id.to_string(), first_tracks(&pl));
            }
        }
        add_mosaics(session, &mut items, contents).await;
        return Ok(items);
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
            Some(Playlist {
                id: id.to_string(),
                name: a.name().to_string(),
                owner: Some(who.to_string()),
                picture: playlist_picture(a.picture(), &sizes),
            })
        })
        .collect::<Vec<_>>();
    add_mosaics(session, &mut items, contents).await;
    name_unnamed(session, &mut items).await;
    Ok(items)
}

/// Name the playlists that have none after their owner, as the apps do: the owner's display
/// name, else their username (what `owner` holds here).
async fn name_unnamed(session: &Session, items: &mut [Playlist]) {
    let mut names = HashMap::new();
    for owner in unnamed_owners(items) {
        let shown = display_name(session, &owner)
            .await
            .unwrap_or_else(|| owner.clone());
        names.insert(owner, shown);
    }
    name_after_owners(items, &names);
}

/// A user's display name, from their profile.
pub async fn display_name(session: &Session, user: &str) -> Option<String> {
    let bytes = session
        .spclient()
        .get_user_profile(user, Some(0), Some(0))
        .await
        .ok()?;
    profile_name(&bytes)
}

fn is_unnamed(p: &Playlist) -> bool {
    p.name.trim().is_empty()
}

/// The owners of the playlists without a name, once each.
fn unnamed_owners(items: &[Playlist]) -> Vec<String> {
    let mut owners: Vec<String> = items
        .iter()
        .filter(|p| is_unnamed(p))
        .filter_map(|p| p.owner.clone())
        .collect();
    owners.sort();
    owners.dedup();
    owners
}

/// Give the playlists without a name their owner's (from `names`, by username).
fn name_after_owners(items: &mut [Playlist], names: &HashMap<String, String>) {
    for p in items.iter_mut().filter(|p| is_unnamed(p)) {
        if let Some(n) = p.owner.as_ref().and_then(|o| names.get(o)) {
            p.name = n.clone();
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

/// A playlist's own picture: one uploaded for it, or the largest of the sizes Spotify's own
/// playlists come with.  None when it has neither (the apps then show a mosaic, see
/// [`mosaic`]).
fn playlist_picture(picture: &[u8], sizes: &[(String, String)]) -> Option<String> {
    if !picture.is_empty() {
        let hex: String = picture.iter().map(|b| format!("{b:02x}")).collect();
        return Some(format!("https://i.scdn.co/image/{hex}"));
    }
    sizes
        .iter()
        .filter(|(_, url)| !url.is_empty())
        .max_by_key(|(target, _)| match target.as_str() {
            "small" => 60,
            "large" => 640,
            "xlarge" => 1280,
            _ => 300,
        })
        .map(|(_, url)| url.clone())
}

fn sizes(s: &librespot_metadata::image::PictureSizes) -> Vec<(String, String)> {
    s.iter()
        .map(|p| (p.target_name.clone(), p.url.clone()))
        .collect()
}

/// Tracks of a playlist looked at for its mosaic, at most.
const MOSAIC_TRACKS: usize = 50;

fn first_tracks(pl: &SessionPlaylist) -> Vec<String> {
    pl.tracks()
        .filter_map(|u| u.to_uri().ok())
        .filter(|u| u.starts_with("spotify:track:"))
        .take(MOSAIC_TRACKS)
        .collect()
}

/// The picture the apps show for a playlist without one, from its tracks (in order): the
/// covers of its first four albums in a grid, or the first cover when there are fewer.
fn mosaic(tracks: &[&Track]) -> Option<String> {
    let mut covers: Vec<&str> = Vec::new();
    for t in tracks {
        let cover = t
            .cover
            .as_deref()
            .and_then(|c| c.strip_prefix("https://i.scdn.co/image/"));
        if let Some(c) = cover.filter(|c| !covers.contains(c)) {
            covers.push(c);
            if covers.len() == 4 {
                return Some(format!("https://mosaic.scdn.co/640/{}", covers.concat()));
            }
        }
    }
    covers
        .first()
        .map(|c| format!("https://i.scdn.co/image/{c}"))
}

/// Playlists looked up at once for their mosaics.
const MOSAIC_LOOKUPS: usize = 8;

/// Give each of `lists` without a picture its [`mosaic`], from its first tracks: in
/// `contents` (track URIs by playlist id), or else looked up.  Best effort: a playlist that
/// can't be looked up keeps no picture.
async fn add_mosaics(
    session: &Session,
    lists: &mut [Playlist],
    mut contents: HashMap<String, Vec<String>>,
) {
    use futures::StreamExt;
    let bare: Vec<String> = lists
        .iter()
        .filter(|p| p.picture.is_none())
        .map(|p| p.id.clone())
        .collect();
    let missing: Vec<String> = bare
        .iter()
        .filter(|id| !contents.contains_key(*id))
        .cloned()
        .collect();
    let looked_up: Vec<(String, Vec<String>)> = futures::stream::iter(missing)
        .map(|id| async move {
            let pl = match uri(&format!("spotify:playlist:{id}")) {
                Ok(u) => SessionPlaylist::get(session, &u).await.ok(),
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
    let by_id: HashMap<&str, &Track> = found.iter().map(|t| (t.id.as_str(), t)).collect();
    for p in lists.iter_mut().filter(|p| bare.contains(&p.id)) {
        let ts: Vec<&Track> = contents
            .get(&p.id)
            .into_iter()
            .flatten()
            .filter_map(|u| by_id.get(u.trim_start_matches("spotify:track:")).copied())
            .collect();
        p.picture = mosaic(&ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protobuf::CodedOutputStream;
    use serde_json::json;

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

    fn track_on(cover: &str) -> Track {
        Track {
            cover: Some(format!("https://i.scdn.co/image/{cover}")),
            ..Default::default()
        }
    }

    #[test]
    fn a_playlist_without_a_picture_gets_a_mosaic_of_its_first_four_albums() {
        let ts: Vec<Track> = ["a", "a", "b", "c", "b", "d", "e"]
            .into_iter()
            .map(track_on)
            .collect();
        assert_eq!(
            mosaic(&ts.iter().collect::<Vec<_>>()).as_deref(),
            Some("https://mosaic.scdn.co/640/abcd")
        );
    }

    #[test]
    fn a_playlist_of_fewer_than_four_albums_shows_the_first_cover() {
        let ts: Vec<Track> = ["b", "b", "c", "b", "b"]
            .into_iter()
            .map(track_on)
            .collect();
        assert_eq!(
            mosaic(&ts.iter().collect::<Vec<_>>()).as_deref(),
            Some("https://i.scdn.co/image/b")
        );
        assert_eq!(mosaic(&[&Track::default()]), None);
    }

    #[test]
    fn a_playlist_shows_its_uploaded_picture_or_the_largest_of_spotifys_sizes() {
        let sizes = [
            ("small", "s.jpg"),
            ("default", "d.jpg"),
            ("xlarge", "x.jpg"),
            ("large", ""),
        ]
        .map(|(t, u)| (t.to_string(), u.to_string()));
        assert_eq!(
            playlist_picture(&[0xab, 0x67, 0x0f], &sizes).as_deref(),
            Some("https://i.scdn.co/image/ab670f")
        );
        assert_eq!(playlist_picture(&[], &sizes).as_deref(), Some("x.jpg"));
        assert_eq!(playlist_picture(&[], &[]), None);
    }

    #[test]
    fn a_playlist_without_a_name_is_named_after_its_owner() {
        let pl = |id: &str, name: &str, owner: &str| Playlist {
            id: id.into(),
            name: name.into(),
            owner: Some(owner.into()),
            picture: None,
        };
        let mut items = vec![
            pl("p1", "", "thatpeytonkid"),
            pl("p2", "Bardcore Covers", "adaptable"),
            pl("p3", " ", "thatpeytonkid"),
            pl("p4", "", "nobody"),
        ];
        assert_eq!(unnamed_owners(&items), ["nobody", "thatpeytonkid"]);
        let names = [("thatpeytonkid", "Peyton Ryan White"), ("nobody", "nobody")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into_iter()
            .collect();
        name_after_owners(&mut items, &names);
        let shown: Vec<&str> = items.iter().map(|p| p.name.as_str()).collect();
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
    fn recently_played_lists_what_was_found_in_the_played_order() {
        let uris = [
            "spotify:album:b1",
            "spotify:playlist:gone",
            "spotify:artist:a1",
        ]
        .map(String::from);
        let auve = Artist {
            id: "a1".into(),
            name: "Auve".into(),
            picture: None,
        };
        let dream = Album {
            id: "b1".into(),
            name: "Walking on a Dream".into(),
            ..Default::default()
        };
        let found = [
            ("spotify:artist:a1", Context::Artist(auve.clone())),
            ("spotify:album:b1", Context::Album(dream.clone())),
        ]
        .into_iter()
        .map(|(u, c)| (u.to_string(), c))
        .collect();
        assert_eq!(
            in_order(&uris, found),
            [Context::Album(dream), Context::Artist(auve)]
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
