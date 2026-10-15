//! The display test on every player with a VFD: every pixel lit at full brightness until a
//! button is pressed.

use crate::common::*;
use std::time::{Duration, Instant};

struct Model {
    name: &'static str,
    device_id: u8,
    /// SB2 and SB3 (Classic) share a device id and differ by MAC range.
    mac: [u8; 6],
    width: usize,
    /// Brightness on (`powerOnBrightness`'s default) and the brightest level.
    normal: u16,
    brightest: u16,
}

const MODELS: &[Model] = &[
    Model {
        name: "Squeezebox2",
        device_id: 4,
        mac: [0x00, 0x04, 0x20, 0x05, 0x00, 0x01],
        width: 320,
        normal: 4,
        brightest: 4,
    },
    Model {
        name: "Squeezebox Classic",
        device_id: 4,
        mac: [0x00, 0x04, 0x20, 0x07, 0x00, 0x02],
        width: 320,
        normal: 4,
        brightest: 4,
    },
    Model {
        name: "Transporter",
        device_id: 5,
        mac: [0x00, 0x04, 0x20, 0x10, 0x00, 0x03],
        width: 320,
        normal: 4,
        brightest: 4,
    },
    Model {
        name: "Squeezebox Boom",
        device_id: 10,
        mac: [0x00, 0x04, 0x20, 0x16, 0x00, 0x04],
        width: 160,
        // automatic: (21 - sensitivity 10) * 256 + minimum 2
        normal: (21 - 10) * 256 + 2,
        brightest: 5,
    },
];

impl Model {
    fn mac_s(&self) -> String {
        self.mac
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    fn connect(&self, server: &Server) -> FakePlayer {
        FakePlayer::connect_as(server.slim, self.device_id, &self.mac, "flc,pcm,mp3")
    }
}

/// Wait for a `grfb` (brightness) frame with this value.
fn brightness(p: &mut FakePlayer, value: u16) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let b = p.expect(b"grfb", deadline.saturating_duration_since(Instant::now()));
        if b[..] == value.to_be_bytes() {
            return;
        }
    }
}

/// The bitmap of the next `grfe` frame at `offset` (0, or 640 for the Transporter's second
/// screen) that satisfies `want`.
fn screen(p: &mut FakePlayer, offset: u16, want: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let f = p.expect(b"grfe", left);
        if f[2] != b'c' {
            p.send(b"ANIC", &[]);
        }
        if f[..2] == offset.to_be_bytes() && want(&f[4..]) {
            return f[4..].to_vec();
        }
    }
}

fn all_on(b: &[u8]) -> bool {
    b.iter().all(|&x| x == 0xff)
}

#[test]
fn all_pixels_on_until_a_button() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    for m in MODELS {
        let mut p = m.connect(&server);
        let mac = m.mac_s();
        // the normal screen first
        let normal = screen(&mut p, 0, |b| !all_on(b));
        assert_eq!(normal.len(), m.width * 4, "{}", m.name);

        cli(server.cli, &format!("{mac} button displaytest"));
        if m.normal != m.brightest {
            brightness(&mut p, m.brightest);
        }
        let lit = screen(&mut p, 0, all_on);
        assert_eq!(lit.len(), m.width * 4, "{}: the whole display", m.name);
        if m.device_id == 5 {
            screen(&mut p, 640, all_on);
        }

        // the press that started it doesn't end it, the next one does
        std::thread::sleep(Duration::from_millis(600));
        cli(server.cli, &format!("{mac} button arrow_left"));
        if m.normal != m.brightest {
            brightness(&mut p, m.normal);
        }
        screen(&mut p, 0, |b| !all_on(b));
    }
}

/// From the player's own menus: Settings → Advanced → Player Display → Display Test.
#[test]
fn display_test_in_the_menus() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    for m in MODELS {
        let mut p = m.connect(&server);
        screen(&mut p, 0, |_| true);
        // Settings; up wraps to Advanced, the last item; Player Display is its first; Display
        // Test the last item of that
        for b in [
            "menu_settings",
            "arrow_up",
            "arrow_right",
            "arrow_right",
            "arrow_up",
            "arrow_right",
        ] {
            cli(server.cli, &format!("{} button {b}", m.mac_s()));
            std::thread::sleep(Duration::from_millis(300));
        }
        screen(&mut p, 0, all_on);
    }
}
