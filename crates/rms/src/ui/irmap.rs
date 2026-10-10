//! IR code tables and button → function maps (`Slim::Hardware::IR::loadIRFile`,
//! `loadMapFile`), read from Lyrion's own `IR/Slim_Devices_Remote.ir`, `IR/Front_Panel.ir`
//! and `IR/Default.map`.

use std::collections::HashMap;
use std::sync::OnceLock;

const REMOTE: &str = include_str!("../../assets/ir/Slim_Devices_Remote.ir");
const FRONT_PANEL: &str = include_str!("../../assets/ir/Front_Panel.ir");
const DEFAULT_MAP: &str = include_str!("../../assets/ir/Default.map");

/// `@buttonPressStyles`: what `button.*` expands to.
const PRESS_STYLES: &[&str] = &[
    "",
    ".single",
    ".double",
    ".repeat",
    ".hold",
    ".hold_release",
];

pub struct Maps {
    /// IR code (lower case hex, 8 digits) → button name.
    codes: HashMap<String, String>,
    /// mode → button (with press style) → function.
    map: HashMap<String, HashMap<String, String>>,
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim()
}

fn parse_codes(text: &str, codes: &mut HashMap<String, String>) {
    for line in text.lines() {
        let line = strip_comment(line);
        if let Some((button, code)) = line.split_once('=') {
            let code = code.trim().to_lowercase();
            if !code.is_empty() {
                codes.insert(format!("{:0>8}", code), button.trim().to_string());
            }
        }
    }
}

fn parse_map(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut map: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut mode = String::from("common");
    for line in text.lines() {
        let line = strip_comment(line);
        if let Some(m) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            mode = m.to_string();
            continue;
        }
        let Some((button, function)) = line.split_once('=') else {
            continue;
        };
        let (button, function) = (button.trim(), function.trim().to_string());
        let entries = map.entry(mode.clone()).or_default();
        match button.strip_suffix(".*") {
            Some(base) => {
                for style in PRESS_STYLES {
                    entries.insert(format!("{base}{style}"), function.clone());
                }
            }
            None => {
                entries.insert(button.to_string(), function);
            }
        }
    }
    map
}

pub fn maps() -> &'static Maps {
    static M: OnceLock<Maps> = OnceLock::new();
    M.get_or_init(|| {
        let mut codes = HashMap::new();
        parse_codes(REMOTE, &mut codes);
        parse_codes(FRONT_PANEL, &mut codes);
        let mut map = parse_map(DEFAULT_MAP);
        // `Slim::Plugin::SongScanner` registers its own defaults
        map.insert(
            "songscanner".into(),
            [
                ("fwd.repeat", "song_scanner_fwd"),
                ("rew.repeat", "song_scanner_rew"),
                ("fwd.hold", "dead"),
                ("rew.hold", "dead"),
            ]
            .into_iter()
            .map(|(b, f)| (b.to_string(), f.to_string()))
            .collect(),
        );
        Maps { codes, map }
    })
}

impl Maps {
    /// Button name for an IR code (`lookupCodeBytes`).
    pub fn button(&self, code: u32) -> Option<&str> {
        self.codes.get(&format!("{code:08x}")).map(String::as_str)
    }

    /// `lookupFunction`: the function for a button in the first of `modes` that maps it,
    /// falling back to `common`.
    pub fn function(&self, modes: &[&str], button: &str) -> Option<&str> {
        modes
            .iter()
            .chain(std::iter::once(&"common"))
            .find_map(|m| self.map.get(*m).and_then(|e| e.get(button)))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_codes_and_functions() {
        let m = maps();
        assert_eq!(m.button(0x7689e01f), Some("arrow_up"));
        assert_eq!(m.button(0x00010012), Some("play.down"));
        assert_eq!(m.button(0x1234), None);
        assert_eq!(m.function(&["home"], "arrow_up"), Some("up"));
        assert_eq!(m.function(&["home"], "arrow_up.repeat"), Some("up_repeat"));
        assert_eq!(m.function(&["home"], "arrow_left.hold"), Some("dead"));
        assert_eq!(m.function(&["browsedb"], "arrow_left.hold"), Some("home"));
        assert_eq!(m.function(&["browsedb"], "play.single"), Some("play_0"));
        assert_eq!(m.function(&["browsedb"], "play"), Some("dead"));
        assert_eq!(m.function(&["home"], "play.single"), Some("play"));
        // `.*` expands to every press style
        assert_eq!(m.function(&["off"], "add.hold"), Some("dead"));
        assert_eq!(m.function(&["off"], "add"), Some("dead"));
        assert_eq!(
            m.function(&["off"], "play.single"),
            Some("modefunction_off->play")
        );
        assert_eq!(
            m.function(&["INPUT.Text", "browsedb"], "arrow_right"),
            Some("nextChar")
        );
        assert_eq!(m.function(&["home"], "0.hold"), Some("preset_0"));
        // song scanner: held FWD keeps scanning, a quick press jumps
        assert_eq!(m.function(&["home"], "fwd.hold"), Some("song_scanner"));
        assert_eq!(
            m.function(&["INPUT.Bar", "songscanner"], "fwd.repeat"),
            Some("passback")
        );
        assert_eq!(
            m.function(&["songscanner"], "fwd.repeat"),
            Some("song_scanner_fwd")
        );
        assert_eq!(m.function(&["songscanner"], "fwd.single"), Some("jump_fwd"));
        assert_eq!(
            m.function(&["INPUT.Time", "settings"], "arrow_left"),
            Some("left")
        );
        assert_eq!(
            m.function(&["INPUT.Time", "settings"], "5"),
            Some("numberLetter_5")
        );
    }
}
