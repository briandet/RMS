//! Cover art from the [Cover Art Archive](https://coverartarchive.org) for an album's
//! MusicBrainz release (or else its release group): what images there are, and the one the
//! user picks, downloaded for `tagedit` to write.  Listings are cached for a day.  Only
//! images the archive lists for the album's release are fetched, never a URL from a client.

use crate::server::Server;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// How long a listing is kept.
const LISTING_AGE: i64 = 86400;
/// The largest image taken.
const MAX_BYTES: usize = 20 << 20;

/// An image of a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// The archive's id of the image.
    pub id: String,
    /// `Front`, `Back`, `Booklet`, `Medium`, …
    pub types: Vec<String>,
    pub front: bool,
    /// About 250–500 pixels, to choose from.
    pub thumbnail: String,
    /// About 1200 pixels (the original when there is no such size), to keep.
    pub image: String,
}

/// The images of the release (or, when it has none, of its release group), front first.
pub async fn images(
    server: &Server,
    release: &str,
    release_group: Option<&str>,
) -> Result<Vec<Image>, String> {
    let mut out = listing(server, "release", release).await?;
    if out.is_empty() {
        if let Some(rg) = release_group.filter(|g| !g.is_empty()) {
            out = listing(server, "release-group", rg).await?;
        }
    }
    out.sort_by_key(|i| !i.front);
    Ok(out)
}

async fn listing(server: &Server, kind: &str, mbid: &str) -> Result<Vec<Image>, String> {
    let key = format!("coverart:{kind}:{mbid}");
    if let Some(body) = server.library.cached(&key, LISTING_AGE) {
        return Ok(serde_json::from_str(&body).unwrap_or_default());
    }
    let base = server.config.coverart_url.trim_end_matches('/');
    let resp = server
        .http
        .get(format!("{base}/{kind}/{mbid}"))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("Cover Art Archive: {e}"))?;
    let images = match resp.status().as_u16() {
        // no art for it
        404 => Vec::new(),
        s if s >= 400 => return Err(format!("Cover Art Archive: HTTP {s}")),
        _ => {
            let v: Value = resp
                .json()
                .await
                .map_err(|e| format!("Cover Art Archive: {e}"))?;
            parse(&v)
        }
    };
    server
        .library
        .cache(&key, &serde_json::to_string(&images).unwrap_or_default());
    Ok(images)
}

/// The images of a listing (`{"images": [{"id", "types", "front", "image", "thumbnails"}]}`).
pub fn parse(v: &Value) -> Vec<Image> {
    let s = |v: &Value| v.as_str().map(str::to_string);
    v["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let original = s(&i["image"])?;
            let t = &i["thumbnails"];
            Some(Image {
                id: i["id"]
                    .as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| s(&i["id"]))?,
                types: i["types"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(&s)
                    .collect(),
                front: i["front"].as_bool().unwrap_or(false),
                thumbnail: s(&t["500"])
                    .or_else(|| s(&t["large"]))
                    .or_else(|| s(&t["250"]))
                    .or_else(|| s(&t["small"]))
                    .unwrap_or_else(|| original.clone()),
                image: s(&t["1200"]).unwrap_or(original),
            })
        })
        .collect()
}

/// Download an image and check it is one: its bytes and MIME type.
pub async fn download(server: &Server, image: &Image) -> Result<(Vec<u8>, String), String> {
    let resp = server
        .http
        .get(&image.image)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("Cover Art Archive: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Cover Art Archive: HTTP {}",
            resp.status().as_u16()
        ));
    }
    if resp.content_length().unwrap_or(0) as usize > MAX_BYTES {
        return Err("the image is too large".into());
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Cover Art Archive: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("the image is too large".into());
    }
    let mime = match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Jpeg) => "image/jpeg",
        Ok(image::ImageFormat::Png) => "image/png",
        _ => return Err("not a JPEG or PNG image".into()),
    };
    // (decodes: not just the right first bytes)
    image::load_from_memory(&bytes).map_err(|e| format!("a broken image: {e}"))?;
    Ok((bytes.to_vec(), mime.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listings_are_read() {
        let v = json!({"images": [
            {"id": 2, "types": ["Back"], "front": false, "image": "http://x/2.jpg",
             "thumbnails": {"250": "http://x/2-250.jpg", "500": "http://x/2-500.jpg"}},
            {"id": 1, "types": ["Front"], "front": true, "image": "http://x/1.jpg",
             "thumbnails": {"500": "http://x/1-500.jpg", "1200": "http://x/1-1200.jpg"}},
            {"types": ["Front"]}
        ]});
        let i = parse(&v);
        assert_eq!(i.len(), 2);
        assert_eq!(i[0].thumbnail, "http://x/2-500.jpg");
        assert_eq!(i[0].image, "http://x/2.jpg", "no 1200: the original");
        assert_eq!(i[1].image, "http://x/1-1200.jpg");
        assert!(i[1].front);
        assert_eq!(i[1].types, ["Front"]);
    }
}
