//! Making and changing saved playlists (`/api/v1/playlists`): create one from a library item
//! or a player's queue, rename, delete, reorder or remove its tracks, add to it.  Only
//! playlists saved in the playlist folder can be changed; a playlist file among the music or
//! a cue sheet is the music's own.

use super::commands::{item_entries, CommandError};
use super::library::{playlist_by_id, LibraryError};
use super::model::{NewPlaylist, Playlist, PlaylistAdd, PlaylistOrder, PlaylistPatch};
use crate::server::Server;
use std::sync::Arc;

fn failed(e: impl std::fmt::Display) -> LibraryError {
    LibraryError::Failed(e.to_string())
}

fn invalid(e: CommandError) -> LibraryError {
    match e {
        CommandError::Invalid(m) | CommandError::Failed(m) => LibraryError::Invalid(m),
    }
}

async fn blocking<T: Send + 'static>(
    server: &Arc<Server>,
    f: impl FnOnce(&rms_library::Library) -> T + Send + 'static,
) -> Result<T, LibraryError> {
    let lib = server.library.clone();
    tokio::task::spawn_blocking(move || f(&lib))
        .await
        .map_err(failed)
}

/// The playlist, if it exists and can be changed.
async fn editable(server: &Arc<Server>, id: i64) -> Result<Playlist, LibraryError> {
    let p = playlist_by_id(server, id).await?;
    if !p.editable {
        return Err(LibraryError::Invalid(
            "this playlist is a file among the music and can't be changed here; \
             save a copy (create a playlist from it) to edit"
                .into(),
        ));
    }
    Ok(p)
}

fn check_name(name: &str) -> Result<String, LibraryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::Invalid("a playlist needs a name".into()));
    }
    Ok(name.to_string())
}

async fn name_taken(
    server: &Arc<Server>,
    name: &str,
    except: Option<i64>,
) -> Result<(), LibraryError> {
    let n = name.to_string();
    if let Some(other) = blocking(server, move |l| l.playlist_by_name(&n)).await? {
        if Some(other) != except {
            return Err(LibraryError::Invalid(format!(
                "there already is a playlist named {name}"
            )));
        }
    }
    Ok(())
}

pub async fn create(server: &Arc<Server>, p: NewPlaylist) -> Result<Playlist, LibraryError> {
    let name = check_name(&p.name)?;
    name_taken(server, &name, None).await?;
    let urls: Vec<String> = match (p.item, p.from_player) {
        (Some(_), Some(_)) => {
            return Err(LibraryError::Invalid(
                "either item or from_player, not both".into(),
            ))
        }
        (Some(item), None) => item_entries(server, item)
            .await
            .map_err(invalid)?
            .into_iter()
            .map(|e| e.url)
            .collect(),
        (None, Some(player)) => {
            let pl = server
                .resolve_client(&player)
                .ok_or_else(|| LibraryError::Invalid(format!("unknown player {player}")))?;
            let ctl = pl.controller();
            let c = ctl.lock();
            c.playlist.entries.iter().map(|e| e.url.clone()).collect()
        }
        (None, None) => Vec::new(),
    };
    let id = blocking(server, move |l| l.save_playlist(&name, &urls))
        .await?
        .map_err(failed)?;
    playlist_by_id(server, id).await
}

pub async fn rename(
    server: &Arc<Server>,
    id: i64,
    p: PlaylistPatch,
) -> Result<Playlist, LibraryError> {
    editable(server, id).await?;
    let name = check_name(&p.name)?;
    name_taken(server, &name, Some(id)).await?;
    blocking(server, move |l| l.rename_playlist(id, &name))
        .await?
        .map_err(failed)?;
    playlist_by_id(server, id).await
}

pub async fn delete(server: &Arc<Server>, id: i64) -> Result<(), LibraryError> {
    editable(server, id).await?;
    blocking(server, move |l| l.delete_playlist(id))
        .await?
        .map_err(failed)
}

pub async fn reorder(server: &Arc<Server>, id: i64, p: PlaylistOrder) -> Result<(), LibraryError> {
    editable(server, id).await?;
    blocking(server, move |l| l.reorder_playlist(id, &p.order))
        .await?
        .map_err(|e| LibraryError::Invalid(e.to_string()))
}

pub async fn add(server: &Arc<Server>, id: i64, p: PlaylistAdd) -> Result<usize, LibraryError> {
    editable(server, id).await?;
    let urls: Vec<String> = item_entries(server, p.item)
        .await
        .map_err(invalid)?
        .into_iter()
        .map(|e| e.url)
        .collect();
    if urls.is_empty() {
        return Err(LibraryError::Invalid("nothing to add".into()));
    }
    let n = urls.len();
    blocking(server, move |l| l.add_to_playlist(id, &urls, p.position))
        .await?
        .map_err(failed)?;
    Ok(n)
}
