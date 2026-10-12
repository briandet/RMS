//! Where an album's files belong: a path template filled in from their tags.
//!
//! A template is a path relative to the music folder holding the file, without the file's
//! extension: `{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}`.  `{name}` is a
//! tag ([`FIELDS`]); `{name:0N}` pads a number with zeros to N digits.  A part in `[…]` is
//! left out when a tag in it is empty, e.g. the year of an undated album, or the disc of a
//! single-disc one (`{disc}` is only given when the album has several discs).  Tags are made
//! safe for file names: no path separators, no characters Windows and Samba refuse, no
//! leading dots (hidden files are not scanned).

use crate::tagwrite::Values;
use std::path::{Path, PathBuf};

pub const DEFAULT_TEMPLATE: &str = "{album_artist}/{album}[ ({year})]/[{disc}-]{track:02} {title}";

/// The names a template can use.
pub const FIELDS: &[&str] = &[
    "title",
    "artist",
    "album",
    "album_artist",
    "track",
    "track_total",
    "disc",
    "disc_total",
    "year",
    "date",
    "original_year",
    "original_date",
    "label",
    "catalog_number",
];

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Field { name: String, width: usize },
    Group(Vec<Node>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template(Vec<Node>);

impl Template {
    pub fn parse(s: &str) -> Result<Template, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("the template is empty".into());
        }
        if s.starts_with('/') || s.starts_with('\\') {
            return Err("the template is relative to the music folder: no leading /".into());
        }
        let mut chars = s.chars().peekable();
        let nodes = parse_nodes(&mut chars, false)?;
        if chars.next().is_some() {
            return Err("unbalanced ]".into());
        }
        Ok(Template(nodes))
    }

    /// The path for a file with these tags, relative to its music folder, without extension.
    pub fn render(&self, get: &dyn Fn(&str) -> Option<String>) -> PathBuf {
        let mut out = String::new();
        render_nodes(&self.0, get, &mut out);
        let parts: Vec<String> = out
            .split('/')
            .map(sanitize_component)
            .filter(|c| !c.is_empty())
            .collect();
        let mut p = PathBuf::new();
        for c in parts {
            p.push(c);
        }
        p
    }
}

fn parse_nodes(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    in_group: bool,
) -> Result<Vec<Node>, String> {
    let mut nodes = Vec::new();
    let mut text = String::new();
    while let Some(&c) = chars.peek() {
        match c {
            '{' => {
                chars.next();
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                let mut spec = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => spec.push(c),
                        None => return Err("unclosed {".into()),
                    }
                }
                let (name, width) = match spec.split_once(':') {
                    Some((n, w)) => (
                        n.trim(),
                        w.trim()
                            .parse::<usize>()
                            .ok()
                            .filter(|w| *w <= 9)
                            .ok_or_else(|| {
                                format!("bad width in {{{spec}}}: use e.g. {{track:02}}")
                            })?,
                    ),
                    None => (spec.trim(), 0),
                };
                if !FIELDS.contains(&name) {
                    return Err(format!(
                        "unknown tag {{{name}}}: use one of {}",
                        FIELDS.join(", ")
                    ));
                }
                nodes.push(Node::Field {
                    name: name.to_string(),
                    width,
                });
            }
            '[' => {
                chars.next();
                if !text.is_empty() {
                    nodes.push(Node::Text(std::mem::take(&mut text)));
                }
                let inner = parse_nodes(chars, true)?;
                if chars.next() != Some(']') {
                    return Err("unclosed [".into());
                }
                nodes.push(Node::Group(inner));
            }
            ']' if in_group => break,
            ']' => return Err("unbalanced ]".into()),
            '}' => return Err("unbalanced }".into()),
            _ => {
                chars.next();
                text.push(c);
            }
        }
    }
    if !text.is_empty() {
        nodes.push(Node::Text(text));
    }
    Ok(nodes)
}

/// Render `nodes`; false when a tag in them is empty.
fn render_nodes(nodes: &[Node], get: &dyn Fn(&str) -> Option<String>, out: &mut String) -> bool {
    let mut complete = true;
    for n in nodes {
        match n {
            Node::Text(t) => out.push_str(t),
            Node::Field { name, width } => match get(name) {
                Some(v) if !v.trim().is_empty() => {
                    let v = clean_value(&v);
                    let v = match (*width, leading_number(&v)) {
                        (w, Some(n)) if w > 0 => format!("{n:0w$}"),
                        _ => v,
                    };
                    out.push_str(&v);
                }
                _ => complete = false,
            },
            Node::Group(inner) => {
                let mut s = String::new();
                if render_nodes(inner, get, &mut s) {
                    out.push_str(&s);
                }
            }
        }
    }
    complete
}

/// "3/12" → 3.
fn leading_number(s: &str) -> Option<u64> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// A tag's value as part of a name: one value, no path separators.
fn clean_value(v: &str) -> String {
    // several values ("A; B"): the first
    let v = v.split("; ").next().unwrap_or(v).trim();
    v.chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

/// A safe file or folder name.
pub fn sanitize_component(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    // leave room for an extension in a 255-byte name
    let mut out = String::new();
    for c in trimmed.chars() {
        if out.len() + c.len_utf8() > 200 {
            break;
        }
        out.push(c);
    }
    out.trim_end_matches(['.', ' ']).to_string()
}

/// A file of an album to place.
pub struct Source<'a> {
    pub path: &'a Path,
    pub tags: &'a Values,
}

/// Where each of an album's files belongs under `root` (its music folder).
pub fn place(template: &Template, root: &Path, files: &[Source]) -> Vec<PathBuf> {
    let num = |v: Option<&str>| v.and_then(leading_number).unwrap_or(0);
    let tag = |t: &Values, k: &str| t.get(k).cloned().flatten().filter(|v| !v.trim().is_empty());
    // the disc number only when the album has several
    let discs = files.iter().any(|f| {
        num(tag(f.tags, "disc_total").as_deref()) > 1
            || num(tag(f.tags, "disc_number").as_deref()) > 1
    });
    files
        .iter()
        .map(|f| {
            let t = f.tags;
            let get = |name: &str| -> Option<String> {
                match name {
                    "title" => tag(t, "title")
                        .or_else(|| f.path.file_stem().map(|s| s.to_string_lossy().to_string())),
                    "artist" => tag(t, "artist").or_else(|| Some("Unknown Artist".into())),
                    "album_artist" => tag(t, "album_artist")
                        .or_else(|| tag(t, "artist"))
                        .or_else(|| Some("Unknown Artist".into())),
                    "album" => tag(t, "album").or_else(|| Some("Unknown Album".into())),
                    "track" => tag(t, "track_number"),
                    "disc" => tag(t, "disc_number").filter(|_| discs),
                    "year" => tag(t, "date").and_then(|d| d.get(..4).map(str::to_string)),
                    "original_year" => {
                        tag(t, "original_date").and_then(|d| d.get(..4).map(str::to_string))
                    }
                    other => tag(t, other),
                }
            };
            let mut p = root.join(template.render(&get));
            if let Some(ext) = f.path.extension() {
                let name = format!(
                    "{}.{}",
                    p.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default(),
                    ext.to_string_lossy().to_lowercase()
                );
                p.set_file_name(name);
            }
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Some(v.to_string())))
            .collect()
    }

    fn placed(template: &str, files: &[(&str, Values)]) -> Vec<String> {
        let t = Template::parse(template).unwrap();
        let paths: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
        let sources: Vec<Source> = files
            .iter()
            .zip(&paths)
            .map(|((_, t), p)| Source { path: p, tags: t })
            .collect();
        place(&t, Path::new("/m"), &sources)
            .into_iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    #[test]
    fn files_go_where_the_template_says() {
        let a = tags(&[
            ("title", "Smells Like Teen Spirit"),
            ("artist", "Nirvana"),
            ("album", "Nevermind"),
            ("track_number", "1/12"),
            ("date", "1991-09-24"),
        ]);
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/x/01.FLAC", a)]),
            ["/m/Nirvana/Nevermind (1991)/01 Smells Like Teen Spirit.flac"]
        );
    }

    #[test]
    fn missing_tags_leave_out_their_part_or_fall_back() {
        let a = tags(&[("title", "Song"), ("track_number", "3")]);
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/a.mp3", a)]),
            ["/m/Unknown Artist/Unknown Album/03 Song.mp3"]
        );
        let b = tags(&[("album", "X"), ("artist", "Y")]);
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/old name.ogg", b)]),
            ["/m/Y/X/old name.ogg"]
        );
    }

    #[test]
    fn the_disc_shows_only_for_albums_of_several() {
        let one = tags(&[
            ("title", "A"),
            ("album", "Al"),
            ("artist", "Ar"),
            ("track_number", "1"),
            ("disc_number", "1"),
        ]);
        let two = tags(&[
            ("title", "B"),
            ("album", "Al"),
            ("artist", "Ar"),
            ("track_number", "1"),
            ("disc_number", "2"),
        ]);
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/1.flac", one.clone())]),
            ["/m/Ar/Al/01 A.flac"]
        );
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/1.flac", one), ("/m/2.flac", two)]),
            ["/m/Ar/Al/1-01 A.flac", "/m/Ar/Al/2-01 B.flac"]
        );
    }

    #[test]
    fn names_are_made_safe() {
        let a = tags(&[
            ("title", "What? / Why: \"Now\""),
            ("artist", "AC/DC"),
            ("album", "..Hidden..."),
            ("track_number", "1"),
        ]);
        assert_eq!(
            placed(DEFAULT_TEMPLATE, &[("/m/a.mp3", a)]),
            ["/m/AC-DC/Hidden/01 What_ - Why_ _Now_.mp3"]
        );
        // nothing can climb out of the music folder
        let b = tags(&[("album", ".."), ("artist", "../.."), ("title", "x")]);
        let p = placed("{artist}/{album}/../{title}", &[("/m/a.mp3", b)]);
        assert!(p[0].starts_with("/m/") && !p[0].contains(".."), "{p:?}");
    }

    #[test]
    fn bad_templates_are_refused() {
        assert!(Template::parse("{nope}").is_err());
        assert!(Template::parse("{title").is_err());
        assert!(Template::parse("[{title}").is_err());
        assert!(Template::parse("{title}]").is_err());
        assert!(Template::parse("/abs/{title}").is_err());
        assert!(Template::parse("{track:xx}").is_err());
        assert!(Template::parse(DEFAULT_TEMPLATE).is_ok());
    }
}
