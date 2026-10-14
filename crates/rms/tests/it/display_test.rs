//! The display test on a Boom: every pixel lit at full brightness until a button is pressed.

use crate::common::*;
use std::time::{Duration, Instant};

const BOOM_MAC: [u8; 6] = [0x00, 0x04, 0x20, 0xb0, 0x0b, 0x01];
const BOOM: &str = "00:04:20:b0:0b:01";

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

/// The bitmap of the next full-screen `grfe` frame (after its offset, transition and
/// parameter bytes) that satisfies `want`.
fn screen(p: &mut FakePlayer, want: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let f = p.expect(b"grfe", left);
        if f[2] != b'c' {
            p.send(b"ANIC", &[]);
        }
        if want(&f[4..]) {
            return f[4..].to_vec();
        }
    }
}

#[test]
fn boom_all_pixels_on_until_a_button() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect_as(
        server.slim,
        10,
        &BOOM_MAC,
        "Model=boom,ModelName=Squeezebox Boom,Firmware=59,flc,pcm,mp3",
    );
    // the normal screen first
    let normal = screen(&mut p, |b| b.iter().any(|&x| x != 0xff));
    assert_eq!(normal.len(), 160 * 4);

    cli(server.cli, &format!("{BOOM} button displaytest"));
    // the brightest manual level (the default is automatic: (21 - 10) * 256 + 2)
    brightness(&mut p, 5);
    let all_on = screen(&mut p, |b| b.iter().all(|&x| x == 0xff));
    assert_eq!(all_on.len(), 160 * 4, "the Boom's whole 160x32 display");

    // the press that started it doesn't end it, the next one does
    std::thread::sleep(Duration::from_millis(600));
    cli(server.cli, &format!("{BOOM} button arrow_left"));
    brightness(&mut p, (21 - 10) * 256 + 2);
    let back = screen(&mut p, |b| b.iter().any(|&x| x != 0xff));
    assert_eq!(back.len(), 160 * 4);
}

/// From the player's own menus: Settings → Advanced → Player Display → Display Test.
#[test]
fn boom_display_test_in_the_menus() {
    let music = tempfile::tempdir().unwrap();
    let server = start_server(music.path());
    let mut p = FakePlayer::connect_as(
        server.slim,
        10,
        &BOOM_MAC,
        "Model=boom,ModelName=Squeezebox Boom,Firmware=59,flc,pcm,mp3",
    );
    screen(&mut p, |_| true);
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
        cli(server.cli, &format!("{BOOM} button {b}"));
        std::thread::sleep(Duration::from_millis(300));
    }
    screen(&mut p, |b| b.iter().all(|&x| x == 0xff));
}
