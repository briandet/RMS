//! Graphical displays of Squeezebox2/3/Classic, Transporter and Boom
//! (`Slim::Display::Squeezebox2`, `::Transporter`, `::Boom`, `::Graphics`,
//! `Slim::Display::Lib::Fonts`).
//!
//! Renders the "Now Playing" screen (with the player's playing display mode: progress bar,
//! elapsed/remaining time, buffer fullness, clock, visualizer), the date/time screensaver
//! while off, `showBriefly` messages and the volume bar, using Lyrion's own bitmap fonts in
//! the player's chosen text size.  Long lines scroll server side like Lyrion's.  Frames are
//! sent as `grfe`, brightness as `grfb` and visualizer settings as `visu`.  Players without
//! a display (squeezelite, WiiM, SqueezePlay) are skipped.

use crate::player::Player;
use crate::server::Server;
use crate::strings::s;
use serde_json::{json, Value};
use slimproto::server::ServerFrame;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// A parsed `*.font.bmp`: one glyph per Latin-1 code point, each glyph a list of 32-bit
/// columns (MSB = top row).
pub struct Font {
    glyphs: Vec<Vec<u32>>,
}

/// Control characters of Lyrion's font strings (`Slim::Display::Graphics::%fontSymbols`).
const TIGHT: char = '\x1d';
const END_TIGHT: char = '\x1c';
const PROGRESS_END: char = '\x03';
const PROGRESS1E: char = '\x04';
const PROGRESS2E: char = '\x05';
const PROGRESS1: char = '\x07';
const PROGRESS2: char = '\x08';
const BELL: char = '\x10';

impl Font {
    /// `Slim::Display::Lib::Fonts::parseBMP` + `parseFont`.
    pub fn parse(bmp: &[u8]) -> Option<Font> {
        if bmp.len() < 62 || &bmp[0..2] != b"BM" {
            return None;
        }
        let rd32 = |o: usize| u32::from_le_bytes([bmp[o], bmp[o + 1], bmp[o + 2], bmp[o + 3]]);
        let offset = rd32(10) as usize;
        let width = rd32(18) as usize;
        let height = rd32(22) as usize;
        let bpp = u16::from_le_bytes([bmp[28], bmp[29]]);
        if bpp != 1 || height < 2 {
            return None;
        }
        let normal_palette = rd32(54) & 0xffffff == 0xffffff;
        let row_bytes = width.div_ceil(32) * 4;
        // rows[0] = top row
        let pixel = |row_from_top: usize, x: usize| -> bool {
            let file_row = height - 1 - row_from_top;
            let byte = bmp
                .get(offset + file_row * row_bytes + x / 8)
                .copied()
                .unwrap_or(0);
            let bit = byte & (0x80 >> (x % 8)) != 0;
            if normal_palette {
                bit
            } else {
                !bit
            }
        };
        let bottom = height - 1;
        let mut glyphs: Vec<Vec<u32>> = Vec::new();
        let mut x = 0;
        while x < width {
            if pixel(bottom, x) {
                x += 1;
                continue;
            }
            let mut cols = Vec::new();
            while x < width && !pixel(bottom, x) {
                let mut col = 0u32;
                for y in 0..bottom.min(32) {
                    if pixel(y, x) {
                        col |= 0x8000_0000 >> y;
                    }
                }
                cols.push(col);
                x += 1;
            }
            glyphs.push(cols);
        }
        // glyph 0 is the inter-character space; pixels in it mean "no interspace"
        if let Some(g) = glyphs.first_mut() {
            if g.iter().any(|c| *c != 0) {
                g.clear();
            }
        }
        Some(Font { glyphs })
    }

    /// Render text to columns (glyph, then interspace).  `\x1d` … `\x1c` render without
    /// interspace, as Lyrion's `tight` / `/tight` symbols do.
    pub fn render(&self, text: &str) -> Vec<u32> {
        let space = self.glyphs.first().cloned().unwrap_or_default();
        let mut out = Vec::new();
        let mut tight = false;
        for ch in text.chars() {
            match ch {
                TIGHT => {
                    tight = true;
                    continue;
                }
                END_TIGHT => {
                    tight = false;
                    continue;
                }
                _ => {}
            }
            let cp = ch as u32;
            let idx = if (1..=255).contains(&cp) && (cp as usize) < self.glyphs.len() {
                cp as usize
            } else {
                '?' as usize
            };
            if let Some(g) = self.glyphs.get(idx) {
                out.extend_from_slice(g);
                if !tight {
                    out.extend_from_slice(&space);
                }
            }
        }
        out
    }
}

/// The font files shipped with Lyrion's `Graphics` folder that the players here use.
const FONT_FILES: &[(&str, &[u8])] = &[
    ("light.1", include_bytes!("../assets/fonts/light.1.font.bmp")),
    ("light.2", include_bytes!("../assets/fonts/light.2.font.bmp")),
    ("standard.1", include_bytes!("../assets/fonts/standard.1.font.bmp")),
    ("standard.2", include_bytes!("../assets/fonts/standard.2.font.bmp")),
    ("full.2", include_bytes!("../assets/fonts/full.2.font.bmp")),
    ("light_n.1", include_bytes!("../assets/fonts/light_n.1.font.bmp")),
    ("light_n.2", include_bytes!("../assets/fonts/light_n.2.font.bmp")),
    ("standard_n.1", include_bytes!("../assets/fonts/standard_n.1.font.bmp")),
    ("standard_n.2", include_bytes!("../assets/fonts/standard_n.2.font.bmp")),
    ("full_n.2", include_bytes!("../assets/fonts/full_n.2.font.bmp")),
];

fn font(name: &str) -> Option<&'static Font> {
    static F: OnceLock<HashMap<&'static str, Font>> = OnceLock::new();
    F.get_or_init(|| {
        FONT_FILES
            .iter()
            .filter_map(|(n, b)| Font::parse(b).map(|f| (*n, f)))
            .collect()
    })
    .get(name)
}

/// The fonts of one family for lines 1 and 2 (`gfonthash`).  Single line families
/// (`full`) only have a line 2 font.
#[derive(Clone, Copy)]
pub struct FontSet {
    line: [Option<&'static Font>; 2],
}

impl FontSet {
    pub fn named(family: &str) -> Option<FontSet> {
        let set = FontSet {
            line: [
                font(&format!("{family}.1")),
                font(&format!("{family}.2")),
            ],
        };
        set.line[1].map(|_| set)
    }

    pub fn single_line(&self) -> bool {
        self.line[0].is_none()
    }
}

// ---- display models ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Squeezebox2 / Squeezebox3 (Classic): 320x32.
    Squeezebox2,
    /// Transporter: two 320x32 screens.
    Transporter,
    /// Boom: 160x32.
    Boom,
}

impl Kind {
    pub fn for_model(model: &str) -> Option<Kind> {
        match model {
            "squeezebox2" | "squeezebox3" => Some(Kind::Squeezebox2),
            "transporter" => Some(Kind::Transporter),
            "boom" => Some(Kind::Boom),
            _ => None,
        }
    }

    pub fn width(self) -> usize {
        match self {
            Kind::Boom => 160,
            _ => 320,
        }
    }

    fn modes(self) -> &'static [Mode] {
        match self {
            Kind::Squeezebox2 => SB2_MODES,
            Kind::Transporter => TRANSPORTER_MODES,
            Kind::Boom => BOOM_MODES,
        }
    }

    /// `brightnessMap`: brightness pref level → `grfb` value.
    fn brightness_map(self, p: &Player) -> Vec<u16> {
        match self {
            Kind::Boom => {
                let sens = p.prefs.get_i64("sensAutoBrightness").clamp(1, 20);
                let offset = p.prefs.get_i64("minAutoBrightness").clamp(1, 7);
                vec![0, 1, 2, 3, 4, 5, ((21 - sens) * 256 + offset) as u16]
            }
            _ => vec![65535, 0, 1, 3, 4],
        }
    }

    fn default_fonts(self) -> &'static [&'static str] {
        match self {
            Kind::Boom => &["light_n", "standard_n", "full_n"],
            _ => &["light", "standard", "full"],
        }
    }
}

/// Display width in pixels for a player model, or None without a graphic display.
pub fn display_width(model: &str) -> Option<usize> {
    Kind::for_model(model).map(Kind::width)
}

fn kind_of(p: &Player) -> Option<Kind> {
    Kind::for_model(&p.state.lock().caps.model)
}

/// A playing display mode (`@modes` of the display classes).
pub struct Mode {
    desc: &'static [&'static str],
    bar: bool,
    /// 1 = elapsed, -1 = remaining, 0 = no time.
    secs: i8,
    /// Text width while the visualizer is shown.
    width: usize,
    fullness: bool,
    clock: bool,
    /// `visu` parameters: visualizer type followed by its settings.
    visu: &'static [u32],
}

const VU: u32 = 1;
const SPECTRUM: u32 = 2;

macro_rules! mode {
    ($desc:expr, $bar:expr, $secs:expr, $width:expr, $visu:expr) => {
        Mode { desc: $desc, bar: $bar, secs: $secs, width: $width, fullness: false, clock: false, visu: $visu }
    };
}

const SB2_VU: &[u32] = &[VU, 0, 0, 280, 18, 302, 18];
const SB2_SPECTRUM_SMALL: &[u32] = &[SPECTRUM, 1, 1, 0x10000, 280, 40, 0, 4, 1, 0, 1, 3];
const SB2_SPECTRUM: &[u32] =
    &[SPECTRUM, 0, 0, 0x10000, 0, 160, 0, 4, 1, 1, 1, 1, 160, 160, 1, 4, 1, 1, 1, 1];

static SB2_MODES: &[Mode] = &[
    mode!(&["BLANK"], false, 0, 320, &[0]),
    mode!(&["ELAPSED"], true, 1, 320, &[0]),
    mode!(&["REMAINING"], true, -1, 320, &[0]),
    mode!(&["VISUALIZER_VUMETER_SMALL"], false, 0, 278, SB2_VU),
    mode!(&["VISUALIZER_VUMETER_SMALL", "AND", "ELAPSED"], true, 1, 278, SB2_VU),
    mode!(&["VISUALIZER_VUMETER_SMALL", "AND", "REMAINING"], true, -1, 278, SB2_VU),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER_SMALL"], false, 0, 278, SB2_SPECTRUM_SMALL),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER_SMALL", "AND", "ELAPSED"], true, 1, 278, SB2_SPECTRUM_SMALL),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER_SMALL", "AND", "REMAINING"], true, -1, 278, SB2_SPECTRUM_SMALL),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER"], false, 0, 320, SB2_SPECTRUM),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER", "AND", "ELAPSED"], true, 1, 320, SB2_SPECTRUM),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER", "AND", "REMAINING"], true, -1, 320, SB2_SPECTRUM),
    Mode { desc: &["SETUP_SHOWBUFFERFULLNESS"], bar: true, secs: 0, width: 320, fullness: true, clock: false, visu: &[0] },
    Mode { desc: &["CLOCK"], bar: false, secs: 0, width: 320, fullness: false, clock: true, visu: &[0] },
];

static TRANSPORTER_MODES: &[Mode] = &[
    mode!(&["BLANK"], false, 0, 320, &[0]),
    mode!(&["PROGRESS_BAR"], true, 0, 320, &[0]),
    mode!(&["PROGRESS_BAR", "AND", "ELAPSED"], true, 1, 320, &[0]),
    mode!(&["PROGRESS_BAR", "AND", "REMAINING"], true, -1, 320, &[0]),
    mode!(&["ELAPSED"], false, 1, 320, &[0]),
    mode!(&["REMAINING"], false, -1, 320, &[0]),
    Mode { desc: &["SETUP_SHOWBUFFERFULLNESS"], bar: true, secs: 0, width: 320, fullness: true, clock: false, visu: &[0] },
    Mode { desc: &["CLOCK"], bar: false, secs: 0, width: 320, fullness: false, clock: true, visu: &[0] },
];

/// Transporter's second screen (`@visualizers`): `true` = extended text (album / artist).
static TRANSPORTER_VISUALIZERS: &[(bool, &[u32])] = &[
    (false, &[0]),
    (true, &[0]),
    (false, &[VU, 0, 1, 320, 160, 160 + 320, 160]),
    (false, &[VU, 0, 0, 20 + 320, 130, 170 + 320, 130]),
    (false, &[SPECTRUM, 0, 0, 0x10000, 320, 160, 0, 4, 1, 1, 1, 3, 160 + 320, 160, 1, 4, 1, 1, 1, 3]),
    (true, &[SPECTRUM, 0, 0, 0x10000, 320, 160, 0, 4, 1, 1, 1, 1, 160 + 320, 160, 1, 4, 1, 1, 1, 1]),
];

const BOOM_SPECTRUM: &[u32] =
    &[SPECTRUM, 0, 0, 0x10000, 0, 80, 0, 3, 1, 1, 1, 1, 81, 80, 1, 3, 1, 1, 1, 1];

static BOOM_MODES: &[Mode] = &[
    mode!(&["BLANK"], false, 0, 160, &[0]),
    mode!(&["PROGRESS_BAR"], true, 0, 160, &[0]),
    mode!(&["ELAPSED"], false, 1, 160, &[0]),
    mode!(&["REMAINING"], false, -1, 160, &[0]),
    mode!(&["VISUALIZER_VUMETER_SMALL"], false, 0, 145, &[VU, 0, 0, 146, 6, 154, 6]),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER_SMALL"], false, 0, 145, &[SPECTRUM, 1, 1, 0x10000, 146, 16, 0, 2, 0, 0, 1, 3]),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER"], false, 0, 160, BOOM_SPECTRUM),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER", "AND", "PROGRESS_BAR"], true, 0, 160, BOOM_SPECTRUM),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER", "AND", "ELAPSED"], false, 1, 160, BOOM_SPECTRUM),
    mode!(&["VISUALIZER_SPECTRUM_ANALYZER", "AND", "REMAINING"], false, -1, 160, BOOM_SPECTRUM),
    Mode { desc: &["CLOCK"], bar: false, secs: 0, width: 160, fullness: false, clock: true, visu: &[0] },
    Mode { desc: &["SETUP_SHOWBUFFERFULLNESS"], bar: false, secs: 0, width: 160, fullness: true, clock: false, visu: &[0] },
];

/// Human readable name of a playing display mode (`desc` strings joined).
pub fn mode_description(kind: Kind, mode: usize) -> String {
    kind.modes()
        .get(mode)
        .map(|m| m.desc.iter().map(|d| s(d)).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

// ---- prefs -----------------------------------------------------------------------------------

/// Initialise the display prefs of a player with a graphic display, as the display classes'
/// `initPrefs` do (only prefs not stored yet are set).
pub fn init_prefs(p: &Player) {
    let Some(kind) = kind_of(p) else {
        return;
    };
    let fonts = json!(kind.default_fonts());
    let mut prefs: Vec<(&str, Value)> = vec![
        ("activeFont", fonts.clone()),
        ("activeFont_curr", json!(1)),
        ("idleFont", fonts),
        ("scrollPixels", json!(2)),
        ("scrollPixelsDouble", json!(3)),
        ("scrollPause", json!(3.6)),
        ("scrollPauseDouble", json!(3.6)),
        ("scrollRate", json!(0.033)),
        ("scrollRateDouble", json!(0.033)),
    ];
    match kind {
        Kind::Squeezebox2 => prefs.extend([
            ("idleFont_curr", json!(1)),
            ("playingDisplayMode", json!(5)),
            ("playingDisplayModes", json!((0..=11).collect::<Vec<_>>())),
            ("idleBrightness", json!(2)),
            ("powerOnBrightness", json!(4)),
            ("powerOffBrightness", json!(1)),
        ]),
        Kind::Transporter => prefs.extend([
            ("idleFont_curr", json!(1)),
            ("playingDisplayMode", json!(5)),
            ("playingDisplayModes", json!((0..=5).collect::<Vec<_>>())),
            ("visualMode", json!(2)),
            ("visualModes", json!((0..=5).collect::<Vec<_>>())),
            ("idleBrightness", json!(2)),
            ("powerOnBrightness", json!(4)),
            ("powerOffBrightness", json!(1)),
        ]),
        Kind::Boom => prefs.extend([
            ("idleFont_curr", json!(2)),
            ("playingDisplayMode", json!(1)),
            ("playingDisplayModes", json!((0..=10).collect::<Vec<_>>())),
            ("idleBrightness", json!(6)),
            ("powerOnBrightness", json!(6)),
            ("powerOffBrightness", json!(6)),
            ("scrollPause", json!(1.5)),
            ("scrollPauseDouble", json!(1.5)),
            ("minAutoBrightness", json!(2)),
            ("sensAutoBrightness", json!(10)),
        ]),
    }
    for (k, v) in prefs {
        p.prefs.init(k, v);
    }
    // `validateFonts`: reset font lists naming fonts we don't have
    for pref in ["activeFont", "idleFont"] {
        let list = p.prefs.get_string_list(pref);
        if list.is_empty() || list.iter().any(|f| FontSet::named(f).is_none()) {
            p.prefs.set(pref, json!(kind.default_fonts()));
        }
    }
}

/// Font list pref (`activeFont` while on, `idleFont` while off).
fn font_pref(p: &Player) -> &'static str {
    if p.power() {
        "activeFont"
    } else {
        "idleFont"
    }
}

/// The font family for the current text size (`Slim::Display::Graphics::fonts`).
fn current_fonts(p: &Player, kind: Kind) -> FontSet {
    let pref = font_pref(p);
    let list = p.prefs.get_string_list(pref);
    let size = p.prefs.get_i64(&format!("{pref}_curr")).max(0) as usize;
    list.get(size)
        .and_then(|f| FontSet::named(f))
        .or_else(|| FontSet::named(kind.default_fonts()[1]))
        .expect("embedded fonts")
}

/// `getBrightnessOptions`: pref value → label, brightest first.
pub fn brightness_options(p: &Player) -> Vec<(i64, String)> {
    let Some(kind) = kind_of(p) else {
        return Vec::new();
    };
    let map = kind.brightness_map(p);
    let mut max = map.len() as i64 - 1;
    let mut opts: HashMap<i64, String> = HashMap::from([
        (0, format!("0 ({})", s("BRIGHTNESS_DARK"))),
        (1, format!("1 ({})", s("BRIGHTNESS_DIMMEST"))),
        (2, "2".into()),
        (3, "3".into()),
        (4, "4".into()),
    ]);
    if map[max as usize] > 255 {
        for b in 4..=max {
            if map[b as usize] > 255 {
                opts.insert(b, s("BRIGHTNESS_AMBIENT"));
                max -= 1;
            }
        }
    }
    opts.insert(max, format!("{} ({})", max, s("BRIGHTNESS_BRIGHTEST")));
    let mut v: Vec<(i64, String)> = opts.into_iter().collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

/// Which brightness pref applies now (`Slim::Buttons::ScreenSaver::screenSaver`): idle after
/// `screensavertimeout` seconds without IR input while on.
fn brightness_pref(p: &Player) -> &'static str {
    if !p.power() {
        return "powerOffBrightness";
    }
    let timeout = p.prefs.get_f64("screensavertimeout");
    let last_ir = p.state.lock().last_ir;
    if timeout > 0.0 && last_ir > 0.0 && last_ir < crate::util::now() - timeout {
        "idleBrightness"
    } else {
        "powerOnBrightness"
    }
}

// ---- remote control functions -----------------------------------------------------------------

/// `brightness_toggle` (`Slim::Buttons::Common` `brightness`).
pub fn toggle_brightness(p: &Player) {
    let Some(kind) = kind_of(p) else {
        return;
    };
    let pref = brightness_pref(p);
    let max = kind.brightness_map(p).len() as i64 - 1;
    let mut b = p.prefs.get_i64(pref) - 1;
    if b < 0 {
        b = max;
    }
    p.prefs.set(pref, json!(b));
}

/// `textsize_toggle` (`Slim::Buttons::Common` `textsize`).
pub fn toggle_text_size(p: &Player) {
    if kind_of(p).is_none() {
        return;
    }
    let pref = font_pref(p);
    let max = p.prefs.get_string_list(pref).len().saturating_sub(1) as i64;
    let mut size = p.prefs.get_i64(&format!("{pref}_curr")) + 1;
    if size > max {
        size = 0;
    }
    p.prefs.set(&format!("{pref}_curr"), json!(size));
}

/// `playdisp_toggle` on the Now Playing screen (`Slim::Buttons::Playlist::playdisp`): next
/// playing display mode.
pub fn cycle_playing_display_mode(p: &Player) {
    if kind_of(p).is_none() {
        return;
    }
    let modes = p.prefs.get_string_list("playingDisplayModes");
    if modes.is_empty() {
        return;
    }
    let pdm = (p.prefs.get_i64("playingDisplayMode").max(0) as usize + 1) % modes.len();
    p.prefs.set("playingDisplayMode", json!(pdm));
    // unlike Lyrion, name the new mode briefly
    if let (Some(kind), Some(m)) = (kind_of(p), modes.get(pdm).and_then(|m| m.parse().ok())) {
        p.state.lock().show_briefly = Some((
            vec![s("SETUP_PLAYINGDISPLAYMODE"), mode_description(kind, m)],
            crate::util::now() + 1.5,
        ));
    }
}

/// `mixerDisplay('volume')`: show the volume bar for a second.
pub fn show_volume(p: &Player) {
    if kind_of(p).is_some() {
        p.state.lock().mixer_display_until = crate::util::now() + 1.0;
    }
}

// ---- rendering ---------------------------------------------------------------------------------

/// What to draw on one screen (`$parts` of Lyrion's display code).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parts {
    pub line: [String; 2],
    pub overlay: [String; 2],
    pub center: [String; 2],
    /// Progress bar (fraction complete) drawn between line 1 and its overlay.
    pub progress: Option<f64>,
    /// Full width slider on line 2 (`simpleSliderBar`), 0..=100.
    pub slider: Option<f64>,
}

/// A rendered screen: the static columns, plus line 2 if it must scroll.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rendered {
    pub bits: Vec<u32>,
    /// Scrolling text columns and the width of the area they scroll in.
    pub scroll: Option<(Vec<u32>, usize)>,
}

/// Columns of blank space between the end and the restart of scrolling text.
const SCROLL_PAD: usize = 40;

/// `Slim::Display::Graphics::sliderBar` (without cursor): progress symbols for a bar of
/// `width` pixels at `value` percent, with an optional centre divider at `midpoint` percent.
pub fn slider_bar(width: usize, value: f64, midpoint: f64) -> String {
    if width == 0 {
        return String::new();
    }
    let value = value.clamp(0.0, 100.0);
    let spaces = width as i64 - if midpoint > 0.0 { 2 } else { 1 };
    let dots = (value / 100.0 * spaces as f64) as i64;
    let divider = (midpoint / 100.0 * spaces as f64) as i64;
    // Lyrion's progress3 symbols map to progress2
    let (p1, p2, p3, p1e, p2e, p3e) =
        (PROGRESS1, PROGRESS2, PROGRESS2, PROGRESS1E, PROGRESS2E, PROGRESS2E);
    let mut chart = String::new();
    chart.push(TIGHT);
    chart.push(PROGRESS_END);
    if midpoint > 0.0 {
        for i in 0..divider {
            let edge1 = i == divider - 1 || i == 0;
            let edge2 = i == divider - 2 || i == 1;
            chart.push(if value >= midpoint {
                if edge1 {
                    p1e
                } else if edge2 {
                    p2e
                } else {
                    p3e
                }
            } else if edge1 {
                if i < dots { p1e } else { p1 }
            } else if edge2 {
                if i < dots { p2e } else { p2 }
            } else if i < dots {
                p3e
            } else {
                p3
            });
        }
        chart.push(PROGRESS_END);
    }
    for i in divider + 1..spaces {
        let edge1 = i == divider + 1 || i == spaces - 1;
        let edge2 = i == divider + 2 || i == spaces - 2;
        chart.push(if value <= midpoint {
            if edge1 {
                p1e
            } else if edge2 {
                p2e
            } else {
                p3e
            }
        } else {
            let empty = i > dots;
            match (edge1, edge2, empty) {
                (true, _, true) => p1e,
                (true, _, false) => p1,
                (_, true, true) => p2e,
                (_, true, false) => p2,
                (_, _, true) => p3e,
                _ => p3,
            }
        });
    }
    chart.push(PROGRESS_END);
    chart.push(END_TIGHT);
    chart
}

fn or_at(bits: &mut [u32], start: usize, cols: &[u32]) {
    for (i, c) in cols.iter().enumerate() {
        if let Some(b) = bits.get_mut(start + i) {
            *b |= c;
        }
    }
}

/// Render a screen `width` pixels wide (`Slim::Display::Graphics::render`).  Line 2 scrolls
/// when it doesn't fit beside its overlay; line 1 is truncated.
pub fn render(width: usize, fonts: FontSet, parts: &Parts, allow_scroll: bool) -> Rendered {
    let mut bits = vec![0u32; width];
    let mut scroll = None;
    for l in 0..2 {
        let Some(f) = fonts.line[l] else {
            continue;
        };
        let text = f.render(&parts.line[l]);
        let mut overlay = f.render(&parts.overlay[l]);
        if l == 0 {
            if let Some(frac) = parts.progress {
                let barlen = width.saturating_sub(text.len() + overlay.len());
                if barlen > 2 {
                    let mut bar = f.render(&slider_bar(barlen, frac * 100.0, 0.0));
                    bar.extend_from_slice(&overlay);
                    overlay = bar;
                }
            }
        }
        let ov_start = width.saturating_sub(overlay.len());
        or_at(&mut bits, ov_start, &overlay);
        if l == 1 {
            if let Some(v) = parts.slider {
                or_at(&mut bits, 0, &f.render(&slider_bar(width, v, 0.0)));
                continue;
            }
        }
        if l == 1 && allow_scroll && text.len() > ov_start {
            scroll = Some((text, ov_start));
        } else {
            or_at(&mut bits[..ov_start], 0, &text);
        }
        let center = f.render(&parts.center[l]);
        if !center.is_empty() {
            or_at(&mut bits, width.saturating_sub(center.len()) / 2, &center);
        }
    }
    Rendered { bits, scroll }
}

/// The frame for a rendered screen with its scrolling line advanced by `offset` pixels.
pub fn frame(r: &Rendered, offset: usize) -> Vec<u8> {
    let mut bits = r.bits.clone();
    if let Some((cols, area)) = &r.scroll {
        let period = cols.len() + SCROLL_PAD;
        for (i, b) in bits.iter_mut().take(*area).enumerate() {
            if let Some(c) = cols.get((offset + i) % period) {
                *b |= c;
            }
        }
    }
    bits.iter().flat_map(|c| c.to_be_bytes()).collect()
}

/// Compose a frame: line 1 (with right-aligned overlay) and line 2 in the standard font.
#[cfg(test)]
pub fn compose(width: usize, line1: &str, overlay1: &str, line2: &str) -> Vec<u8> {
    let parts = Parts {
        line: [line1.into(), line2.into()],
        overlay: [overlay1.into(), String::new()],
        ..Default::default()
    };
    match FontSet::named("standard") {
        Some(f) => frame(&render(width, f, &parts, false), 0),
        None => vec![0; width * 4],
    }
}

// ---- screens -----------------------------------------------------------------------------------

/// Lyrion date/time formats: strftime with `|` marking leading zeros to remove
/// (`Slim::Utils::DateTime::longDateF` / `timeF`).
fn lms_strftime(fmt: &str, t: chrono::DateTime<chrono::Local>, all_zeros: bool) -> String {
    use chrono::format::{Item, StrftimeItems};
    let fmt = fmt.replace('|', "\u{1}");
    if StrftimeItems::new(&fmt).any(|i| matches!(i, Item::Error)) {
        return t.format("%H:%M").to_string();
    }
    let out = t.format(&fmt).to_string();
    let mut res = String::with_capacity(out.len());
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1}' {
            res.push(c);
            continue;
        }
        if all_zeros {
            while chars.peek() == Some(&'0') {
                chars.next();
            }
        } else if chars.peek() == Some(&'0') {
            let mut rest = chars.clone();
            rest.next();
            if rest.peek().is_some_and(|c| c.is_ascii_digit()) {
                chars.next();
            }
        }
    }
    res
}

/// `textSongTime`: `mm:ss` / `h:mm:ss`, remaining time prefixed with `-`.
fn song_time(elapsed: f64, duration: Option<f64>, remaining: bool) -> String {
    let mut delta = elapsed.max(0.0);
    if let Some(d) = duration.filter(|d| *d > 0.0) {
        delta = delta.min(d);
    }
    let mut sign = "";
    if remaining {
        if let Some(d) = duration.filter(|d| *d > 0.0) {
            delta = d - delta;
            sign = "-";
        }
    }
    let delta = delta as u64;
    let (h, m, sec) = (delta / 3600, delta / 60 % 60, delta % 60);
    if h > 0 {
        format!("{sign}{h}:{m:02}:{sec:02}")
    } else {
        format!("{sign}{m:02}:{sec:02}")
    }
}

/// Format a track title with a Lyrion title format such as `TRACKNUM. TITLE (ARTIST)`,
/// dropping the separators of missing elements like `Slim::Music::TitleFormatter`: text
/// before an element needs that element and (unless it leads) an earlier present one;
/// trailing text needs the last element.
pub fn format_title(format: &str, value: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut pending: Option<String> = None;
    let mut seen_elem = false;
    let mut any_present = false;
    let mut last_present = false;
    for (is_elem, text) in title_tokens(format) {
        if !is_elem {
            pending = Some(text);
            continue;
        }
        let sep = pending.take();
        match value(&text).filter(|v| !v.is_empty()) {
            Some(v) => {
                if let Some(sep) = sep.filter(|_| !seen_elem || any_present) {
                    out.push_str(&sep);
                }
                out.push_str(&v);
                any_present = true;
                last_present = true;
            }
            None => last_present = false,
        }
        seen_elem = true;
    }
    if let Some(sep) = pending.filter(|_| last_present) {
        out.push_str(&sep);
    }
    out.trim().to_string()
}

/// Split a title format into (is_element, text) tokens.
fn title_tokens(format: &str) -> Vec<(bool, String)> {
    const ELEMENTS: &[&str] = &[
        "TRACKNUM", "TITLE", "ARTIST", "ALBUMARTIST", "ALBUM", "DISC", "YEAR", "GENRE", "FILE",
        "EXT",
    ];
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut i = 0;
    let b = format.as_bytes();
    while i < b.len() {
        let elem = ELEMENTS.iter().filter(|e| format[i..].starts_with(**e)).max_by_key(|e| e.len());
        match elem {
            Some(e) => {
                out.push((true, e.to_string()));
                i += e.len();
            }
            None => {
                let ch = format[i..].chars().next().unwrap();
                match out.last_mut() {
                    Some((false, t)) => t.push(ch),
                    _ => out.push((false, ch.to_string())),
                }
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Title as shown on the player (`getCurrentTitle` with the player's title format).
fn display_title(p: &Player, server: Option<&Arc<Server>>, info: &crate::player::song::TrackInfo, meta: Option<&crate::player::song::RemoteMeta>) -> String {
    if info.remote {
        return meta
            .and_then(|m| m.title.clone().or_else(|| m.station.clone()))
            .or_else(|| info.title.clone())
            .unwrap_or_else(|| crate::player::controller::title_from_url(&info.url));
    }
    let formats = server
        .map(|s| s.prefs.server.get_string_list("titleFormat"))
        .unwrap_or_default();
    let player_formats = p.prefs.get_string_list("titleFormat");
    let curr = p.prefs.get_i64("titleFormatCurr").max(0) as usize;
    let fmt = player_formats
        .get(curr)
        .and_then(|i| i.parse::<usize>().ok())
        .and_then(|i| formats.get(i).cloned())
        .unwrap_or_else(|| "TITLE".into());
    let file = crate::util::path_from_file_url(&info.url);
    let title = format_title(&fmt, &|e| match e {
        "TITLE" => info
            .title
            .clone()
            .or_else(|| Some(crate::player::controller::title_from_url(&info.url))),
        "ARTIST" | "ALBUMARTIST" => info.artist.clone(),
        "ALBUM" => info.album.clone(),
        "TRACKNUM" => info.tracknum.filter(|n| *n > 0).map(|n| n.to_string()),
        "DISC" => info.disc.filter(|d| *d > 1).map(|d| d.to_string()),
        "YEAR" => info.year.filter(|y| *y > 0).map(|y| y.to_string()),
        "GENRE" => info.genre.clone(),
        "FILE" => file
            .as_ref()
            .and_then(|f| f.file_stem())
            .map(|f| f.to_string_lossy().into_owned()),
        "EXT" => file
            .as_ref()
            .and_then(|f| f.extension())
            .map(|f| f.to_string_lossy().into_owned()),
        _ => None,
    });
    if title.is_empty() {
        crate::player::controller::title_from_url(&info.url)
    } else {
        title
    }
}

/// Everything to show on a player's display right now.
#[derive(Debug, Clone, PartialEq)]
struct Screen {
    parts: Parts,
    /// Transporter's second screen.
    parts2: Option<Parts>,
    /// Text width of screen 1 (narrower beside a small visualizer).
    width: usize,
    visu: Vec<u32>,
    brightness: u16,
}

fn screen_for(server: &Arc<Server>, p: &Arc<Player>, kind: Kind) -> Screen {
    let now = crate::util::now();
    let brightness = {
        let map = kind.brightness_map(p);
        let level = p.prefs.get_i64(brightness_pref(p)).clamp(0, map.len() as i64 - 1);
        map[level as usize]
    };
    let full = kind.width();
    let mut screen = Screen {
        parts: Parts::default(),
        parts2: (kind == Kind::Transporter).then(Parts::default),
        width: full,
        visu: vec![0],
        brightness,
    };

    if !p.power() {
        // date/time screensaver (`Slim::Plugin::DateTime`)
        let t = chrono::Local::now();
        let date = lms_strftime(&server.prefs.server.get_str("longdateFormat"), t, true);
        let time = lms_strftime(&server.prefs.server.get_str("timeFormat"), t, false);
        screen.parts.center = [date, time];
        let (state, next) = crate::plugins::alarm::status_info(p);
        if state == "set" {
            let mut ov = BELL.to_string();
            if kind != Kind::Boom {
                if let Some(at) = chrono::DateTime::from_timestamp(next, 0) {
                    let at = at.with_timezone(&chrono::Local);
                    ov.push(' ');
                    ov.push_str(&lms_strftime(&server.prefs.server.get_str("timeFormat"), at, false));
                }
            }
            screen.parts.overlay[0] = ov;
        }
        return screen;
    }

    let (mixer_until, briefly) = {
        let st = p.state.lock();
        (st.mixer_display_until, st.show_briefly.clone())
    };
    if mixer_until > now {
        // `mixerDisplay`: header + full width bar, visualizer hidden
        let vol = if p.is_muted() { 0 } else { p.volume() };
        let value = if vol <= 0 {
            format!(" ({})", s("MUTED"))
        } else if kind == Kind::Transporter {
            format!(" ({:.2} dB)", -((vol as f64 / 2.0) - 50.0).abs())
        } else {
            format!(" ({})", vol)
        };
        screen.parts.line[0] = format!("{}{}", s("VOLUME"), value);
        screen.parts.slider = Some(vol.max(0) as f64);
        return screen;
    }
    if let Some((lines, until)) = briefly {
        if until > now {
            screen.parts.line = [
                lines.first().cloned().unwrap_or_default(),
                lines.get(1).cloned().unwrap_or_default(),
            ];
            return screen;
        }
    }

    // Now Playing (`currentSongLines` + `nowPlayingModeLines`)
    let pdm = {
        let list = p.prefs.get_string_list("playingDisplayModes");
        let idx = p.prefs.get_i64("playingDisplayMode").max(0) as usize;
        list.get(idx)
            .and_then(|m| m.parse::<usize>().ok())
            .unwrap_or(1)
            .min(kind.modes().len() - 1)
    };
    let mode = &kind.modes()[pdm];
    screen.visu = mode.visu.to_vec();
    screen.width = mode.width;
    // Transporter: screen 2 shows a visualizer and/or album and artist (`visualMode`)
    let mut extended_text = false;
    if kind == Kind::Transporter {
        let list = p.prefs.get_string_list("visualModes");
        let idx = p.prefs.get_i64("visualMode").max(0) as usize;
        let vm = list
            .get(idx)
            .and_then(|m| m.parse::<usize>().ok())
            .unwrap_or(0)
            .min(TRANSPORTER_VISUALIZERS.len() - 1);
        let (text, visu) = TRANSPORTER_VISUALIZERS[vm];
        screen.visu = visu.to_vec();
        extended_text = text;
    }

    let c = p.controller();
    let c = c.lock();
    let count = c.playlist.len();
    if count == 0 {
        screen.parts.line = [s("NOW_PLAYING"), s("NOTHING")];
        return screen;
    }
    let idx = c.playing_song_index();
    let mode_str = c.mode();
    let status = match mode_str {
        "pause" => s("PAUSED"),
        "stop" => s("STOPPED"),
        _ => {
            let mut st = s("PLAYING");
            if p.is_muted() {
                st = format!("{} {}", st, s("LCMUTED"));
            }
            st
        }
    };
    screen.parts.line[0] = if count > 1 {
        format!("{} ({} {} {}) ", status, idx + 1, s("OUT_OF"), count)
    } else {
        status
    };

    let (info, meta) = match c.playing_song() {
        Some(song) => (Some(song.track.clone()), Some(song.remote_meta.clone())),
        None => (c.playlist.song(idx).map(|e| server.track_info_for(e)), None),
    };
    if let Some(info) = &info {
        screen.parts.line[1] = display_title(p, Some(server), info, meta.as_ref());
        if let Some(p2) = screen.parts2.as_mut() {
            if extended_text {
                let album = meta.as_ref().and_then(|m| m.album.clone()).or_else(|| info.album.clone());
                let artist = meta.as_ref().and_then(|m| m.artist.clone()).or_else(|| info.artist.clone());
                p2.line = [album.unwrap_or_default(), artist.unwrap_or_default()];
            }
        }
    }

    let duration = c.playing_song_duration().filter(|d| *d > 0.0);
    let elapsed = if c.is_stopped() { 0.0 } else { c.playing_song_elapsed() };
    drop(c);
    let mut overlay = String::new();
    let mut fraction = 0.0;
    if mode.fullness {
        let st = p.state.lock();
        if st.buffer_size > 0 {
            fraction = st.buffer_fullness as f64 / st.buffer_size as f64;
        }
        overlay = format!(" {}%", (fraction * 100.0 + 0.5) as i64);
    } else if mode.secs != 0 {
        overlay = format!(" {}", song_time(elapsed, duration, mode.secs < 0));
    } else if mode.clock {
        overlay = format!(
            " {}",
            lms_strftime(&server.prefs.server.get_str("timeFormat"), chrono::Local::now(), false)
        );
    } else if let Some(d) = duration {
        fraction = (elapsed / d).clamp(0.0, 1.0);
    }
    if mode.bar && (duration.is_some() || mode.fullness) {
        if !mode.fullness {
            fraction = duration.map(|d| (elapsed / d).clamp(0.0, 1.0)).unwrap_or(0.0);
        }
        screen.parts.progress = Some(fraction);
    }
    screen.parts.overlay[0] = overlay;
    screen
}

// ---- background task ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScrollPhase {
    Pause(f64),
    Running,
    Done,
}

/// Per-player display state of the refresh task.
struct DisplayState {
    screen: Option<Screen>,
    fonts_key: String,
    rendered: Rendered,
    rendered2: Option<Rendered>,
    offset: usize,
    phase: ScrollPhase,
    next_compute: f64,
    sent: Option<Vec<u8>>,
    sent2: Option<Vec<u8>>,
    sent_brightness: Option<u16>,
    sent_visu: Option<Vec<u32>>,
    connected_at: f64,
}

impl DisplayState {
    fn new(connected_at: f64) -> DisplayState {
        DisplayState {
            screen: None,
            fonts_key: String::new(),
            rendered: Rendered::default(),
            rendered2: None,
            offset: 0,
            phase: ScrollPhase::Done,
            next_compute: 0.0,
            sent: None,
            sent2: None,
            sent_brightness: None,
            sent_visu: None,
            connected_at,
        }
    }
}

/// How often the screen content is recomputed; scrolling runs at `TICK`.
const COMPUTE_INTERVAL: f64 = 0.15;
const TICK: Duration = Duration::from_millis(33);

fn visu_frame(params: &[u32]) -> ServerFrame {
    let mut b = Vec::with_capacity(2 + params.len() * 4);
    b.push(params.first().copied().unwrap_or(0) as u8);
    b.push(params.len().saturating_sub(1) as u8);
    for v in params.iter().skip(1) {
        b.extend_from_slice(&v.to_be_bytes());
    }
    ServerFrame::Visu(b.into())
}

fn update_player(server: &Arc<Server>, p: &Arc<Player>, kind: Kind, ds: &mut DisplayState) {
    let now = crate::util::now();
    if now >= ds.next_compute {
        ds.next_compute = now + COMPUTE_INTERVAL;
        let screen = screen_for(server, p, kind);
        let fonts_key = {
            let pref = font_pref(p);
            format!("{}/{}", p.prefs.get_str(pref), p.prefs.get_i64(&format!("{pref}_curr")))
        };
        if ds.screen.as_ref() != Some(&screen) || ds.fonts_key != fonts_key {
            let fonts = current_fonts(p, kind);
            let scroll_mode = p.prefs.get_i64("scrollMode");
            let mut r = render(screen.width, fonts, &screen.parts, scroll_mode != 2);
            r.bits.resize(kind.width(), 0);
            if r.scroll.as_ref().map(|s| &s.0) != ds.rendered.scroll.as_ref().map(|s| &s.0)
                || ds.fonts_key != fonts_key
            {
                // new scrolling text: start again after the scroll pause
                ds.offset = 0;
                let pause = p.prefs.get_f64(if fonts.single_line() {
                    "scrollPauseDouble"
                } else {
                    "scrollPause"
                });
                ds.phase = ScrollPhase::Pause(now + pause);
            }
            ds.rendered = r;
            ds.rendered2 = screen
                .parts2
                .as_ref()
                .map(|p2| render(kind.width(), fonts, p2, false));
            ds.screen = Some(screen);
            ds.fonts_key = fonts_key;
        }
    }
    let Some(screen) = ds.screen.as_ref() else {
        return;
    };

    // scrolling (`scrollInit` / `scrollUpdate`)
    if let Some((cols, _)) = &ds.rendered.scroll {
        match ds.phase {
            ScrollPhase::Pause(until) if now >= until => ds.phase = ScrollPhase::Running,
            ScrollPhase::Running => {
                let fonts_single = ds.fonts_key.contains("full");
                let px = p
                    .prefs
                    .get_i64(if fonts_single { "scrollPixelsDouble" } else { "scrollPixels" })
                    .clamp(1, 20) as usize;
                ds.offset += px;
                if ds.offset >= cols.len() + SCROLL_PAD {
                    ds.offset = 0;
                    ds.phase = if p.prefs.get_i64("scrollMode") == 1 {
                        ScrollPhase::Done
                    } else {
                        ScrollPhase::Pause(
                            now + p.prefs.get_f64(if fonts_single {
                                "scrollPauseDouble"
                            } else {
                                "scrollPause"
                            }),
                        )
                    };
                }
            }
            _ => {}
        }
    }

    if ds.sent_brightness != Some(screen.brightness) {
        p.send(ServerFrame::Grfb(screen.brightness));
        ds.sent_brightness = Some(screen.brightness);
    }
    if ds.sent_visu.as_ref() != Some(&screen.visu) {
        p.send(visu_frame(&screen.visu));
        ds.sent_visu = Some(screen.visu.clone());
    }
    let f = frame(&ds.rendered, ds.offset);
    if ds.sent.as_ref() != Some(&f) {
        p.send(ServerFrame::Grfe {
            offset: 0,
            transition: b'c',
            param: 0,
            bitmap: f.clone().into(),
        });
        ds.sent = Some(f);
    }
    if let Some(r2) = &ds.rendered2 {
        let f2 = frame(r2, 0);
        if ds.sent2.as_ref() != Some(&f2) {
            p.send(ServerFrame::Grfe {
                offset: 640,
                transition: b'c',
                param: 0,
                bitmap: f2.clone().into(),
            });
            ds.sent2 = Some(f2);
        }
    }
}

/// Background task: keep the displays of connected players up to date.
pub async fn run(server: Arc<Server>) {
    let mut states: HashMap<slimproto::MacAddress, DisplayState> = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let players = server.connected_players();
        for p in &players {
            let Some(kind) = kind_of(p) else {
                continue;
            };
            let connected_at = p.state.lock().connected_at;
            let ds = states
                .entry(p.id)
                .or_insert_with(|| DisplayState::new(connected_at));
            if ds.connected_at != connected_at {
                // reconnected: resend everything
                *ds = DisplayState::new(connected_at);
            }
            update_player(&server, p, kind, ds);
        }
        states.retain(|id, _| players.iter().any(|p| p.id == *id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_parse_and_render() {
        for (name, _) in FONT_FILES {
            assert!(font(name).is_some(), "{name} parses");
        }
        let std = FontSet::named("standard").unwrap();
        let l1 = std.line[0].unwrap();
        assert!(l1.glyphs.len() > 200, "latin-1 glyph table");
        let a = std.line[1].unwrap().render("A");
        assert!(!a.is_empty() && a.iter().any(|c| *c != 0));
        // line 1 glyphs live in the upper half, line 2 in the lower half of the 32 rows
        assert!(l1.render("A").iter().all(|c| c & 0x0000_ffff == 0));
        let frame = compose(320, "Now Playing (1 of 2)", "0:42", "Song - Artist");
        assert_eq!(frame.len(), 1280);
        assert!(frame.iter().any(|b| *b != 0));
        assert!(FontSet::named("full").unwrap().single_line());
        assert!(FontSet::named("nope").is_none());
    }

    #[test]
    fn slider_bar_is_exact_width() {
        let f = FontSet::named("standard").unwrap();
        for w in [10usize, 100, 320] {
            for v in [0.0, 37.0, 100.0] {
                assert_eq!(f.line[0].unwrap().render(&slider_bar(w, v, 0.0)).len(), w);
                assert_eq!(f.line[1].unwrap().render(&slider_bar(w, v, 0.0)).len(), w);
            }
        }
        // fuller bars light more pixels
        let px = |v| {
            f.line[1]
                .unwrap()
                .render(&slider_bar(100, v, 0.0))
                .iter()
                .map(|c| c.count_ones())
                .sum::<u32>()
        };
        assert!(px(80.0) > px(20.0));
    }

    #[test]
    fn progress_bar_fills_gap() {
        let f = FontSet::named("standard").unwrap();
        let parts = Parts {
            line: ["Now Playing".into(), "Song".into()],
            overlay: [" 01:00".into(), String::new()],
            progress: Some(0.5),
            ..Default::default()
        };
        let r = render(320, f, &parts, true);
        assert!(r.scroll.is_none());
        // the bar reaches from the end of line 1 to the time overlay
        let l1 = f.line[0].unwrap().render("Now Playing").len();
        assert!(r.bits[l1 + 5] != 0);
    }

    #[test]
    fn long_lines_scroll() {
        let f = FontSet::named("standard").unwrap();
        let long = "A very long title that will never ever fit on a Squeezebox display at all";
        let parts = Parts {
            line: ["Now Playing".into(), long.into()],
            ..Default::default()
        };
        let r = render(320, f, &parts, true);
        let (cols, area) = r.scroll.clone().expect("scrolls");
        assert_eq!(area, 320);
        assert!(cols.len() > 320);
        assert_ne!(frame(&r, 0), frame(&r, 10));
        // a full period brings it back to the start
        assert_eq!(frame(&r, 0), frame(&r, cols.len() + SCROLL_PAD));
        assert!(render(320, f, &parts, false).scroll.is_none());
    }

    #[test]
    fn single_line_font_shows_line_two() {
        let f = FontSet::named("full").unwrap();
        let parts = Parts {
            line: ["ignored".into(), "Hi".into()],
            ..Default::default()
        };
        let r = render(320, f, &parts, true);
        assert!(r.bits.iter().any(|c| *c != 0));
        // the full font is tall: uses both halves
        assert!(r.bits.iter().any(|c| c & 0xffff_0000 != 0) && r.bits.iter().any(|c| c & 0xffff != 0));
    }

    #[test]
    fn song_times() {
        assert_eq!(song_time(42.7, Some(200.0), false), "00:42");
        assert_eq!(song_time(42.0, Some(200.0), true), "-02:38");
        assert_eq!(song_time(3725.0, None, false), "1:02:05");
        assert_eq!(song_time(10.0, None, true), "00:10");
    }

    #[test]
    fn lms_date_formats() {
        use chrono::TimeZone;
        let t = chrono::Local.with_ymd_and_hms(2024, 3, 5, 9, 7, 0).unwrap();
        assert_eq!(lms_strftime("%A, %B |%d, %Y", t, true), "Tuesday, March 5, 2024");
        assert_eq!(lms_strftime("|%I:%M %p", t, false), "9:07 AM");
        assert_eq!(lms_strftime("%H:%M", t, false), "09:07");
    }

    #[test]
    fn title_formats() {
        let v = |e: &str| match e {
            "TITLE" => Some("Song".to_string()),
            "TRACKNUM" => Some("3".to_string()),
            "ARTIST" => None,
            _ => None,
        };
        assert_eq!(format_title("TRACKNUM. TITLE", &v), "3. Song");
        assert_eq!(format_title("DISC-TRACKNUM. TITLE", &v), "3. Song");
        assert_eq!(format_title("TITLE (ARTIST)", &v), "Song");
        let v2 = |e: &str| match e {
            "TITLE" => Some("Song".to_string()),
            "ARTIST" => Some("Band".to_string()),
            _ => None,
        };
        assert_eq!(format_title("TRACKNUM. TITLE (ARTIST)", &v2), "Song (Band)");
        assert_eq!(format_title("ARTIST - TITLE", &v2), "Band - Song");
    }

    #[test]
    fn visu_frame_packing() {
        let ServerFrame::Visu(b) = visu_frame(SB2_VU) else {
            panic!()
        };
        assert_eq!(&b[..2], &[1, 6]);
        assert_eq!(b.len(), 2 + 6 * 4);
        assert_eq!(&b[2..6], &[0, 0, 0, 0]);
        assert_eq!(&b[10..14], &280u32.to_be_bytes());
    }
}
