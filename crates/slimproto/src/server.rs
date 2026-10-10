//! Frames sent from the server to the player.

use bytes::{BufMut, Bytes, BytesMut};

/// Decoder/format byte for the `strm` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// `p` raw PCM / WAV / AIFF
    Pcm,
    /// `m` MPEG audio
    Mp3,
    /// `f` FLAC (also OggFLAC with sample-size byte `o`)
    Flac,
    /// `w` WMA
    Wma,
    /// `o` Ogg Vorbis
    Ogg,
    /// `u` Opus
    Opus,
    /// `l` Apple Lossless
    Alac,
    /// `a` AAC (ADTS or MP4 container, selected via sample-size byte)
    Aac,
    /// `d` DSD (DSF/DFF)
    Dsd,
    /// `s` SqueezePlay native protocol handler
    SqueezePlayDirect,
    /// `n` test tone
    Test,
}

impl Format {
    pub fn byte(self) -> u8 {
        match self {
            Format::Pcm => b'p',
            Format::Mp3 => b'm',
            Format::Flac => b'f',
            Format::Wma => b'w',
            Format::Ogg => b'o',
            Format::Opus => b'u',
            Format::Alac => b'l',
            Format::Aac => b'a',
            Format::Dsd => b'd',
            Format::SqueezePlayDirect => b's',
            Format::Test => b'n',
        }
    }

    /// Map an LMS content-type code (`flc`, `mp3`, `pcm`, `aif`, `wav`, `ogg`, `ops`, `alc`, `aac`,
    /// `mp4`, `wma`, `dsf`, `dff`, `ogf`) to the decoder byte.  Unknown types fall back to MP3,
    /// exactly like the reference server.
    pub fn from_content_type(ct: &str) -> Format {
        match ct {
            "pcm" | "wav" | "aif" | "aiff" => Format::Pcm,
            "flc" | "flac" | "ogf" => Format::Flac,
            "wma" | "wmal" | "wmap" | "asx" => Format::Wma,
            "ogg" => Format::Ogg,
            "ops" => Format::Opus,
            "alc" => Format::Alac,
            "aac" | "mp4" => Format::Aac,
            "dsf" | "dff" => Format::Dsd,
            "test" => Format::Test,
            _ => Format::Mp3,
        }
    }
}

/// PCM sample size byte: `'0'`=8, `'1'`=16, `'2'`=24, `'3'`=32 bits; `'?'` when not applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSampleSize(pub u8);

impl PcmSampleSize {
    pub const UNKNOWN: PcmSampleSize = PcmSampleSize(b'?');
    pub fn from_bits(bits: u32) -> PcmSampleSize {
        PcmSampleSize(match bits {
            8 => b'0',
            16 => b'1',
            24 => b'2',
            32 => b'3',
            _ => b'1',
        })
    }
}

/// PCM sample rate byte.  Unknown rates default to `'3'` (44100) like the reference server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSampleRate(pub u8);

impl PcmSampleRate {
    pub const UNKNOWN: PcmSampleRate = PcmSampleRate(b'?');
    pub fn from_hz(hz: u32) -> PcmSampleRate {
        PcmSampleRate(match hz {
            11025 => b'0',
            22050 => b'1',
            32000 => b'2',
            44100 => b'3',
            48000 => b'4',
            8000 => b'5',
            12000 => b'6',
            16000 => b'7',
            24000 => b'8',
            96000 => b'9',
            88200 => b':',
            176400 => b';',
            192000 => b'<',
            352800 => b'=',
            384000 => b'>',
            _ => b'3',
        })
    }
}

/// PCM endianness byte: `'0'` big endian, `'1'` little endian, `'?'` n/a.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmEndian {
    Big,
    Little,
    Unknown,
}

impl PcmEndian {
    fn byte(self) -> u8 {
        match self {
            PcmEndian::Big => b'0',
            PcmEndian::Little => b'1',
            PcmEndian::Unknown => b'?',
        }
    }
}

/// Transition type byte for `strm s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransitionType {
    None = b'0',
    Crossfade = b'1',
    FadeIn = b'2',
    FadeOut = b'3',
    FadeInOut = b'4',
    /// `5`: crossfade immediately (used for user-initiated skips).
    CrossfadeImmediate = b'5',
}

impl TransitionType {
    pub fn from_pref(v: i64) -> TransitionType {
        match v {
            1 => TransitionType::Crossfade,
            2 => TransitionType::FadeIn,
            3 => TransitionType::FadeOut,
            4 => TransitionType::FadeInOut,
            5 => TransitionType::CrossfadeImmediate,
            _ => TransitionType::None,
        }
    }
}

/// The `strm` command byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrmCommand {
    /// `s` start a new stream (header fields and HTTP request follow).
    Start,
    /// `p` pause; the replay-gain field carries "pause for N ms" (0 = indefinite).
    Pause,
    /// `u` unpause; the replay-gain field carries the jiffies value at which to resume (0 = now).
    Unpause,
    /// `q` stop and flush everything.
    Stop,
    /// `t` status request — player answers with STAT STMt echoing the timestamp.
    Status,
    /// `f` flush the stream/decode buffers (used before a new stream when already playing).
    Flush,
    /// `a` skip ahead N ms (replay-gain field).
    SkipAhead,
    /// `i` — unused by current firmware (was "i2c").
    Reserved,
}

impl StrmCommand {
    fn byte(self) -> u8 {
        match self {
            StrmCommand::Start => b's',
            StrmCommand::Pause => b'p',
            StrmCommand::Unpause => b'u',
            StrmCommand::Stop => b'q',
            StrmCommand::Status => b't',
            StrmCommand::Flush => b'f',
            StrmCommand::SkipAhead => b'a',
            StrmCommand::Reserved => b'i',
        }
    }
}

/// The 24-byte fixed header of a `strm` frame.  Matches `pack 'aaaaaaaCCCaCCCNnN'`:
///
/// ```text
/// 0  a  command
/// 1  a  autostart ('0' off, '1' on, '2' direct off, '3' direct on)
/// 2  a  format
/// 3  a  pcm sample size
/// 4  a  pcm sample rate
/// 5  a  pcm channels ('1' or '2')
/// 6  a  pcm endian
/// 7  C  buffer threshold (KB of input buffer before autostart/STMl)
/// 8  C  s/pdif auto (0)
/// 9  C  transition duration (seconds)
/// 10 a  transition type
/// 11 C  flags: 0x80 loop, 0x40 reconnect-on-fail, 0x0c output channels, 0x03 polarity inversion
/// 12 C  output threshold (tenths of a second of output buffer before start)
/// 13 C  slave streams (reserved)
/// 14 N  replay gain 16.16 fixed; or ms interval for p/a; jiffies for u
/// 18 n  server port
/// 20 N  server ip (0 = same host as the control connection)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrmHeader {
    pub command: StrmCommand,
    pub autostart: u8,
    pub format: Format,
    pub pcm_sample_size: u8,
    pub pcm_sample_rate: u8,
    pub pcm_channels: u8,
    pub pcm_endian: u8,
    pub buffer_threshold: u8,
    pub spdif_auto: u8,
    pub transition_duration: u8,
    pub transition_type: TransitionType,
    pub flags: u8,
    pub output_threshold: u8,
    pub slave_streams: u8,
    pub replay_gain: u32,
    pub server_port: u16,
    pub server_ip: u32,
}

impl StrmHeader {
    /// Header for every command except `s`: all fields zero/unknown apart from command,
    /// `replay_gain` (interval) and flags.
    pub fn simple(command: StrmCommand, interval: u32, flags: u8) -> StrmHeader {
        StrmHeader {
            command,
            autostart: b'0',
            format: Format::Mp3,
            pcm_sample_size: b'?',
            pcm_sample_rate: b'?',
            pcm_channels: b'?',
            pcm_endian: b'?',
            buffer_threshold: 0,
            spdif_auto: 0,
            transition_duration: 0,
            transition_type: TransitionType::None,
            flags,
            output_threshold: 0,
            slave_streams: 0,
            replay_gain: interval,
            server_port: 0,
            server_ip: 0,
        }
    }

    /// Start header for a given format with the reference server's per-format defaults
    /// (`Slim::Player::Squeezebox::stream_s`).  Callers adjust the PCM bytes and thresholds.
    pub fn start(format: Format, autostart: bool) -> StrmHeader {
        let (size, rate, chans, endian, out_thr): (u8, u8, u8, u8, u8) = match format {
            Format::Pcm => (b'1', b'3', b'2', b'1', 0),
            Format::Flac => (b'?', b'?', b'?', b'?', 0),
            Format::Wma => (b'0', b'1', b'?', b'?', 10),
            Format::Ogg => (b'?', b'?', b'?', b'?', 20),
            Format::Opus => (b'?', b'?', b'?', b'?', 20),
            Format::Alac => (b'?', b'?', b'?', b'?', 0),
            Format::Aac => (b'2', b'?', b'?', b'?', 0),
            Format::Dsd => (b'0', b'?', b'?', b'?', 0),
            Format::SqueezePlayDirect => (b'?', b'?', b'?', b'?', 1),
            Format::Test => (b'?', b'?', b'?', b'?', 0),
            Format::Mp3 => (b'?', b'?', b'?', b'?', 1),
        };
        StrmHeader {
            command: StrmCommand::Start,
            autostart: if autostart { b'1' } else { b'0' },
            format,
            pcm_sample_size: size,
            pcm_sample_rate: rate,
            pcm_channels: chans,
            pcm_endian: endian,
            buffer_threshold: 255,
            spdif_auto: 0,
            transition_duration: 0,
            transition_type: TransitionType::None,
            flags: 0,
            output_threshold: out_thr,
            slave_streams: 0,
            replay_gain: 0,
            server_port: 9000,
            server_ip: 0,
        }
    }

    pub fn with_pcm(mut self, bits: u32, hz: u32, channels: u32, little_endian: bool) -> Self {
        self.pcm_sample_size = PcmSampleSize::from_bits(bits).0;
        self.pcm_sample_rate = PcmSampleRate::from_hz(hz).0;
        self.pcm_channels = if channels == 1 { b'1' } else { b'2' };
        self.pcm_endian = if little_endian { PcmEndian::Little.byte() } else { PcmEndian::Big.byte() };
        self
    }

    pub fn encode(&self, out: &mut BytesMut) {
        out.put_u8(self.command.byte());
        out.put_u8(self.autostart);
        out.put_u8(self.format.byte());
        out.put_u8(self.pcm_sample_size);
        out.put_u8(self.pcm_sample_rate);
        out.put_u8(self.pcm_channels);
        out.put_u8(self.pcm_endian);
        out.put_u8(self.buffer_threshold);
        out.put_u8(self.spdif_auto);
        out.put_u8(self.transition_duration);
        out.put_u8(self.transition_type as u8);
        out.put_u8(self.flags);
        out.put_u8(self.output_threshold);
        out.put_u8(self.slave_streams);
        out.put_u32(self.replay_gain);
        out.put_u16(self.server_port);
        out.put_u32(self.server_ip);
    }
}

/// Payload of an `audg` (audio gain) frame.
///
/// `pack('NNCCNN[N]', old_left, old_right, dvc, preamp, new_left, new_right[, sequence])`
/// where old gains are the legacy 1.7 fixed-point values (0..128) and new gains are 16.16
/// fixed point.  `dvc` is the digital-volume-control flag, `preamp` is `255 - 2*preampdB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioGain {
    pub old_left: u32,
    pub old_right: u32,
    pub dvc: bool,
    pub preamp: u8,
    pub new_left: u32,
    pub new_right: u32,
    pub sequence: Option<u32>,
}

impl AudioGain {
    /// Legacy 129-step volume map from the reference server (indexed by 0..=100).
    pub const VOLUME_MAP: [u8; 101] = [
        0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5, 5, 6, 6, 7, 8, 9, 9, 10, 11, 12, 13, 14, 15, 16, 16, 17, 18, 19,
        20, 22, 23, 24, 25, 26, 27, 28, 29, 30, 32, 33, 34, 35, 37, 38, 39, 40, 42, 43, 44, 46, 47, 48,
        50, 51, 53, 54, 56, 57, 59, 60, 61, 63, 65, 66, 68, 69, 71, 72, 74, 75, 77, 79, 80, 82, 84, 85,
        87, 89, 90, 92, 94, 96, 97, 99, 101, 103, 104, 106, 108, 110, 112, 113, 115, 117, 119, 121, 123,
        125, 127, 128,
    ];

    /// Convert a dB attenuation to the 16.16 fixed-point multiplier used by firmware ≥22.
    pub fn db_to_fixed(db: f64) -> u32 {
        let mult = 10f64.powf(db / 20.0);
        if (-30.0..=0.0).contains(&db) {
            ((mult * 256.0 + 0.5).floor() as u32) * 256
        } else {
            (mult * 65536.0 + 0.5).floor() as u32
        }
    }

    /// Volume (0..=100) → dB using the Squeezebox2 curve: linear from -50 dB at 0 to 0 dB at 100.
    pub fn volume_to_db(volume: u32) -> f64 {
        let total_range = -50.0f64;
        let v = volume.min(100) as f64;
        // slope_high = (0 - total_range*1)/(100 - (-1)); applied from x1=100,y1=0
        let slope = (0.0 - total_range) / 101.0;
        slope * (v - 100.0)
    }

    /// Build the gain frame for a UI volume 0..=100 (negative/zero = mute) with balance -25..=25,
    /// mirroring `Slim::Player::Squeezebox2::volume`.
    pub fn for_volume(volume: i32, dvc: bool, preamp_db: f64, balance: i32, sequence: Option<u32>) -> AudioGain {
        let old_gain = if volume > 0 { Self::VOLUME_MAP[volume.min(100) as usize] as f64 } else { 0.0 };
        let new_gain = if volume <= 0 { 0.0 } else { Self::db_to_fixed(Self::volume_to_db(volume as u32)) as f64 };
        let left = if balance > 0 { (25 - balance) as f64 / 25.0 } else { 1.0 };
        let right = if balance < 0 { (25 + balance) as f64 / 25.0 } else { 1.0 };
        let preamp = (255i64 - (2.0 * preamp_db) as i64).clamp(0, 255) as u8;
        AudioGain {
            old_left: (old_gain * left) as u32,
            old_right: (old_gain * right) as u32,
            dvc,
            preamp,
            new_left: (new_gain * left) as u32,
            new_right: (new_gain * right) as u32,
            sequence,
        }
    }

    pub fn encode(&self, out: &mut BytesMut) {
        out.put_u32(self.old_left);
        out.put_u32(self.old_right);
        out.put_u8(self.dvc as u8);
        out.put_u8(self.preamp);
        out.put_u32(self.new_left);
        out.put_u32(self.new_right);
        if let Some(seq) = self.sequence {
            out.put_u32(seq);
        }
    }
}

/// Firmware setting ids used with `setd`.
pub mod setd_id {
    pub const PLAYER_NAME: u8 = 0;
    pub const DIGITAL_OUTPUT_ENCODING: u8 = 1;
    pub const WORD_CLOCK_OUTPUT: u8 = 2;
    pub const POWER_OFF_DAC: u8 = 3;
    pub const DISABLE_DAC: u8 = 4;
    pub const FXLOOP_SOURCE: u8 = 5;
    pub const FXLOOP_CLOCK: u8 = 6;
}

/// A frame to send to the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFrame {
    /// `strm` with its 24-byte header and (for `s`) the HTTP request text / URL.
    Strm { header: StrmHeader, request: Bytes },
    /// `audg`
    Audg(AudioGain),
    /// `aude` — enable/disable S/PDIF and DAC outputs.
    Aude { spdif: bool, dac: bool },
    /// `setd` query (`value == None`) or set.
    Setd { id: u8, value: Option<Bytes> },
    /// `vers` — server version string.
    Vers(String),
    /// `serv` — tell the player to connect to another server (`ip`, optional sync group id as 10 ASCII digits).
    Serv { ip: u32, sync_group_id: Option<u64> },
    /// `cont` — direct stream continue: ICY metaint, loop flag, WMA GUID list.
    Cont { metaint: u32, loop_: bool, guids: Vec<u8> },
    /// `dsco` — tell the player to disconnect (sent on unknown device id).
    Dsco,
    /// `ureq` — ask the player to perform a firmware update.
    Ureq,
    /// `body` — body length announcement for direct stream playlist fetches.
    Body { length: u32 },
    /// `grfe` — graphics frame for the display (raw bitmap, Squeezebox2 and later).
    Grfe { offset: u16, transition: u8, param: u8, bitmap: Bytes },
    /// `grfb` — display brightness.
    Grfb(u16),
    /// `visu` — visualiser parameters (raw).
    Visu(Bytes),
    /// `ledc` — Boom LED control.
    Ledc(u32),
    /// `rtcs` — Boom RTC set.
    Rtcs(Bytes),
    /// `audp` — audio pitch (Transporter).
    Audp(u8),
    /// `audo` — audio output / polarity settings.
    Audo(Bytes),
    /// `bdac` — Transporter DAC settings.
    Bdac(Bytes),
    /// `i2cc` — raw i2c (SB1 only).
    I2cc(Bytes),
    /// `test` — test mode.
    Test(Bytes),
    /// `vfdc` — text display frame (SB1 / squeezeslave).
    Vfdc(Bytes),
    /// `http` — SB1: raw HTTP request to the player.
    Http(Bytes),
    /// Any other op with an opaque payload.
    Raw { op: [u8; 4], payload: Bytes },
}

impl ServerFrame {
    pub fn strm(header: StrmHeader) -> ServerFrame {
        ServerFrame::Strm { header, request: Bytes::new() }
    }

    pub fn op(&self) -> [u8; 4] {
        match self {
            ServerFrame::Strm { .. } => *b"strm",
            ServerFrame::Audg(_) => *b"audg",
            ServerFrame::Aude { .. } => *b"aude",
            ServerFrame::Setd { .. } => *b"setd",
            ServerFrame::Vers(_) => *b"vers",
            ServerFrame::Serv { .. } => *b"serv",
            ServerFrame::Cont { .. } => *b"cont",
            ServerFrame::Dsco => *b"dsco",
            ServerFrame::Ureq => *b"ureq",
            ServerFrame::Body { .. } => *b"body",
            ServerFrame::Grfe { .. } => *b"grfe",
            ServerFrame::Grfb(_) => *b"grfb",
            ServerFrame::Visu(_) => *b"visu",
            ServerFrame::Ledc(_) => *b"ledc",
            ServerFrame::Rtcs(_) => *b"rtcs",
            ServerFrame::Audp(_) => *b"audp",
            ServerFrame::Audo(_) => *b"audo",
            ServerFrame::Bdac(_) => *b"bdac",
            ServerFrame::I2cc(_) => *b"i2cc",
            ServerFrame::Test(_) => *b"test",
            ServerFrame::Vfdc(_) => *b"vfdc",
            ServerFrame::Http(_) => *b"http",
            ServerFrame::Raw { op, .. } => *op,
        }
    }

    fn encode_payload(&self, out: &mut BytesMut) {
        match self {
            ServerFrame::Strm { header, request } => {
                header.encode(out);
                out.put_slice(request);
            }
            ServerFrame::Audg(g) => g.encode(out),
            ServerFrame::Aude { spdif, dac } => {
                out.put_u8(*spdif as u8);
                out.put_u8(*dac as u8);
            }
            ServerFrame::Setd { id, value } => {
                out.put_u8(*id);
                if let Some(v) = value {
                    out.put_slice(v);
                }
            }
            ServerFrame::Vers(v) => out.put_slice(v.as_bytes()),
            ServerFrame::Serv { ip, sync_group_id } => {
                out.put_u32(*ip);
                if let Some(id) = sync_group_id {
                    out.put_slice(format!("{:010}", id).as_bytes());
                }
            }
            ServerFrame::Cont { metaint, loop_, guids } => {
                out.put_u32(*metaint);
                out.put_u8(*loop_ as u8);
                out.put_u16(guids.len() as u16);
                out.put_slice(guids);
            }
            ServerFrame::Dsco | ServerFrame::Ureq => {}
            ServerFrame::Body { length } => out.put_u32(*length),
            ServerFrame::Grfe { offset, transition, param, bitmap } => {
                out.put_u16(*offset);
                out.put_u8(*transition);
                out.put_u8(*param);
                out.put_slice(bitmap);
            }
            ServerFrame::Grfb(b) => out.put_u16(*b),
            ServerFrame::Ledc(v) => out.put_u32(*v),
            ServerFrame::Audp(p) => out.put_u8(*p),
            ServerFrame::Visu(b)
            | ServerFrame::Rtcs(b)
            | ServerFrame::Audo(b)
            | ServerFrame::Bdac(b)
            | ServerFrame::I2cc(b)
            | ServerFrame::Test(b)
            | ServerFrame::Vfdc(b)
            | ServerFrame::Http(b)
            | ServerFrame::Raw { payload: b, .. } => out.put_slice(b),
        }
    }

    /// Encode a complete wire frame: `u16 BE (len+4) | op | payload`.
    pub fn encode(&self, out: &mut BytesMut) {
        let mut payload = BytesMut::with_capacity(64);
        self.encode_payload(&mut payload);
        let len = payload.len() + 4;
        assert!(len <= u16::MAX as usize, "server frame too large");
        out.reserve(2 + len);
        out.put_u16(len as u16);
        out.put_slice(&self.op());
        out.put_slice(&payload);
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut b = BytesMut::new();
        self.encode(&mut b);
        b.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strm_stop_frame_layout() {
        let f = ServerFrame::strm(StrmHeader::simple(StrmCommand::Stop, 0, 0));
        let b = f.to_bytes();
        assert_eq!(b.len(), 2 + 4 + 24);
        assert_eq!(&b[0..2], &[0, 28]);
        assert_eq!(&b[2..6], b"strm");
        assert_eq!(&b[6..13], b"q0m????");
        assert_eq!(b[16], b'0'); // transition type ascii '0'
        assert!(b[13..30].iter().enumerate().all(|(i, &x)| i == 3 || x == 0));
    }

    #[test]
    fn strm_start_frame_layout() {
        let mut h = StrmHeader::start(Format::Flac, true);
        h.transition_type = TransitionType::Crossfade;
        h.transition_duration = 3;
        h.replay_gain = 0x0001_0000;
        h.server_port = 9000;
        let req = Bytes::from_static(b"GET /stream.mp3?player=00:04:20:aa:bb:cc HTTP/1.0\r\n\r\n");
        let f = ServerFrame::Strm { header: h, request: req.clone() };
        let b = f.to_bytes();
        assert_eq!(u16::from_be_bytes([b[0], b[1]]) as usize, 4 + 24 + req.len());
        let p = &b[6..];
        assert_eq!(&p[0..7], b"s1f????");
        assert_eq!(p[7], 255); // buffer threshold
        assert_eq!(p[9], 3); // transition duration
        assert_eq!(p[10], b'1'); // crossfade
        assert_eq!(&p[14..18], &0x0001_0000u32.to_be_bytes());
        assert_eq!(&p[18..20], &9000u16.to_be_bytes());
        assert_eq!(&p[20..24], &[0, 0, 0, 0]);
        assert_eq!(&p[24..], &req[..]);
    }

    #[test]
    fn pcm_header() {
        let h = StrmHeader::start(Format::Pcm, true).with_pcm(24, 96000, 2, true);
        assert_eq!(
            [h.pcm_sample_size, h.pcm_sample_rate, h.pcm_channels, h.pcm_endian],
            [b'2', b'9', b'2', b'1']
        );
    }

    #[test]
    fn gain_math() {
        // 0 dB -> 1.0 in 16.16 == 65536
        assert_eq!(AudioGain::db_to_fixed(0.0), 65536);
        assert_eq!(AudioGain::volume_to_db(100), 0.0);
        assert!((AudioGain::volume_to_db(0) + 5000.0 / 101.0).abs() < 1e-9);
        let g = AudioGain::for_volume(100, true, 0.0, 0, Some(7));
        assert_eq!(g.old_left, 128);
        assert_eq!(g.new_left, 65536);
        assert_eq!(g.preamp, 255);
        let mut b = BytesMut::new();
        g.encode(&mut b);
        assert_eq!(b.len(), 22);
        let m = AudioGain::for_volume(0, false, 0.0, 0, None);
        assert_eq!((m.old_left, m.new_left), (0, 0));
        let bal = AudioGain::for_volume(100, true, 0.0, 25, None);
        assert_eq!((bal.new_left, bal.new_right), (0, 65536));
    }

    #[test]
    fn misc_frames() {
        assert_eq!(&ServerFrame::Aude { spdif: true, dac: true }.to_bytes()[..], &[0, 6, b'a', b'u', b'd', b'e', 1, 1]);
        assert_eq!(&ServerFrame::Setd { id: 0, value: None }.to_bytes()[..], &[0, 5, b's', b'e', b't', b'd', 0]);
        let v = ServerFrame::Vers("9.2.0".into()).to_bytes();
        assert_eq!(&v[..], b"\x00\x09vers9.2.0");
        let s = ServerFrame::Serv { ip: 0xc0a80001, sync_group_id: Some(42) }.to_bytes();
        assert_eq!(&s[6..10], &[0xc0, 0xa8, 0, 1]);
        assert_eq!(&s[10..], b"0000000042");
        let c = ServerFrame::Cont { metaint: 16000, loop_: false, guids: vec![] }.to_bytes();
        assert_eq!(&c[6..], &[0, 0, 0x3e, 0x80, 0, 0, 0]);
        assert_eq!(&ServerFrame::Dsco.to_bytes()[..], &[0, 4, b'd', b's', b'c', b'o']);
    }
}
