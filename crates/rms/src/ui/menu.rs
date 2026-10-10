//! Reading Jive (SqueezePlay) menu items for the player UI: item kinds, the
//! `base`/`itemsParams` action protocol (as `SlimBrowser` applies it), and building the
//! command tokens of an action.

use serde_json::{Map, Value};

/// Symbols of Lyrion's display fonts.
pub const NOTESYMBOL: &str = "\x01";
pub const RIGHTARROW: &str = "\x02";
pub const CURSORPOS: &str = "\x0a";
const CIRCLE: &str = "\x0c";
const FILLEDCIRCLE: &str = "\x0d";
const SQUARE: &str = "\x0e";
const FILLEDSQUARE: &str = "\x0f";

/// What pressing right on an item does.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// RMS pseudo item (e.g. `nowplaying`).
    Rms(String),
    /// A node of the home menu tree.
    Node(String),
    /// Items given inline (`item_loop` inside the item).
    Inline,
    Choice,
    Checkbox(bool),
    Radio(bool),
    Slider,
    Input,
    /// Has a `go` (or `more` for tracks) action.
    Go,
    /// Has only a `do` action.
    Do,
    /// Plain text.
    Text,
}

pub fn text(item: &Value) -> String {
    let t = item
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| item.get("name").and_then(Value::as_str))
        .unwrap_or("");
    t.split('\n').next().unwrap_or("").to_string()
}

fn flag(item: &Value, key: &str) -> Option<bool> {
    item.get(key).map(|v| match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !(s.is_empty() || s == "0"),
        _ => false,
    })
}

pub fn kind(item: &Value, base: Option<&Value>) -> Kind {
    if let Some(r) = item.get("rms").and_then(Value::as_str) {
        return Kind::Rms(r.to_string());
    }
    if flag(item, "isANode") == Some(true) {
        return Kind::Node(
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
    }
    if item.get("item_loop").is_some_and(Value::is_array) {
        return Kind::Inline;
    }
    if item.get("choiceStrings").is_some() {
        return Kind::Choice;
    }
    if let Some(c) = flag(item, "checkbox") {
        return Kind::Checkbox(c);
    }
    if let Some(r) = flag(item, "radio") {
        return Kind::Radio(r);
    }
    if flag(item, "slider") == Some(true) {
        return Kind::Slider;
    }
    if item.get("input").is_some_and(Value::is_object) {
        return Kind::Input;
    }
    if action(item, base, "go").is_some() || is_audio(item) && action(item, base, "more").is_some()
    {
        return Kind::Go;
    }
    if action(item, base, "do").is_some() {
        return Kind::Do;
    }
    Kind::Text
}

pub fn is_audio(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("audio")
}

/// Right-hand overlay for an item on line 2.
pub fn overlay(item: &Value, base: Option<&Value>) -> String {
    if let Some(o) = item.get("rms_overlay").and_then(Value::as_str) {
        return o.to_string();
    }
    match kind(item, base) {
        Kind::Checkbox(on) => if on { FILLEDSQUARE } else { SQUARE }.into(),
        Kind::Radio(on) => if on { FILLEDCIRCLE } else { CIRCLE }.into(),
        _ if is_audio(item) => NOTESYMBOL.into(),
        // actions that open no further list (play, add …) get no arrow
        Kind::Go
            if action(item, base, "go")
                .as_ref()
                .and_then(next_window)
                .is_some() =>
        {
            String::new()
        }
        Kind::Node(_) | Kind::Inline | Kind::Choice | Kind::Slider | Kind::Input | Kind::Go => {
            RIGHTARROW.into()
        }
        Kind::Rms(_) => RIGHTARROW.into(),
        _ if item.get("type").and_then(Value::as_str) == Some("playlist") => NOTESYMBOL.into(),
        _ => String::new(),
    }
}

/// The action `name` of an item: the item's own, or the list's `base` action with the
/// item's `itemsParams` merged in (unavailable when the item lacks them).  `<name>Action`
/// on the item redirects to another action (e.g. `goAction: play`).
pub fn action(item: &Value, base: Option<&Value>, name: &str) -> Option<Value> {
    let name = item
        .get(format!("{name}Action"))
        .and_then(Value::as_str)
        .unwrap_or(name);
    if let Some(a) = item.get("actions").and_then(|a| a.get(name)) {
        if a.is_object() {
            return Some(a.clone());
        }
    }
    let b = base?.get("actions")?.get(name)?;
    let mut a = b.clone();
    if let Some(ip) = b.get("itemsParams").and_then(Value::as_str) {
        let extra = item.get(ip)?.as_object()?.clone();
        let params = a
            .as_object_mut()?
            .entry("params")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(p) = params.as_object_mut() {
            p.extend(extra);
        }
    }
    Some(a)
}

fn token(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some((*b as i32).to_string()),
        other => Some(other.to_string()),
    }
}

/// Command tokens for an action: `cmd`, optionally `start quantity`, then `key:value`
/// params with `__INPUT__` / `__TAGGEDINPUT__` replaced by `input`.
pub fn tokens(action: &Value, range: Option<(usize, usize)>, input: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = action
        .get("cmd")
        .and_then(Value::as_array)
        .map(|c| c.iter().filter_map(token).collect())
        .unwrap_or_default();
    if let Some((start, qty)) = range {
        out.push(start.to_string());
        out.push(qty.to_string());
    }
    if let Some(params) = action.get("params").and_then(Value::as_object) {
        for (k, v) in params {
            let Some(mut v) = token(v) else { continue };
            if let Some(i) = input {
                v = v.replace("__TAGGEDINPUT__", i).replace("__INPUT__", i);
            }
            out.push(format!("{k}:{v}"));
        }
    }
    out
}

/// `nextWindow` of an action, if any.
pub fn next_window(action: &Value) -> Option<&str> {
    action.get("nextWindow").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_actions_with_items_params() {
        let base = json!({ "actions": {
            "go": { "cmd": ["browselibrary", "items"], "params": { "menu": 1, "mode": "albums" }, "itemsParams": "commonParams" },
            "add": { "cmd": ["playlistcontrol"], "params": { "cmd": "add" }, "itemsParams": "addallParams" },
            "play": { "cmd": ["playlistcontrol"], "params": { "cmd": "load" }, "itemsParams": "playallParams" },
        } });
        let artist =
            json!({ "text": "The Beatles\nsecond line", "commonParams": { "artist_id": 2 } });
        let go = action(&artist, Some(&base), "go").unwrap();
        let t = tokens(&go, Some((0, 100)), None);
        assert_eq!(&t[..4], &["browselibrary", "items", "0", "100"]);
        assert!(t.contains(&"artist_id:2".to_string()) && t.contains(&"mode:albums".to_string()));
        // no addallParams on the item: add is unavailable
        assert!(action(&artist, Some(&base), "add").is_none());
        assert_eq!(text(&artist), "The Beatles");
        assert_eq!(kind(&artist, Some(&base)), Kind::Go);
        assert_eq!(overlay(&artist, Some(&base)), RIGHTARROW);

        let track =
            json!({ "type": "audio", "goAction": "play", "playallParams": { "play_index": 3 } });
        let go = action(&track, Some(&base), "go").unwrap();
        assert!(tokens(&go, None, None).contains(&"play_index:3".to_string()));
        assert_eq!(overlay(&track, Some(&base)), NOTESYMBOL);
    }

    #[test]
    fn inputs_and_settings_items() {
        let search = json!({ "text": "Search", "input": { "len": 1 },
            "actions": { "go": { "cmd": ["globalsearch", "items"], "params": { "menu": "globalsearch", "search": "__TAGGEDINPUT__" } } } });
        assert_eq!(kind(&search, None), Kind::Input);
        let go = action(&search, None, "go").unwrap();
        assert!(tokens(&go, Some((0, 10)), Some("abba")).contains(&"search:abba".to_string()));
        assert_eq!(
            kind(&json!({ "radio": 1, "actions": { "do": {} } }), None),
            Kind::Radio(true)
        );
        assert_eq!(kind(&json!({ "checkbox": 0 }), None), Kind::Checkbox(false));
        assert_eq!(
            kind(&json!({ "isANode": 1, "id": "myMusic" }), None),
            Kind::Node("myMusic".into())
        );
        assert_eq!(kind(&json!({ "text": "plain" }), None), Kind::Text);
    }
}
