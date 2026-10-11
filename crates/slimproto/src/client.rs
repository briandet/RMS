//! Messages sent from the player to the server.

use crate::{Error, MacAddress, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Device-id → model string table (`@deviceids` in the reference server).  Index is the
/// device id byte from HELO.
pub const DEVICE_IDS: [Option<&str>; 13] = [
    None,
    None,
    Some("squeezebox"),
    Some("softsqueeze"),
    Some("squeezebox2"),
    Some("transporter"),
    Some("softsqueeze3"),
    Some("receiver"),
    Some("squeezeslave"),
    Some("controller"),
    Some("boom"),
    Some("softboom"),
    Some("squeezeplay"),
];

/// Decoded `HELO` frame.
///
/// Layout (payload):
/// ```text
/// 0    u8   device id
/// 1    u8   firmware revision
/// 2    6    MAC address
/// 8    16   UUID (only when payload >= 36 bytes)
/// 8/24 u16  wlan channel list; bit15 = bitmapped display, bit14 = reconnect
/// +2   u64  bytes received (high u32, low u32)
/// +8   2    language code (ASCII)
/// 36.. str  capabilities (comma separated), SqueezePlay/squeezelite only
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helo {
    pub device_id: u8,
    pub revision: u8,
    pub mac: MacAddress,
    pub uuid: Option<[u8; 16]>,
    pub wlan_channel_list: u16,
    pub bitmapped: bool,
    pub reconnect: bool,
    pub bytes_received: Option<u64>,
    pub language: Option<[u8; 2]>,
    pub capabilities: Option<String>,
}

impl Helo {
    pub fn model(&self) -> Option<&'static str> {
        DEVICE_IDS.get(self.device_id as usize).copied().flatten()
    }

    /// UUID as a 32-char lower-case hex string (the form the reference server uses).
    pub fn uuid_hex(&self) -> Option<String> {
        self.uuid
            .map(|u| u.iter().map(|b| format!("{:02x}", b)).collect())
    }

    /// The `SyncgroupID=<10 digits>` capability, if any.
    pub fn syncgroup_id(&self) -> Option<u64> {
        self.capability_value("SyncgroupID").and_then(|v| {
            (v.len() == 10 && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse().ok())
                .flatten()
        })
    }

    /// Look up a `Key=value` capability; a bare `Key` yields `"1"`.
    pub fn capability_value(&self, key: &str) -> Option<&str> {
        let caps = self.capabilities.as_deref()?;
        for cap in caps.split(',') {
            match cap.split_once('=') {
                Some((k, v)) if k == key => return Some(v),
                None if cap == key => return Some("1"),
                _ => {}
            }
        }
        None
    }

    /// Codec list declared in the capabilities string: every token matching `^[a-z][a-z0-9]{1,4}$`.
    pub fn declared_formats(&self) -> Vec<String> {
        let Some(caps) = self.capabilities.as_deref() else {
            return Vec::new();
        };
        caps.split(',')
            .filter(|c| {
                let b = c.as_bytes();
                (2..=5).contains(&b.len())
                    && b[0].is_ascii_lowercase()
                    && b[1..]
                        .iter()
                        .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit())
            })
            .map(str::to_owned)
            .collect()
    }

    /// The payload as a player sends it (what [`Helo::parse`] reads).
    pub fn encode(&self, out: &mut BytesMut) {
        out.put_u8(self.device_id);
        out.put_u8(self.revision);
        out.put_slice(&self.mac.0);
        out.put_slice(&self.uuid.unwrap_or([0; 16]));
        let mut wlan = self.wlan_channel_list & 0x3fff;
        if self.bitmapped {
            wlan |= 0x8000;
        }
        if self.reconnect {
            wlan |= 0x4000;
        }
        out.put_u16(wlan);
        out.put_u64(self.bytes_received.unwrap_or(0));
        out.put_slice(&self.language.unwrap_or(*b"EN"));
        if let Some(c) = &self.capabilities {
            out.put_slice(c.as_bytes());
        }
    }

    pub fn parse(mut p: &[u8]) -> Result<Helo> {
        let total = p.len();
        if total < 10 {
            return Err(Error::Short {
                op: "HELO",
                need: 10,
                got: total,
            });
        }
        let device_id = p.get_u8();
        let revision = p.get_u8();
        let mut mac = [0u8; 6];
        p.copy_to_slice(&mut mac);
        let uuid = if total >= 36 {
            let mut u = [0u8; 16];
            p.copy_to_slice(&mut u);
            Some(u)
        } else {
            None
        };
        // Older firmware may send only the 10-byte header; the remaining fields are optional.
        let mut wlan = 0u16;
        let mut bytes_received = None;
        let mut language = None;
        if p.remaining() >= 2 {
            wlan = p.get_u16();
        }
        if p.remaining() >= 8 {
            let hi = p.get_u32() as u64;
            let lo = p.get_u32() as u64;
            bytes_received = Some((hi << 32) | lo);
        }
        if p.remaining() >= 2 {
            let mut l = [0u8; 2];
            p.copy_to_slice(&mut l);
            language = Some(l);
        }
        let capabilities = if total > 36 {
            Some(
                String::from_utf8_lossy(p)
                    .trim_end_matches('\0')
                    .to_string(),
            )
        } else {
            None
        };
        Ok(Helo {
            device_id,
            revision,
            mac: MacAddress(mac),
            uuid,
            wlan_channel_list: wlan & 0x3fff,
            bitmapped: wlan & 0x8000 != 0,
            reconnect: wlan & 0x4000 != 0,
            bytes_received,
            language,
            capabilities,
        })
    }
}

/// Four-character event code carried in a STAT frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatEvent {
    /// `STMa` autostart: player started decoding automatically.
    Autostart,
    /// `STMc` connect: player received the strm 's' command and is connecting.
    Connect,
    /// `STMe` establish: stream TCP connection established.
    Establish,
    /// `STMf` close: stream connection closed (flush/stop acknowledged).
    Flushed,
    /// `STMh` end of headers.
    HeadersDone,
    /// `STMp` pause acknowledged.
    Paused,
    /// `STMr` resume acknowledged.
    Resumed,
    /// `STMt` timer / heartbeat (also reply to strm 't').
    Timer,
    /// `STMu` underrun: output buffer ran dry — end of playback for this stream.
    Underrun,
    /// `STMl` buffer threshold reached (ready to start when autostart is off).
    BufferReady,
    /// `STMd` decoder ready: decode of current stream complete, send the next one.
    DecoderReady,
    /// `STMs` track started: first samples of new stream are playing.
    TrackStarted,
    /// `STMn` not supported / decode error.
    NotSupported,
    /// `STMo` output underrun (temporary).
    OutputUnderrun,
    /// `STMz`/`EoS` pseudo-event raised by the server from a clean DSCO.
    EndOfStream,
    /// Old `vfdc` / `i2cc` acknowledgements or anything unknown.
    Other([u8; 4]),
}

impl StatEvent {
    pub fn from_code(c: [u8; 4]) -> StatEvent {
        match &c {
            b"STMa" => StatEvent::Autostart,
            b"STMc" => StatEvent::Connect,
            b"STMe" => StatEvent::Establish,
            b"STMf" => StatEvent::Flushed,
            b"STMh" => StatEvent::HeadersDone,
            b"STMp" => StatEvent::Paused,
            b"STMr" => StatEvent::Resumed,
            b"STMt" => StatEvent::Timer,
            b"STMu" => StatEvent::Underrun,
            b"STMl" => StatEvent::BufferReady,
            b"STMd" => StatEvent::DecoderReady,
            b"STMs" => StatEvent::TrackStarted,
            b"STMn" => StatEvent::NotSupported,
            b"STMo" => StatEvent::OutputUnderrun,
            b"STMz" => StatEvent::EndOfStream,
            _ => StatEvent::Other(c),
        }
    }

    pub fn code(&self) -> &str {
        match self {
            StatEvent::Autostart => "STMa",
            StatEvent::Connect => "STMc",
            StatEvent::Establish => "STMe",
            StatEvent::Flushed => "STMf",
            StatEvent::HeadersDone => "STMh",
            StatEvent::Paused => "STMp",
            StatEvent::Resumed => "STMr",
            StatEvent::Timer => "STMt",
            StatEvent::Underrun => "STMu",
            StatEvent::BufferReady => "STMl",
            StatEvent::DecoderReady => "STMd",
            StatEvent::TrackStarted => "STMs",
            StatEvent::NotSupported => "STMn",
            StatEvent::OutputUnderrun => "STMo",
            StatEvent::EndOfStream => "STMz",
            StatEvent::Other(_) => "????",
        }
    }
}

/// Decoded `STAT` frame.
///
/// ```text
/// 0   a4   event code
/// 4   u8   num_crlf
/// 5   u8   mas_initialized
/// 6   u8   mas_mode
/// 7   u32  buffer size (decode/stream buffer)   [SB1 fw<20: rptr]
/// 11  u32  buffer fullness                      [SB1 fw<20: wptr]
/// 15  u64  bytes received
/// 23  u16  signal strength
/// 25  u32  jiffies (player ms clock)
/// 29  u32  output buffer size
/// 33  u32  output buffer fullness
/// 37  u32  elapsed seconds
/// 41  u16  voltage
/// 43  u32  elapsed milliseconds
/// 47  u32  server timestamp (echo of strm 't')
/// 51  u16  error code (only trusted when payload is 53 or 57 bytes)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Stat {
    pub event_code: [u8; 4],
    pub num_crlf: u8,
    pub mas_initialized: u8,
    pub mas_mode: u8,
    pub buffer_size: u32,
    pub fullness: u32,
    pub bytes_received: u64,
    pub signal_strength: u16,
    pub jiffies: u32,
    pub output_buffer_size: u32,
    pub output_buffer_fullness: u32,
    pub elapsed_seconds: u32,
    pub voltage: u16,
    pub elapsed_milliseconds: u32,
    pub server_timestamp: u32,
    pub error_code: u16,
}

impl Stat {
    pub fn event(&self) -> StatEvent {
        StatEvent::from_code(self.event_code)
    }

    /// The payload as current firmware sends it (53 bytes, with the error code).
    pub fn encode(&self, out: &mut BytesMut) {
        out.put_slice(&self.event_code);
        out.put_u8(self.num_crlf);
        out.put_u8(self.mas_initialized);
        out.put_u8(self.mas_mode);
        out.put_u32(self.buffer_size);
        out.put_u32(self.fullness);
        out.put_u64(self.bytes_received);
        out.put_u16(self.signal_strength);
        out.put_u32(self.jiffies);
        out.put_u32(self.output_buffer_size);
        out.put_u32(self.output_buffer_fullness);
        out.put_u32(self.elapsed_seconds);
        out.put_u16(self.voltage);
        out.put_u32(self.elapsed_milliseconds);
        out.put_u32(self.server_timestamp);
        out.put_u16(self.error_code);
    }

    pub fn parse(mut p: &[u8]) -> Result<Stat> {
        let len = p.len();
        if len < 43 {
            return Err(Error::Short {
                op: "STAT",
                need: 43,
                got: len,
            });
        }
        let mut event_code = [0u8; 4];
        p.copy_to_slice(&mut event_code);
        let num_crlf = p.get_u8();
        let mas_initialized = p.get_u8();
        let mas_mode = p.get_u8();
        let buffer_size = p.get_u32();
        let fullness = p.get_u32();
        let bytes_received = p.get_u64();
        let signal_strength = p.get_u16();
        let jiffies = p.get_u32();
        let output_buffer_size = p.get_u32();
        let output_buffer_fullness = p.get_u32();
        let elapsed_seconds = p.get_u32();
        let voltage = p.get_u16();
        let elapsed_milliseconds = if p.remaining() >= 4 { p.get_u32() } else { 0 };
        let server_timestamp = if p.remaining() >= 4 { p.get_u32() } else { 0 };
        // Older firmware does not send error_code; 57 = current fw (4 junk bytes), 53 = exact.
        let error_code = if (len == 53 || len == 57) && p.remaining() >= 2 {
            p.get_u16()
        } else {
            0
        };
        Ok(Stat {
            event_code,
            num_crlf,
            mas_initialized,
            mas_mode,
            buffer_size,
            fullness,
            bytes_received,
            signal_strength,
            jiffies,
            output_buffer_size,
            output_buffer_fullness,
            elapsed_seconds,
            voltage,
            elapsed_milliseconds,
            server_timestamp,
            error_code,
        })
    }
}

/// Reason byte of a `DSCO` (data-channel disconnect) frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// 0 — connection closed normally (TCP FIN).
    Normal,
    /// 1 — reset by local host.
    LocalReset,
    /// 2 — reset by remote host.
    RemoteReset,
    /// 3 — unreachable.
    Unreachable,
    /// 4 — timed out.
    Timeout,
    Unknown(u8),
}

impl DisconnectReason {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Self::Normal,
            1 => Self::LocalReset,
            2 => Self::RemoteReset,
            3 => Self::Unreachable,
            4 => Self::Timeout,
            x => Self::Unknown(x),
        }
    }
    pub fn description(&self) -> &'static str {
        match self {
            Self::Normal => "Connection closed normally",
            Self::LocalReset => "Connection reset by local host",
            Self::RemoteReset => "Connection reset by remote host",
            Self::Unreachable => "Connection is no longer able to work",
            Self::Timeout => "Connection timed out",
            Self::Unknown(_) => "Unknown disconnect reason",
        }
    }
}

/// Every frame a player can send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Helo(Helo),
    Stat(Stat),
    /// `BYE!` — payload byte 1 means "going out for firmware upgrade".
    Bye {
        upgrading: bool,
    },
    /// `RESP` — the HTTP response headers received on the stream connection.
    Resp(Bytes),
    /// `BODY` — body data of a direct-stream response (used for remote playlists).
    Body(Bytes),
    /// `META` — ICY metadata block received on a direct stream.
    Meta(Bytes),
    /// `DSCO` — stream (data channel) disconnected.
    Dsco(DisconnectReason),
    /// `SETD` — player setting response: setting id followed by value bytes.
    Setd {
        id: u8,
        value: Bytes,
    },
    /// `ANIC` — display animation complete.
    Anic,
    /// `BUTN` — hard button: time (jiffies) + button code.
    Butn {
        time: u32,
        button: u32,
    },
    /// `IR  ` — IR code: time (jiffies), 2 reserved bytes, 32-bit code.
    Ir {
        time: u32,
        code: u32,
    },
    /// `KNOB` — Transporter knob: time, position, sync.
    Knob {
        time: u32,
        position: u32,
        sync: u8,
    },
    /// `RAWI` — raw IR data.
    RawIr(Bytes),
    /// `ALSS` — ambient light sensor.
    Alss {
        time: u32,
        lux: u32,
        channel_0: u16,
        channel_1: u16,
    },
    /// `UREQ` — player requests a firmware update.
    Ureq,
    /// `DBUG` — debug text from the firmware.
    Dbug(String),
    /// `SHUT` — proxy shutdown notice.
    Shut,
    /// `HELO`-less op we do not understand.
    Unknown {
        op: [u8; 4],
        payload: Bytes,
    },
}

impl ClientMessage {
    pub fn op(&self) -> &'static str {
        match self {
            ClientMessage::Helo(_) => "HELO",
            ClientMessage::Stat(_) => "STAT",
            ClientMessage::Bye { .. } => "BYE!",
            ClientMessage::Resp(_) => "RESP",
            ClientMessage::Body(_) => "BODY",
            ClientMessage::Meta(_) => "META",
            ClientMessage::Dsco(_) => "DSCO",
            ClientMessage::Setd { .. } => "SETD",
            ClientMessage::Anic => "ANIC",
            ClientMessage::Butn { .. } => "BUTN",
            ClientMessage::Ir { .. } => "IR  ",
            ClientMessage::Knob { .. } => "KNOB",
            ClientMessage::RawIr(_) => "RAWI",
            ClientMessage::Alss { .. } => "ALSS",
            ClientMessage::Ureq => "UREQ",
            ClientMessage::Dbug(_) => "DBUG",
            ClientMessage::Shut => "SHUT",
            ClientMessage::Unknown { .. } => "????",
        }
    }

    /// Parse one complete frame payload for the given 4-byte op.
    pub fn parse(op: [u8; 4], payload: Bytes) -> Result<ClientMessage> {
        let short = |need: usize, name: &'static str| Error::Short {
            op: name,
            need,
            got: payload.len(),
        };
        Ok(match &op {
            b"HELO" => ClientMessage::Helo(Helo::parse(&payload)?),
            b"STAT" => ClientMessage::Stat(Stat::parse(&payload)?),
            b"BYE!" => ClientMessage::Bye {
                upgrading: payload.as_ref() == [1u8],
            },
            b"RESP" => ClientMessage::Resp(payload),
            b"BODY" => ClientMessage::Body(payload),
            b"META" => ClientMessage::Meta(payload),
            b"DSCO" => {
                let b = *payload.first().ok_or_else(|| short(1, "DSCO"))?;
                ClientMessage::Dsco(DisconnectReason::from_u8(b))
            }
            b"SETD" => {
                let id = *payload.first().ok_or_else(|| short(1, "SETD"))?;
                ClientMessage::Setd {
                    id,
                    value: payload.slice(1..),
                }
            }
            b"ANIC" => ClientMessage::Anic,
            b"BUTN" => {
                if payload.len() < 8 {
                    return Err(short(8, "BUTN"));
                }
                let mut p = payload.as_ref();
                ClientMessage::Butn {
                    time: p.get_u32(),
                    button: p.get_u32(),
                }
            }
            b"IR  " => {
                if payload.len() < 10 {
                    return Err(short(10, "IR  "));
                }
                let mut p = payload.as_ref();
                let time = p.get_u32();
                p.advance(2);
                ClientMessage::Ir {
                    time,
                    code: p.get_u32(),
                }
            }
            b"KNOB" => {
                if payload.len() < 9 {
                    return Err(short(9, "KNOB"));
                }
                let mut p = payload.as_ref();
                ClientMessage::Knob {
                    time: p.get_u32(),
                    position: p.get_u32(),
                    sync: p.get_u8(),
                }
            }
            b"RAWI" => ClientMessage::RawIr(payload),
            b"ALSS" => {
                if payload.len() < 13 {
                    return Err(short(13, "ALSS"));
                }
                let mut p = payload.as_ref();
                let _packet_rev = p.get_u8();
                ClientMessage::Alss {
                    time: p.get_u32(),
                    lux: p.get_u32(),
                    channel_0: p.get_u16(),
                    channel_1: p.get_u16(),
                }
            }
            b"UREQ" => ClientMessage::Ureq,
            b"DBUG" => ClientMessage::Dbug(String::from_utf8_lossy(&payload).into_owned()),
            b"SHUT" => ClientMessage::Shut,
            _ => ClientMessage::Unknown { op, payload },
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn helo_and_stat_round_trip() {
        let helo = Helo {
            device_id: 12,
            revision: 2,
            mac: MacAddress([0, 4, 0x20, 1, 2, 3]),
            uuid: Some([7; 16]),
            wlan_channel_list: 0x7ff,
            bitmapped: false,
            reconnect: true,
            bytes_received: Some(1234),
            language: Some(*b"EN"),
            capabilities: Some("Model=squeezelite,MaxSampleRate=48000,mp3,flc".into()),
        };
        let mut b = BytesMut::new();
        helo.encode(&mut b);
        assert_eq!(Helo::parse(&b).unwrap(), helo);
        let stat = Stat {
            event_code: *b"STMt",
            buffer_size: 1 << 20,
            fullness: 1 << 19,
            bytes_received: 99,
            jiffies: 123456,
            elapsed_seconds: 61,
            elapsed_milliseconds: 61_500,
            server_timestamp: 77,
            ..Default::default()
        };
        let mut b = BytesMut::new();
        stat.encode(&mut b);
        assert_eq!(b.len(), 53);
        assert_eq!(Stat::parse(&b).unwrap(), stat);
    }
    use super::*;

    fn squeezelite_helo() -> Vec<u8> {
        // device id 12, revision 0, mac, 16 zero uuid, wlan 0x0000, bytes 0, lang "EN", caps
        let mut v = vec![12u8, 0, 0x00, 0x04, 0x20, 0xaa, 0xbb, 0xcc];
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&0u64.to_be_bytes());
        v.extend_from_slice(b"EN");
        v.extend_from_slice(b"Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,Firmware=v1.9.9-1419,ModelName=SqueezeLite,MaxSampleRate=192000,SyncgroupID=1234567890,ogg,flc,pcm,mp3,aac,alc");
        v
    }

    #[test]
    fn parse_helo_long() {
        let h = Helo::parse(&squeezelite_helo()).unwrap();
        assert_eq!(h.device_id, 12);
        assert_eq!(h.model(), Some("squeezeplay"));
        assert_eq!(h.mac.to_string(), "00:04:20:aa:bb:cc");
        assert_eq!(h.language, Some(*b"EN"));
        assert_eq!(h.bytes_received, Some(0));
        assert_eq!(h.capability_value("Model"), Some("squeezelite"));
        assert_eq!(h.capability_value("MaxSampleRate"), Some("192000"));
        assert_eq!(h.syncgroup_id(), Some(1234567890));
        assert_eq!(
            h.declared_formats(),
            vec!["ogg", "flc", "pcm", "mp3", "aac", "alc"]
        );
        assert_eq!(h.uuid_hex().unwrap(), "0".repeat(32));
    }

    #[test]
    fn parse_helo_short() {
        // 20 byte form: no uuid
        let mut v = vec![4u8, 130, 0x00, 0x04, 0x20, 0x11, 0x22, 0x33];
        v.extend_from_slice(&0xC001u16.to_be_bytes()); // bitmapped + reconnect + channel bits
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&2u32.to_be_bytes());
        v.extend_from_slice(b"EN");
        let h = Helo::parse(&v).unwrap();
        assert_eq!(h.model(), Some("squeezebox2"));
        assert!(h.bitmapped && h.reconnect);
        assert_eq!(h.wlan_channel_list, 1);
        assert_eq!(h.bytes_received, Some((1u64 << 32) | 2));
        assert!(h.uuid.is_none());
        assert!(h.capabilities.is_none());
    }

    #[test]
    fn parse_stat() {
        let mut v = Vec::new();
        v.extend_from_slice(b"STMt");
        v.extend_from_slice(&[0, b'm', 0]);
        v.extend_from_slice(&3_000_000u32.to_be_bytes());
        v.extend_from_slice(&123_456u32.to_be_bytes());
        v.extend_from_slice(&((1u64 << 32) + 7).to_be_bytes());
        v.extend_from_slice(&0xffffu16.to_be_bytes());
        v.extend_from_slice(&99_999u32.to_be_bytes());
        v.extend_from_slice(&3_528_000u32.to_be_bytes());
        v.extend_from_slice(&1_000u32.to_be_bytes());
        v.extend_from_slice(&42u32.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v.extend_from_slice(&42_500u32.to_be_bytes());
        v.extend_from_slice(&777u32.to_be_bytes());
        v.extend_from_slice(&5u16.to_be_bytes());
        assert_eq!(v.len(), 53);
        let s = Stat::parse(&v).unwrap();
        assert_eq!(s.event(), StatEvent::Timer);
        assert_eq!(s.buffer_size, 3_000_000);
        assert_eq!(s.fullness, 123_456);
        assert_eq!(s.bytes_received, (1u64 << 32) + 7);
        assert_eq!(s.jiffies, 99_999);
        assert_eq!(s.elapsed_seconds, 42);
        assert_eq!(s.elapsed_milliseconds, 42_500);
        assert_eq!(s.server_timestamp, 777);
        assert_eq!(s.error_code, 5);
        // 57-byte form (4 junk bytes appended) still parses error_code
        v.extend_from_slice(&[9, 9, 9, 9]);
        assert_eq!(Stat::parse(&v).unwrap().error_code, 5);
        // odd length => error code ignored
        v.pop();
        assert_eq!(Stat::parse(&v).unwrap().error_code, 0);
    }

    #[test]
    fn parse_misc() {
        let m = ClientMessage::parse(*b"DSCO", Bytes::from_static(&[2])).unwrap();
        assert_eq!(m, ClientMessage::Dsco(DisconnectReason::RemoteReset));
        let m = ClientMessage::parse(*b"SETD", Bytes::from_static(b"\0Kitchen\0")).unwrap();
        assert_eq!(
            m,
            ClientMessage::Setd {
                id: 0,
                value: Bytes::from_static(b"Kitchen\0")
            }
        );
        let mut ir = 5u32.to_be_bytes().to_vec();
        ir.extend_from_slice(&[0, 0]);
        ir.extend_from_slice(&0x7689c03fu32.to_be_bytes());
        let m = ClientMessage::parse(*b"IR  ", Bytes::from(ir)).unwrap();
        assert_eq!(
            m,
            ClientMessage::Ir {
                time: 5,
                code: 0x7689c03f
            }
        );
        let m = ClientMessage::parse(*b"BYE!", Bytes::from_static(&[1])).unwrap();
        assert_eq!(m, ClientMessage::Bye { upgrading: true });
        assert!(matches!(
            ClientMessage::parse(*b"XXXX", Bytes::new()).unwrap(),
            ClientMessage::Unknown { .. }
        ));
    }
}
