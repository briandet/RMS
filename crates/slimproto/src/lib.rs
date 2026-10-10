//! Wire-level implementation of the Squeezebox control protocol ("slimproto").
//!
//! The protocol runs over a TCP connection from the player to the server on port 3483.
//!
//! * Player → server frames: `4 ASCII op bytes | u32 BE payload length | payload`
//! * Server → player frames: `u16 BE (payload length + 4) | 4 ASCII op bytes | payload`
//!
//! This crate is deliberately I/O free apart from the [`tokio_util::codec`] adaptors in
//! [`codec`]; it only parses and builds byte buffers so it can be unit tested exhaustively.
//! Layouts follow the reference server (Lyrion Music Server, `Slim::Networking::Slimproto`
//! and `Slim::Player::Squeezebox*`) byte for byte.

pub mod client;
pub mod codec;
pub mod discovery;
pub mod server;

pub use client::{
    ClientMessage, DisconnectReason, Helo, Stat, StatEvent, DEVICE_IDS,
};
pub use server::{
    AudioGain, Format, PcmEndian, PcmSampleRate, PcmSampleSize, ServerFrame, StrmCommand, StrmHeader,
    TransitionType,
};

/// Errors raised while decoding frames from a player.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("frame payload too short for {op}: need {need} bytes, got {got}")]
    Short { op: &'static str, need: usize, got: usize },
    #[error("declared frame length {0} exceeds maximum")]
    TooLarge(u32),
    #[error("invalid value in {op}: {detail}")]
    Invalid { op: &'static str, detail: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Largest payload we will accept from a player.  Firmware never sends anything close to
/// this; it exists only to avoid pathological allocations on garbage input.
pub const MAX_CLIENT_FRAME: u32 = 1 << 20;

/// A MAC address as used for player identity.  Formatted lower-case `aa:bb:cc:dd:ee:ff`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    pub fn parse(s: &str) -> Option<MacAddress> {
        let mut out = [0u8; 6];
        let mut n = 0;
        for part in s.split([':', '-']) {
            if n >= 6 || part.len() != 2 {
                return None;
            }
            out[n] = u8::from_str_radix(part, 16).ok()?;
            n += 1;
        }
        (n == 6).then_some(MacAddress(out))
    }
}

impl std::fmt::Display for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let m = &self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

impl std::fmt::Debug for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MacAddress({})", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_roundtrip() {
        let m = MacAddress::parse("00:04:20:1A:2b:3C").unwrap();
        assert_eq!(m.to_string(), "00:04:20:1a:2b:3c");
        assert!(MacAddress::parse("00:04:20").is_none());
        assert!(MacAddress::parse("zz:04:20:1a:2b:3c").is_none());
    }
}
