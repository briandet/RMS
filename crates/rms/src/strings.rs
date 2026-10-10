//! English UI strings (`strings.txt`, EN column), used for menu text and `getstring`.

use std::collections::HashMap;
use std::sync::OnceLock;

static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn table() -> &'static HashMap<&'static str, &'static str> {
    TABLE.get_or_init(|| {
        include_str!("strings_en.txt")
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .collect()
    })
}

/// Localised (English) string for a token.
pub fn get(token: &str) -> Option<&'static str> {
    table().get(token).copied()
}

/// String for a token, falling back to the token itself.
pub fn s(token: &str) -> String {
    get(token).map(|v| v.replace("\\n", "\n")).unwrap_or_else(|| token.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lookup() {
        assert_eq!(super::get("MY_MUSIC"), Some("My Music"));
        assert_eq!(super::s("NOPE_NOT_A_TOKEN"), "NOPE_NOT_A_TOKEN");
    }
}
