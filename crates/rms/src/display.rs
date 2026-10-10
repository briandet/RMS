//! Graphical displays of Squeezebox2/3/Classic, Transporter and Boom
//! (`Slim::Display::Squeezebox2`, `Slim::Display::Lib::Fonts`).
//!
//! Renders a simple "Now Playing" screen with Lyrion's own bitmap fonts and sends it as
//! `grfe` frames.  Players without a display (squeezelite, WiiM, SqueezePlay) are skipped.

use crate::player::Player;
use crate::server::Server;
use parking_lot::Mutex;
use slimproto::server::ServerFrame;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// A parsed `*.font.bmp`: one glyph per Latin-1 code point, each glyph a list of 32-bit
/// columns (MSB = top row).
pub struct Font {
    glyphs: Vec<Vec<u32>>,
}

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

    /// Render text to columns (glyph, then interspace).
    pub fn render(&self, text: &str) -> Vec<u32> {
        let space = self.glyphs.first().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for ch in text.chars() {
            let cp = ch as u32;
            let idx = if (1..=255).contains(&cp) && (cp as usize) < self.glyphs.len() {
                cp as usize
            } else {
                '?' as usize
            };
            if let Some(g) = self.glyphs.get(idx) {
                out.extend_from_slice(g);
                out.extend_from_slice(&space);
            }
        }
        out
    }
}

struct Fonts {
    line1: Font,
    line2: Font,
}

fn fonts() -> Option<&'static Fonts> {
    static F: OnceLock<Option<Fonts>> = OnceLock::new();
    F.get_or_init(|| {
        Some(Fonts {
            line1: Font::parse(include_bytes!("../assets/fonts/standard.1.font.bmp"))?,
            line2: Font::parse(include_bytes!("../assets/fonts/standard.2.font.bmp"))?,
        })
    })
    .as_ref()
}

/// Display width in pixels for a player model, or None without a graphic display.
pub fn display_width(model: &str) -> Option<usize> {
    match model {
        "squeezebox2" | "squeezebox3" | "transporter" => Some(320),
        "boom" => Some(160),
        _ => None,
    }
}

/// Compose a 32-pixel-high frame: line 1 (with right-aligned overlay) and line 2.
pub fn compose(width: usize, line1: &str, overlay1: &str, line2: &str) -> Vec<u8> {
    let mut cols = vec![0u32; width];
    if let Some(f) = fonts() {
        let l1 = f.line1.render(line1);
        let ov = f.line1.render(overlay1);
        let l2 = f.line2.render(line2);
        let ov_start = width.saturating_sub(ov.len());
        for (i, c) in l1.iter().enumerate().take(if ov.is_empty() {
            width
        } else {
            ov_start.saturating_sub(4)
        }) {
            cols[i] |= c;
        }
        for (i, c) in ov.iter().enumerate() {
            if ov_start + i < width {
                cols[ov_start + i] |= c;
            }
        }
        for (i, c) in l2.iter().enumerate().take(width) {
            cols[i] |= c;
        }
    }
    cols.iter().flat_map(|c| c.to_be_bytes()).collect()
}

/// `brightnessMap` of the Squeezebox2 display: pref level 0..4 → frame value.
fn brightness_value(level: i64) -> u16 {
    [65535u16, 0, 1, 3, 4][level.clamp(0, 4) as usize]
}

/// Text for the current player state.
fn screen_for(player: &Arc<Player>) -> (String, String, String) {
    let now = chrono::Local::now();
    if !player.power() {
        return (
            now.format("%A, %B %-d, %Y").to_string(),
            String::new(),
            now.format("%-I:%M %p").to_string(),
        );
    }
    if let Some((lines, until)) = player.state.lock().show_briefly.clone() {
        if until > crate::util::now() {
            return (
                lines.first().cloned().unwrap_or_default(),
                String::new(),
                lines.get(1).cloned().unwrap_or_default(),
            );
        }
    }
    let c = player.controller();
    let c = c.lock();
    let count = c.playlist.len();
    if count == 0 {
        return (player.name(), String::new(), crate::strings::s("NOTHING"));
    }
    let idx = c.playing_song_index();
    let state = match c.mode() {
        "play" => "NOW_PLAYING",
        "pause" => "PAUSED",
        _ => "STOPPED",
    };
    let line1 = format!(
        "{} ({} {} {})",
        crate::strings::s(state),
        idx + 1,
        crate::strings::s("OUT_OF"),
        count
    );
    let (title, artist) = match c.playing_song() {
        Some(s) => (
            crate::player::controller::standard_title(s),
            s.track.artist.clone().or(s.remote_meta.artist.clone()),
        ),
        None => {
            let info = c.playlist.song(idx).and_then(|e| {
                c.master()
                    .and_then(|m| m.server())
                    .map(|srv| srv.track_info_for(e))
            });
            match info {
                Some(i) => (
                    i.title
                        .clone()
                        .unwrap_or_else(|| crate::player::controller::title_from_url(&i.url)),
                    i.artist,
                ),
                None => (String::new(), None),
            }
        }
    };
    let line2 = match artist {
        Some(a) if !a.is_empty() && !title.contains(&a) => format!("{} - {}", title, a),
        _ => title,
    };
    let elapsed = if c.is_stopped() {
        String::new()
    } else {
        crate::util::format_duration(c.playing_song_elapsed())
    };
    (line1, elapsed, line2)
}

/// Background task: refresh displays once per second when their content changes.
pub async fn run(server: Arc<Server>) {
    let sent: Mutex<HashMap<slimproto::MacAddress, (Vec<u8>, u16)>> = Mutex::new(HashMap::new());
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        for p in server.connected_players() {
            let model = p.state.lock().caps.model.clone();
            let Some(width) = display_width(&model) else {
                continue;
            };
            let (l1, ov, l2) = screen_for(&p);
            let frame = compose(width, &l1, &ov, &l2);
            let level = p.prefs.get_i64(if p.power() {
                "powerOnBrightness"
            } else {
                "powerOffBrightness"
            });
            let bright = brightness_value(level);
            let mut sent = sent.lock();
            let prev = sent.get(&p.id);
            if prev.map(|(f, b)| *b != bright).unwrap_or(true) {
                p.send(ServerFrame::Grfb(bright));
            }
            if prev.map(|(f, _)| *f != frame).unwrap_or(true) {
                p.send(ServerFrame::Grfe {
                    offset: 0,
                    transition: b'c',
                    param: 0,
                    bitmap: frame.clone().into(),
                });
                sent.insert(p.id, (frame, bright));
            } else if let Some(e) = sent.get_mut(&p.id) {
                e.1 = bright;
            }
        }
        // forget players that reconnected so they get a full refresh
        sent.lock().retain(|id, _| {
            server
                .player(id)
                .map(|p| p.is_connected() && p.state.lock().connected_at < crate::util::now() - 1.0)
                .unwrap_or(false)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_parse_and_render() {
        let f = fonts().expect("embedded fonts parse");
        assert!(f.line1.glyphs.len() > 200, "latin-1 glyph table");
        let a = f.line2.render("A");
        assert!(!a.is_empty() && a.iter().any(|c| *c != 0));
        // line 1 glyphs live in the upper half, line 2 in the lower half of the 32 rows
        assert!(f.line1.render("A").iter().all(|c| c & 0x0000_ffff == 0));
        let frame = compose(320, "Now Playing (1 of 2)", "0:42", "Song - Artist");
        assert_eq!(frame.len(), 1280);
        assert!(frame.iter().any(|b| *b != 0));
    }
}
