//! Finding the GraphQL queries' IDs in the Spotify web player's JavaScript.
//!
//! Each query is declared there as `"<name>","query","<id>"`, in the main script or in the
//! chunks it loads on demand (webpack's: `(names[n] || n) + "." + hashes[n] + ".js"`).  This
//! fetches the page, its main script and every chunk, and collects them.  Self-contained:
//! nothing here knows what the IDs are for.

use futures::StreamExt;
use regex::Regex;
use std::collections::HashMap;
use std::time::Duration;

const PAGE: &str = "https://open.spotify.com/";
const BUILD: &str = "https://open.spotifycdn.com/cdn/build/web-player/";
/// The page is only served to browsers.
const AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                     Chrome/130.0 Safari/537.36";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Chunks fetched at once.
const FETCHES: usize = 8;

/// The web player's queries (name → ID), and the main script they came from.
pub async fn scrape(http: &reqwest::Client) -> Result<(String, HashMap<String, String>), String> {
    let page = get(http, PAGE).await?;
    let main = main_script(&page).ok_or("no web player script on open.spotify.com")?;
    let js = get(http, &format!("{BUILD}{main}")).await?;
    let mut ops = operations(&js);
    let chunks: Vec<String> = futures::stream::iter(chunk_urls(&js))
        // a chunk that won't load: the others may still have the queries
        .map(|url| async move { get(http, &url).await.unwrap_or_default() })
        .buffer_unordered(FETCHES)
        .collect()
        .await;
    for c in &chunks {
        ops.extend(operations(c));
    }
    if ops.is_empty() {
        return Err(format!("no queries found in the web player ({main})"));
    }
    Ok((main.to_string(), ops))
}

async fn get(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = http
        .get(url)
        .header(reqwest::header::USER_AGENT, AGENT)
        .timeout(TIMEOUT)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("{url}: {e}"))?;
    resp.text().await.map_err(|e| format!("{url}: {e}"))
}

/// The main script's file name in the page.
fn main_script(page: &str) -> Option<&str> {
    let re = Regex::new(r"web-player/(web-player\.[0-9a-f]+\.js)").unwrap();
    Some(re.captures(page)?.get(1)?.as_str())
}

/// The queries declared in `js`.
fn operations(js: &str) -> HashMap<String, String> {
    let re = Regex::new(r#""([A-Za-z0-9_]+)","(?:query|mutation)","([0-9a-f]{64})""#).unwrap();
    re.captures_iter(js)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

/// The URLs of the chunks the main script loads: from its `{n:"…",…}` maps, the one whose
/// values are all 8-digit hashes giving the file hashes, the others the names.
fn chunk_urls(main: &str) -> Vec<String> {
    let map = Regex::new(r#"\{(?:\d+:"[^"]+",?){20,}\}"#).unwrap();
    let entry = Regex::new(r#"(\d+):"([^"]+)""#).unwrap();
    let hash = Regex::new(r"^[0-9a-f]{8}$").unwrap();
    let (mut hashes, mut names) = (None, HashMap::new());
    for m in map.find_iter(main) {
        let entries: Vec<(&str, &str)> = entry
            .captures_iter(m.as_str())
            .map(|c| (c.get(1).unwrap().as_str(), c.get(2).unwrap().as_str()))
            .collect();
        if entries.iter().all(|(_, v)| hash.is_match(v)) {
            // the first: the scripts' (the stylesheets' comes after)
            hashes.get_or_insert(entries);
        } else {
            names.extend(entries);
        }
    }
    hashes
        .unwrap_or_default()
        .into_iter()
        .map(|(n, h)| format!("{BUILD}{}.{h}.js", names.get(n).unwrap_or(&n)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A webpack map of `n` entries: `{100:"<f(100)>",101:…}`.
    fn map(n: usize, f: impl Fn(usize) -> String) -> String {
        let entries: Vec<String> = (100..100 + n)
            .map(|i| format!("{i}:\"{}\"", f(i)))
            .collect();
        format!("{{{}}}", entries.join(","))
    }

    #[test]
    fn chunks_are_named_from_the_name_map_and_hashed_from_the_first_hash_map() {
        let names = map(20, |i| format!("xpui-routes-{i}"));
        let scripts = map(21, |i| format!("{:08x}", i));
        let styles = map(21, |i| format!("{:08x}", i + 0xff00));
        let js = format!("a={names};b=e=>({scripts})[e]+\".js\";c={styles};");
        let urls = chunk_urls(&js);
        assert_eq!(urls.len(), 21);
        assert_eq!(urls[0], format!("{BUILD}xpui-routes-100.00000064.js"));
        // no name: the chunk's number
        assert_eq!(urls[20], format!("{BUILD}120.00000078.js"));
    }

    #[test]
    fn queries_and_mutations_are_found_by_name() {
        let id = "61b7353d3625780022dbc63d459eb64cd609e7e5e2a820f9160d65030efe2e15";
        let js = format!(
            r#"t4=new t6.l("userTopContent","query","{id}",null);x=new l("saveThing","mutation","{}",null);y=("short","query","abc")"#,
            "e".repeat(64)
        );
        let ops = operations(&js);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops["userTopContent"], id);
        assert_eq!(ops["saveThing"], "e".repeat(64));
    }

    #[test]
    fn the_main_script_is_found_in_the_page() {
        let page = r#"<script src="https://open.spotifycdn.com/cdn/build/web-player/vendor~web-player.842d320c.js"></script><script src="https://open.spotifycdn.com/cdn/build/web-player/web-player.aeaf3de5.js"></script>"#;
        assert_eq!(main_script(page), Some("web-player.aeaf3de5.js"));
        assert_eq!(main_script("<html></html>"), None);
    }
}
