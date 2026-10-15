//! Favorites, Spotify and internet radio: their menus as `BrowseItem` lists, and the Spotify
//! sign-in.  They run the LMS commands Jive clients use, so all clients see the same menus.

use super::commands::{exec_for, CommandError};
use super::library::{LIMIT, MAX_LIMIT};
use super::model::{
    BrowseItem, BrowseItemType, LearnedTrack, LearnedTracks, ListenBrainzAccount,
    ListenBrainzAccountPatch, NewFavorite, NewListenBrainzAccount, Page, SpotifyLogin,
    SpotifyLoginResult, SpotifyPatch, SpotifyStatus,
};
use super::state::{artwork_path, int, string};
use crate::server::Server;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
pub struct MenuParams {
    #[serde(default)]
    pub offset: usize,
    pub limit: Option<usize>,
    /// The folder to list; the top level without.
    pub id: Option<String>,
}

async fn run(server: &Arc<Server>, words: Vec<String>) -> Result<Map<String, Value>, CommandError> {
    Ok(exec_for(server, None, words).await?.results)
}

/// `favorites items` / `spotify items` as a page of `BrowseItem`s.
pub async fn menu(
    server: &Arc<Server>,
    service: &str,
    p: MenuParams,
) -> Result<Page<BrowseItem>, CommandError> {
    let offset = p.offset;
    let limit = p.limit.unwrap_or(LIMIT).min(MAX_LIMIT);
    let mut words = vec![
        service.to_string(),
        "items".into(),
        offset.to_string(),
        limit.to_string(),
        "want_url:1".into(),
    ];
    if let Some(id) = p.id.filter(|s| !s.is_empty()) {
        words.push(format!("item_id:{}", id));
    }
    let r = run(server, words).await?;
    let items: Vec<BrowseItem> = r
        .get("loop_loop")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_object)
                .filter_map(browse_item)
                .collect()
        })
        .unwrap_or_default();
    let total = int(r.get("count"))
        .map(|n| n.max(0) as usize)
        .unwrap_or(offset + items.len());
    Ok(Page {
        total,
        offset,
        items,
    })
}

/// Split a radio `BrowseItem` id (`radiobrowser.1.0`) into a radio provider's tag and the
/// item id within its menu.  Only the radio providers' commands can be reached this way.
pub fn radio_id(id: &str) -> Result<(&str, &str), CommandError> {
    let (tag, rest) = id.split_once('.').unwrap_or((id, ""));
    if !crate::control::jive::radio::providers()
        .iter()
        .any(|p| p.tag == tag)
    {
        return Err(CommandError::Invalid(format!("unknown radio item {id}")));
    }
    Ok((tag, rest))
}

/// `/radio`: the radio providers (`radios`), then each one's menu (`<tag> items`), with the
/// provider's tag leading the item ids.
pub async fn radio(server: &Arc<Server>, p: MenuParams) -> Result<Page<BrowseItem>, CommandError> {
    let Some(id) = p.id.clone().filter(|s| !s.is_empty()) else {
        let r = run(server, vec!["radios".into(), "0".into(), "100".into()]).await?;
        let items: Vec<BrowseItem> = r
            .get("radioss_loop")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|row| {
                        Some(BrowseItem {
                            id: string(row.get("cmd"))?,
                            name: string(row.get("name")).unwrap_or_default(),
                            kind: BrowseItemType::Folder,
                            playable: false,
                            url: None,
                            artwork: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total = items.len();
        let items = items
            .into_iter()
            .skip(p.offset)
            .take(p.limit.unwrap_or(LIMIT).min(MAX_LIMIT))
            .collect();
        return Ok(Page {
            total,
            offset: p.offset,
            items,
        });
    };
    let (tag, rest) = radio_id(&id)?;
    let mut page = menu(
        server,
        tag,
        MenuParams {
            id: Some(rest.to_string()),
            ..p
        },
    )
    .await?;
    for it in &mut page.items {
        it.id = format!("{tag}.{}", it.id);
    }
    Ok(page)
}

fn browse_item(row: &Map<String, Value>) -> Option<BrowseItem> {
    let flag = |k: &str| int(row.get(k)).unwrap_or(0) != 0;
    let kind_str = row.get("type").and_then(Value::as_str).unwrap_or("");
    let (audio, has_items) = (flag("isaudio"), flag("hasitems"));
    let kind = match kind_str {
        "search" => BrowseItemType::Search,
        "text" => BrowseItemType::Text,
        _ if has_items && !audio => BrowseItemType::Folder,
        _ if audio || kind_str == "playlist" || kind_str == "audio" => BrowseItemType::Audio,
        _ if has_items => BrowseItemType::Folder,
        _ => BrowseItemType::Text,
    };
    Some(BrowseItem {
        id: string(row.get("id"))?,
        name: string(row.get("name"))
            .or_else(|| string(row.get("title")))
            .unwrap_or_default(),
        kind,
        playable: audio || kind_str == "playlist",
        url: string(row.get("url")),
        artwork: string(row.get("image")).map(|u| artwork_path(&u)),
    })
}

pub async fn add_favorite(server: &Arc<Server>, f: NewFavorite) -> Result<(), CommandError> {
    let (url, title) = (f.url.trim(), f.title.trim());
    if url.is_empty() {
        return Err(CommandError::Invalid("url must not be empty".into()));
    }
    let title = if title.is_empty() { url } else { title };
    run(
        server,
        vec![
            "favorites".into(),
            "add".into(),
            format!("url:{}", url),
            format!("title:{}", title),
        ],
    )
    .await?;
    Ok(())
}

pub async fn delete_favorite(server: &Arc<Server>, id: &str) -> Result<(), CommandError> {
    run(
        server,
        vec![
            "favorites".into(),
            "delete".into(),
            format!("item_id:{}", id),
        ],
    )
    .await?;
    Ok(())
}

pub fn spotify_available() -> bool {
    crate::plugins::spotify::get().is_some()
}

pub async fn spotify_status(server: &Arc<Server>) -> Result<SpotifyStatus, CommandError> {
    let r = run(server, vec!["spotify".into(), "status".into()]).await?;
    Ok(SpotifyStatus {
        signed_in: int(r.get("signed_in")).unwrap_or(0) != 0,
        playback: int(r.get("playback")).unwrap_or(0) != 0,
        browsing_own_app: int(r.get("browsing_own_app")).unwrap_or(0) != 0,
        user: string(r.get("user")),
        product: string(r.get("product")),
        error: string(r.get("error")),
        connect_error: string(r.get("connect_error")),
        learn: crate::plugins::spotify::learn::enabled(server),
        learned: server.library.remote_totals("spotify").analyzed.max(0) as u64,
    })
}

pub async fn spotify_patch(
    server: &Arc<Server>,
    patch: SpotifyPatch,
) -> Result<SpotifyStatus, CommandError> {
    if let Some(on) = patch.learn {
        server
            .prefs
            .server
            .set(crate::plugins::spotify::learn::PREF, serde_json::json!(on));
    }
    spotify_status(server).await
}

/// The learned Spotify tracks, the latest first.
pub async fn spotify_learned(server: &Arc<Server>) -> Result<LearnedTracks, CommandError> {
    let lib = server.library.clone();
    let tracks = crate::util::spawn_blocking(move || lib.learned_remote("spotify"))
        .await
        .map_err(|e| CommandError::Failed(e.to_string()))?;
    let items = tracks
        .into_iter()
        .map(|t| LearnedTrack {
            url: t.url,
            title: t.title,
            artist: t.artists.join(", "),
            album: t.album,
            year: t.year,
            duration: t.duration,
            artwork: t.cover,
            bpm: t.bpm,
            key: t.camelot,
            loudness: t.loudness,
            genres: t.genres,
            style: t.style,
            in_library: t.in_library,
            learned_at: t.learned_at,
        })
        .collect();
    Ok(LearnedTracks { items })
}

/// Forget the learned Spotify tracks.
pub async fn spotify_forget(server: &Arc<Server>) -> Result<(), CommandError> {
    let lib = server.library.clone();
    crate::util::spawn_blocking(move || lib.forget_remote("spotify"))
        .await
        .map_err(|e| CommandError::Failed(e.to_string()))?
        .map(|_| ())
        .map_err(|e| CommandError::Failed(e.to_string()))
}

pub async fn spotify_login(
    server: &Arc<Server>,
    body: SpotifyLogin,
) -> Result<SpotifyLoginResult, CommandError> {
    let mut words = vec!["spotify".to_string(), "login".into()];
    if let Some(code) = body
        .code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        words.push(format!("code:{}", code));
    } else if body.playback == Some(true) {
        words.push("playback:1".into());
    }
    let r = run(server, words).await?;
    if let Some(e) = string(r.get("error")) {
        return Err(CommandError::Failed(e));
    }
    Ok(SpotifyLoginResult {
        url: string(r.get("url")),
        user: string(r.get("user")),
    })
}

pub async fn spotify_logout(server: &Arc<Server>) -> Result<(), CommandError> {
    run(server, vec!["spotify".into(), "logout".into()]).await?;
    Ok(())
}

// ---------------------------------------------------------------- ListenBrainz

/// The ListenBrainz menu (recommendations and the playlists made for each account's user)
/// as a page of `BrowseItem`s.
pub async fn listenbrainz(
    server: &Arc<Server>,
    p: MenuParams,
) -> Result<Page<BrowseItem>, CommandError> {
    let items = crate::plugins::listenbrainz::menu(server, p.id.as_deref())
        .await
        .map_err(CommandError::Invalid)?;
    let total = items.len();
    let items = items
        .into_iter()
        .skip(p.offset)
        .take(p.limit.unwrap_or(LIMIT).min(MAX_LIMIT))
        .map(|m| BrowseItem {
            id: m.id,
            name: m.name,
            kind: if m.folder {
                BrowseItemType::Folder
            } else {
                BrowseItemType::Audio
            },
            playable: true,
            url: None,
            artwork: None,
        })
        .collect();
    Ok(Page {
        total,
        offset: p.offset,
        items,
    })
}

pub async fn add_listenbrainz_account(
    server: &Arc<Server>,
    a: NewListenBrainzAccount,
) -> Result<ListenBrainzAccount, CommandError> {
    let a = crate::plugins::scrobble::add_account(server, &a.token)
        .await
        .map_err(CommandError::Failed)?;
    Ok(ListenBrainzAccount {
        user: a.user,
        default: a.default,
    })
}

pub fn patch_listenbrainz_account(
    server: &Server,
    user: &str,
    patch: ListenBrainzAccountPatch,
) -> Result<(), CommandError> {
    if !crate::plugins::scrobble::set_default(server, user, patch.default) {
        return Err(CommandError::Invalid(format!(
            "no ListenBrainz account {user}"
        )));
    }
    Ok(())
}

pub fn remove_listenbrainz_account(server: &Server, user: &str) -> Result<(), CommandError> {
    if !crate::plugins::scrobble::remove_account(server, user) {
        return Err(CommandError::Invalid(format!(
            "no ListenBrainz account {user}"
        )));
    }
    Ok(())
}
