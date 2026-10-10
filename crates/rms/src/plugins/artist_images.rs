//! Online artist pictures (pref `onlineArtistImages`, off by default). Not part of LMS, where
//! plugins provide this: after a scan, artists the portrait scan found no local picture for are
//! looked up via Deezer's public search API and the picture is cached in `<cache>/artists/`.
//! Runs in the background; local pictures replace downloaded ones on the next scan.

use crate::server::Server;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

const SEARCH_URL: &str = "https://api.deezer.com/search/artist";
/// Deezer allows 50 requests per 5 seconds.
const REQUEST_GAP: Duration = Duration::from_millis(250);
/// Don't ask again for artists Deezer did not know for this long.
const MISS_TTL: Duration = Duration::from_secs(30 * 86400);
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

static RUNNING: AtomicBool = AtomicBool::new(false);

fn enabled(server: &Server) -> bool {
    server.prefs.server.get_bool("onlineArtistImages")
}

/// Start fetching missing artist pictures in the background (no-op unless enabled).
pub fn spawn_fetch(server: &Arc<Server>) {
    if !enabled(server) || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let s = server.clone();
    tokio::spawn(async move {
        let n = fetch_missing(&s).await;
        RUNNING.store(false, Ordering::SeqCst);
        if n > 0 {
            info!("downloaded {} artist pictures", n);
            s.notifier.notify(None, &["library", "changed", "1"]);
        }
    });
}

/// Normalized name for matching search results: case and punctuation insensitive.
fn norm(name: &str) -> String {
    rms_library::text::ignore_punct(name).to_lowercase()
}

/// Cache file stem for an artist.
fn cache_key(name: &str) -> String {
    format!("{:x}", md5::compute(norm(name).as_bytes()))
}

/// `picture_xl` of the search hit named exactly like `name` (Deezer's empty default
/// picture has no image hash: `.../images/artist//...`).
fn pick_picture(body: &[u8], name: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let want = norm(name);
    v.get("data")?
        .as_array()?
        .iter()
        .filter(|a| a.get("name").and_then(|n| n.as_str()).map(norm) == Some(want.clone()))
        .filter_map(|a| a.get("picture_xl").and_then(|p| p.as_str()))
        .find(|p| p.starts_with("https://") && !p.contains("/artist//"))
        .map(str::to_string)
}

fn fresh_miss(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < MISS_TTL)
}

enum Lookup {
    Found(PathBuf),
    Unknown,
    /// network trouble: stop for this run
    Failed,
}

async fn lookup(server: &Server, dir: &Path, name: &str) -> Lookup {
    let key = cache_key(name);
    let file = dir.join(format!("{}.jpg", key));
    if file.is_file() {
        return Lookup::Found(file);
    }
    let miss = dir.join(format!("{}.none", key));
    if fresh_miss(&miss) {
        return Lookup::Unknown;
    }
    tokio::time::sleep(REQUEST_GAP).await;
    let resp = server
        .http
        .get(SEARCH_URL)
        .query(&[("q", name), ("limit", "10")])
        .timeout(Duration::from_secs(15))
        .send()
        .await;
    let body = match resp {
        Ok(r) if r.status().is_success() => match r.bytes().await {
            Ok(b) => b,
            Err(_) => return Lookup::Failed,
        },
        Ok(r) => {
            debug!("artist picture search for {}: HTTP {}", name, r.status());
            return Lookup::Failed;
        }
        Err(e) => {
            debug!("artist picture search for {}: {}", name, e);
            return Lookup::Failed;
        }
    };
    let Some(url) = pick_picture(&body, name) else {
        let _ = std::fs::write(&miss, b"");
        return Lookup::Unknown;
    };
    let img = match server
        .http
        .get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r.bytes().await.ok(),
        _ => None,
    };
    let Some(img) = img.filter(|b| b.len() <= MAX_IMAGE_BYTES) else {
        return Lookup::Failed;
    };
    if !matches!(
        image::guess_format(&img),
        Ok(image::ImageFormat::Jpeg | image::ImageFormat::Png)
    ) {
        let _ = std::fs::write(&miss, b"");
        return Lookup::Unknown;
    }
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, &img).is_err() || std::fs::rename(&tmp, &file).is_err() {
        return Lookup::Failed;
    }
    Lookup::Found(file)
}

/// Look up every artist without a portrait; returns the number of pictures assigned.
async fn fetch_missing(server: &Arc<Server>) -> usize {
    let lib = server.library.clone();
    let dir = lib.artist_image_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return 0;
    }
    let l = lib.clone();
    let artists = tokio::task::spawn_blocking(move || l.contributors_without_portrait())
        .await
        .unwrap_or_default();
    let mut assigned = 0;
    for (id, name) in artists {
        if server.shutdown.is_cancelled() || !enabled(server) || lib.is_scanning() {
            break;
        }
        match lookup(server, &dir, &name).await {
            Lookup::Found(path) => {
                let l = lib.clone();
                let ok = tokio::task::spawn_blocking(move || l.set_contributor_portrait(id, &path))
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                assigned += ok as usize;
            }
            Lookup::Unknown => {}
            Lookup::Failed => break,
        }
    }
    assigned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_exact_match_with_real_picture() {
        let body = br#"{"data":[
            {"name":"Low Roar","picture_xl":"https://e-cdns-images.dzcdn.net/images/artist/aaa/1000x1000.jpg"},
            {"name":"LOW","picture_xl":"https://e-cdns-images.dzcdn.net/images/artist//1000x1000.jpg"},
            {"name":"Low.","picture_xl":"https://e-cdns-images.dzcdn.net/images/artist/bbb/1000x1000.jpg"}
        ]}"#;
        assert_eq!(
            pick_picture(body, "Low").as_deref(),
            Some("https://e-cdns-images.dzcdn.net/images/artist/bbb/1000x1000.jpg")
        );
        assert_eq!(pick_picture(body, "Nobody"), None);
        assert_eq!(pick_picture(b"not json", "Low"), None);
        assert_eq!(cache_key("R.E.M."), cache_key("r e m"));
    }
}
