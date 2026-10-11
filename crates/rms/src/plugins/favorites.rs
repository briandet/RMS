//! Favorites (`Slim::Plugin::Favorites`): OPML storage in `favorites.opml` with dotted
//! hierarchical ids (`0`, `1.2`, …) and the `favorites` CLI commands.

use crate::control::dispatch::{DispatchTable, Handler};
use crate::control::request::{normalize, Request, Results};
use crate::server::Server;
use parking_lot::RwLock;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::warn;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fav {
    pub text: String,
    pub url: Option<String>,
    /// `audio`, `playlist`, `link` (folder when it has children and no url)
    pub kind: Option<String>,
    pub icon: Option<String>,
    pub children: Vec<Fav>,
}

pub struct Favorites {
    path: PathBuf,
    root: RwLock<Vec<Fav>>,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn parse_outlines(node: roxmltree::Node) -> Vec<Fav> {
    node.children()
        .filter(|n| n.has_tag_name("outline"))
        .map(|n| Fav {
            text: n.attribute("text").unwrap_or("").to_string(),
            url: n
                .attribute("URL")
                .or(n.attribute("url"))
                .map(str::to_string),
            kind: n.attribute("type").map(str::to_string),
            icon: n.attribute("icon").map(str::to_string),
            children: parse_outlines(n),
        })
        .collect()
}

fn write_outlines(out: &mut String, items: &[Fav], depth: usize) {
    for f in items {
        let pad = "  ".repeat(depth + 2);
        let mut attrs = format!("text=\"{}\"", xml_escape(&f.text));
        if let Some(u) = &f.url {
            attrs.push_str(&format!(" URL=\"{}\"", xml_escape(u)));
        }
        if let Some(t) = &f.kind {
            attrs.push_str(&format!(" type=\"{}\"", xml_escape(t)));
        }
        if let Some(i) = &f.icon {
            attrs.push_str(&format!(" icon=\"{}\"", xml_escape(i)));
        }
        if f.children.is_empty() {
            out.push_str(&format!("{}<outline {} />\n", pad, attrs));
        } else {
            out.push_str(&format!("{}<outline {}>\n", pad, attrs));
            write_outlines(out, &f.children, depth + 1);
            out.push_str(&format!("{}</outline>\n", pad));
        }
    }
}

impl Favorites {
    pub fn load(dir: &Path) -> Favorites {
        let path = dir.join("favorites.opml");
        let mut root = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match roxmltree::Document::parse(&text) {
                Ok(doc) => {
                    if let Some(body) = doc.descendants().find(|n| n.has_tag_name("body")) {
                        root = parse_outlines(body);
                    }
                }
                Err(e) => warn!("cannot parse {}: {}", path.display(), e),
            }
        }
        Favorites {
            path,
            root: RwLock::new(root),
        }
    }

    pub fn save(&self) {
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"1.0\">\n  <head>\n    <title>Favorites</title>\n  </head>\n  <body>\n");
        write_outlines(&mut out, &self.root.read(), 0);
        out.push_str("  </body>\n</opml>\n");
        if let Err(e) = std::fs::write(&self.path, out) {
            warn!("cannot write favorites: {}", e);
        }
    }

    pub fn all(&self) -> Vec<Fav> {
        self.root.read().clone()
    }

    /// Items at a dotted path (`""` = top level).
    pub fn level(&self, id: &str) -> Option<Vec<Fav>> {
        let root = self.root.read();
        if id.is_empty() {
            return Some(root.clone());
        }
        let mut cur: &Vec<Fav> = &root;
        for part in id.split('.') {
            let i: usize = part.parse().ok()?;
            cur = &cur.get(i)?.children;
        }
        Some(cur.clone())
    }

    pub fn get(&self, id: &str) -> Option<Fav> {
        let (parent, last) = match id.rsplit_once('.') {
            Some((p, l)) => (p, l),
            None => ("", id),
        };
        self.level(parent)?
            .get(last.parse::<usize>().ok()?)
            .cloned()
    }

    fn with_level<R>(&self, id: &str, f: impl FnOnce(&mut Vec<Fav>) -> R) -> Option<R> {
        let mut root = self.root.write();
        let mut cur: &mut Vec<Fav> = &mut root;
        if !id.is_empty() {
            for part in id.split('.') {
                let i: usize = part.parse().ok()?;
                cur = &mut cur.get_mut(i)?.children;
            }
        }
        Some(f(cur))
    }

    /// Index path of the first favorite with this URL.
    pub fn find_url(&self, url: &str) -> Option<String> {
        fn walk(items: &[Fav], prefix: &str, url: &str) -> Option<String> {
            for (i, f) in items.iter().enumerate() {
                let id = if prefix.is_empty() {
                    i.to_string()
                } else {
                    format!("{}.{}", prefix, i)
                };
                if f.url.as_deref() == Some(url) {
                    return Some(id);
                }
                if let Some(x) = walk(&f.children, &id, url) {
                    return Some(x);
                }
            }
            None
        }
        walk(&self.root.read(), "", url)
    }

    pub fn add(&self, parent: &str, fav: Fav, index: Option<usize>) -> Option<String> {
        let r = self.with_level(parent, |l| {
            let i = index.unwrap_or(l.len()).min(l.len());
            l.insert(i, fav);
            i
        })?;
        self.save();
        Some(if parent.is_empty() {
            r.to_string()
        } else {
            format!("{}.{}", parent, r)
        })
    }

    pub fn delete(&self, id: &str) -> bool {
        let (parent, last) = id.rsplit_once('.').unwrap_or(("", id));
        let Ok(i) = last.parse::<usize>() else {
            return false;
        };
        let ok = self
            .with_level(parent, |l| {
                if i < l.len() {
                    l.remove(i);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
        if ok {
            self.save();
        }
        ok
    }

    pub fn rename(&self, id: &str, title: &str) -> bool {
        let (parent, last) = id.rsplit_once('.').unwrap_or(("", id));
        let Ok(i) = last.parse::<usize>() else {
            return false;
        };
        let ok = self
            .with_level(parent, |l| {
                l.get_mut(i).map(|f| f.text = title.to_string()).is_some()
            })
            .unwrap_or(false);
        if ok {
            self.save();
        }
        ok
    }

    /// Move `from` (dotted id) to position `to` (dotted id) — within or across levels.
    pub fn move_item(&self, from: &str, to: &str) -> bool {
        let Some(item) = self.get(from) else {
            return false;
        };
        if !self.delete(from) {
            return false;
        }
        let (parent, last) = to.rsplit_once('.').unwrap_or(("", to));
        let idx = last.parse::<usize>().ok();
        self.add(parent, item, idx).is_some()
    }

    /// Flattened audio favorites (title, url) — used by alarm playlists.
    pub fn flat_audio(&self) -> Vec<(String, String)> {
        fn walk(items: &[Fav], out: &mut Vec<(String, String)>) {
            for f in items {
                if let Some(u) = &f.url {
                    out.push((f.text.clone(), u.clone()));
                }
                walk(&f.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.root.read(), &mut out);
        out
    }
}

pub fn register(t: &mut DispatchTable) {
    t.add(
        &["favorites", "items", "_index", "_quantity"],
        0,
        true,
        true,
        Handler::Sync(items),
    );
    t.add(&["favorites", "add"], 0, false, true, Handler::Sync(add));
    t.add(
        &["favorites", "addlevel"],
        0,
        false,
        true,
        Handler::Sync(addlevel),
    );
    t.add(
        &["favorites", "delete"],
        0,
        false,
        true,
        Handler::Sync(delete),
    );
    t.add(
        &["favorites", "rename"],
        0,
        false,
        true,
        Handler::Sync(rename),
    );
    t.add(
        &["favorites", "move"],
        0,
        false,
        true,
        Handler::Sync(move_cmd),
    );
    t.add(
        &["favorites", "exists", "_id"],
        0,
        true,
        true,
        Handler::Sync(exists),
    );
    t.add(
        &["favorites", "playlist", "_method"],
        1,
        false,
        true,
        Handler::Async(|s, r| Box::pin(playlist(s, r))),
    );
}

fn fav_type(f: &Fav) -> &'static str {
    match (&f.url, f.kind.as_deref()) {
        (None, _) => "link",
        (Some(_), Some("playlist")) => "playlist",
        _ => "audio",
    }
}

/// `favorites items <start> <count> [item_id:x] [want_url:1]` (non-menu form).
fn items(server: &Arc<Server>, r: &mut Request) {
    if r.param("menu").is_some() {
        return crate::control::jive::favorites_menu(server, r);
    }
    let id = r.param("item_id").unwrap_or("").to_string();
    let Some(level) = server.favorites.level(&id) else {
        return r.bad_params();
    };
    let title = if id.is_empty() {
        crate::strings::s("FAVORITES")
    } else {
        server
            .favorites
            .get(&id)
            .map(|f| f.text)
            .unwrap_or_default()
    };
    r.add_result("title", title);
    r.add_result("count", level.len());
    let (valid, start, end) =
        normalize(r.param_i64("_index"), r.param_i64("_quantity"), level.len());
    if valid {
        let want_url = r.param_bool("want_url");
        for (i, f) in level.iter().enumerate().take(end + 1).skip(start) {
            let mut item = Results::new();
            item.insert(
                "id".into(),
                json!(if id.is_empty() {
                    i.to_string()
                } else {
                    format!("{}.{}", id, i)
                }),
            );
            item.insert("name".into(), json!(f.text));
            item.insert("type".into(), json!(fav_type(f)));
            if want_url {
                if let Some(u) = &f.url {
                    item.insert("url".into(), json!(u));
                }
            }
            if let Some(i) = &f.icon {
                item.insert("image".into(), json!(i));
            }
            item.insert("isaudio".into(), json!((f.url.is_some()) as i32));
            item.insert(
                "hasitems".into(),
                json!((!f.children.is_empty() || f.url.is_none()) as i32),
            );
            r.add_loop_item("loop_loop", item);
        }
    }
    r.set_done();
}

fn add(server: &Arc<Server>, r: &mut Request) {
    let (Some(url), Some(title)) = (
        r.param_nonempty("url").map(str::to_string),
        r.param_nonempty("title").map(str::to_string),
    ) else {
        return r.bad_params();
    };
    let fav = Fav {
        text: title,
        url: Some(url),
        kind: r.param("type").map(str::to_string),
        icon: r.param("icon").map(str::to_string),
        children: vec![],
    };
    let (parent, idx) = match r.param("item_id") {
        Some(id) => match id.rsplit_once('.') {
            Some((p, i)) => (p.to_string(), i.parse().ok()),
            None => (String::new(), id.parse().ok()),
        },
        None => (String::new(), None),
    };
    if let Some(id) = server.favorites.add(&parent, fav, idx) {
        r.add_result("count", 1);
        r.add_result("item_id", id);
    }
    server.notifier.notify(None, &["favorites", "changed"]);
    r.set_done();
}

fn addlevel(server: &Arc<Server>, r: &mut Request) {
    let Some(title) = r.param_nonempty("title").map(str::to_string) else {
        return r.bad_params();
    };
    let fav = Fav {
        text: title,
        ..Default::default()
    };
    let parent = r
        .param("item_id")
        .and_then(|id| id.rsplit_once('.').map(|(p, _)| p.to_string()))
        .unwrap_or_default();
    if let Some(id) = server.favorites.add(&parent, fav, None) {
        r.add_result("count", 1);
        r.add_result("item_id", id);
    }
    server.notifier.notify(None, &["favorites", "changed"]);
    r.set_done();
}

fn delete(server: &Arc<Server>, r: &mut Request) {
    let id = match (r.param("item_id"), r.param("url")) {
        (Some(id), _) => Some(id.to_string()),
        (None, Some(u)) => server.favorites.find_url(u),
        _ => None,
    };
    let Some(id) = id else { return r.bad_params() };
    server.favorites.delete(&id);
    server.notifier.notify(None, &["favorites", "changed"]);
    r.set_done();
}

fn rename(server: &Arc<Server>, r: &mut Request) {
    let (Some(id), Some(t)) = (
        r.param("item_id").map(str::to_string),
        r.param_nonempty("title").map(str::to_string),
    ) else {
        return r.bad_params();
    };
    server.favorites.rename(&id, &t);
    server.notifier.notify(None, &["favorites", "changed"]);
    r.set_done();
}

fn move_cmd(server: &Arc<Server>, r: &mut Request) {
    let (Some(from), Some(to)) = (
        r.param("from_id").map(str::to_string),
        r.param("to_id").map(str::to_string),
    ) else {
        return r.bad_params();
    };
    server.favorites.move_item(&from, &to);
    server.notifier.notify(None, &["favorites", "changed"]);
    r.set_done();
}

fn exists(server: &Arc<Server>, r: &mut Request) {
    let id = r.param("_id").unwrap_or("").to_string();
    let url = if let Ok(track_id) = id.parse::<i64>() {
        server.library.track(track_id).map(|t| t.url)
    } else {
        Some(crate::control::commands::to_url(&id))
    };
    match url.and_then(|u| server.favorites.find_url(&u)) {
        Some(idx) => {
            r.add_result("exists", 1);
            r.add_result("index", idx);
        }
        None => r.add_result("exists", 0),
    }
    r.set_done();
}

/// `favorites playlist play|add|insert|load item_id:x`.
async fn playlist(server: Arc<Server>, mut r: Request) -> Request {
    let Some(p) = crate::control::commands::player(&server, &r) else {
        r.bad_dispatch();
        return r;
    };
    let Some(id) = r.param("item_id").map(str::to_string) else {
        r.bad_params();
        return r;
    };
    let Some(fav) = server.favorites.get(&id) else {
        r.bad_params();
        return r;
    };
    let mut urls: Vec<(String, String)> = Vec::new();
    match &fav.url {
        Some(u) => urls.push((fav.text.clone(), u.clone())),
        None => {
            for c in &fav.children {
                if let Some(u) = &c.url {
                    urls.push((c.text.clone(), u.clone()));
                }
            }
        }
    }
    let mut entries = Vec::new();
    for (title, u) in urls {
        if crate::util::is_remote_url(&u) {
            server.remote_titles.insert(u.clone(), title);
        }
        entries.extend(crate::control::commands::expand_item(&server, &u).await);
    }
    let method = r.param("_method").unwrap_or("play").to_string();
    let cmd = match method.as_str() {
        "add" => "add",
        "insert" => "insert",
        _ => "load",
    };
    crate::control::commands::apply_tracks(&server, &p, cmd, entries, None, None);
    r.set_done();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opml_roundtrip_and_ids() {
        let dir = tempfile::tempdir().unwrap();
        let f = Favorites::load(dir.path());
        f.add(
            "",
            Fav {
                text: "Radio A".into(),
                url: Some("http://a".into()),
                kind: Some("audio".into()),
                ..Default::default()
            },
            None,
        );
        f.add(
            "",
            Fav {
                text: "Folder".into(),
                ..Default::default()
            },
            None,
        );
        f.add(
            "1",
            Fav {
                text: "Inner & Co".into(),
                url: Some("http://b?x=1&y=2".into()),
                ..Default::default()
            },
            None,
        );
        let g = Favorites::load(dir.path());
        assert_eq!(g.get("1.0").unwrap().text, "Inner & Co");
        assert_eq!(g.find_url("http://b?x=1&y=2").as_deref(), Some("1.0"));
        assert!(g.move_item("1.0", "0"));
        assert_eq!(g.get("0").unwrap().url.as_deref(), Some("http://b?x=1&y=2"));
        assert_eq!(g.flat_audio().len(), 2);
    }
}
