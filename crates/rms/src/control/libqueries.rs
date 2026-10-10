//! Library-backed queries: thin adapters onto `rms_library::Library::query`.

use super::request::Request;
use crate::server::Server;
use rms_library::{LibQuery, LibQueryName};
use std::sync::Arc;

async fn run(server: Arc<Server>, mut r: Request, name: LibQueryName) -> Request {
    let start = r.param("_index").and_then(|s| s.parse::<usize>().ok());
    let quantity = r.param("_quantity").and_then(|s| s.parse::<usize>().ok());
    let mut params = r.tagged_params();
    // positional "term" style search arrives tagged in LMS too; keep as given
    if name == LibQueryName::Search && !params.contains_key("term") {
        if let Some(s) = params.get("search").cloned() {
            params.insert("term".into(), s);
        }
    }
    let q = LibQuery {
        name,
        start,
        quantity,
        params,
    };
    let lib = server.library.clone();
    match tokio::task::spawn_blocking(move || lib.query(&q)).await {
        Ok(Ok(results)) => {
            r.merge_results(results);
            r.set_done();
        }
        Ok(Err(e)) => {
            tracing::warn!("library query {:?} failed: {}", name, e);
            r.bad_params();
        }
        Err(e) => {
            tracing::warn!("library query {:?} panicked: {}", name, e);
            r.bad_dispatch();
        }
    }
    r
}

pub async fn albums(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Albums).await
}
pub async fn artists(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Artists).await
}
pub async fn genres(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Genres).await
}
pub async fn years(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Years).await
}
pub async fn titles(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Titles).await
}
pub async fn search(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Search).await
}
pub async fn musicfolder(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::MusicFolder).await
}
pub async fn playlists(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Playlists).await
}
pub async fn playlist_tracks(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::PlaylistTracks).await
}
pub async fn songinfo(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::SongInfo).await
}
pub async fn works(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Works).await
}
pub async fn roles(s: Arc<Server>, r: Request) -> Request {
    run(s, r, LibQueryName::Roles).await
}
