//! The pieces of RAOP (AirPlay 1) on the wire: the RTSP client that sets a session up,
//! audio packets (RTP carrying ALAC frames), the timing and sync packets of the control
//! channel, and the optional AES encryption of the audio with its RSA-wrapped key.

use anyhow::{anyhow, bail, Context};
use base64::Engine;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Stereo frames in one audio packet (the `352` of the SDP's `fmtp`).
pub const FRAMES_PER_PACKET: usize = 352;
pub const SAMPLE_RATE: u32 = 44_100;
/// RTP payload type of the audio.
const PT_AUDIO: u8 = 0x60;

/// What senders call themselves; some receivers only talk to iTunes.
const USER_AGENT: &str = "iTunes/7.6.2 (Windows; N;)";
const RTSP_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// ALAC
// ---------------------------------------------------------------------------------------------

/// Writes bits most significant first, as ALAC reads them.
struct Bits {
    out: Vec<u8>,
    acc: u8,
    n: u32,
}

impl Bits {
    fn with_capacity(bytes: usize) -> Bits {
        Bits {
            out: Vec::with_capacity(bytes),
            acc: 0,
            n: 0,
        }
    }

    fn put(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc << (8 - self.n));
        }
        self.out
    }
}

/// One ALAC frame of 16-bit stereo `samples` (interleaved, left first) in the
/// "uncompressed" form: the element header with the escape flag set, then each frame's
/// samples verbatim, then the end tag.  Receivers decode it like any ALAC frame; it costs
/// no CPU to make and only a few bytes more than the raw samples.
pub fn alac_frame(samples: &[i16]) -> Vec<u8> {
    let mut b = Bits::with_capacity(samples.len() * 2 + 8);
    b.put(1, 3); // element: a channel pair
    b.put(0, 4); // element instance tag
    b.put(0, 12); // unused
    b.put(0, 1); // no sample count: a whole frame of FRAMES_PER_PACKET
    b.put(0, 2); // no extra bytes
    b.put(1, 1); // not compressed
    for &s in samples {
        b.put(s as u16 as u32, 16);
    }
    b.put(7, 3); // end of frame
    b.finish()
}

// ---------------------------------------------------------------------------------------------
// RTP and the control channel
// ---------------------------------------------------------------------------------------------

/// The 12-byte RTP header of an audio packet; `first` marks the first packet of a stream
/// (after RECORD or a flush).
pub fn rtp_header(first: bool, seq: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = 0x80; // version 2
    h[1] = if first { 0x80 | PT_AUDIO } else { PT_AUDIO };
    h[2..4].copy_from_slice(&seq.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// A sync packet: frame `playing` is heard at NTP time `ntp`, and the speaker should hold
/// `latency` frames (`playing + latency` is the newest frame it should have).  `first`
/// after a start or a flush.
pub fn sync_packet(first: bool, playing: u32, ntp: u64, latency: u32) -> [u8; 20] {
    let mut p = [0u8; 20];
    p[0] = if first { 0x90 } else { 0x80 };
    p[1] = 0xd4;
    p[2..4].copy_from_slice(&7u16.to_be_bytes());
    p[4..8].copy_from_slice(&playing.to_be_bytes());
    p[8..16].copy_from_slice(&ntp.to_be_bytes());
    p[16..20].copy_from_slice(&playing.wrapping_add(latency).to_be_bytes());
    p
}

/// The answer to a timing request (`0x52`): its transmit time comes back as the origin,
/// with when it was received and when the answer leaves.
pub fn timing_reply(req: &[u8], now: u64) -> Option<[u8; 32]> {
    if req.len() < 32 || req[1] & 0x7f != 0x52 {
        return None;
    }
    let mut r = [0u8; 32];
    r[0] = 0x80;
    r[1] = 0xd3;
    r[2..4].copy_from_slice(&req[2..4]);
    r[8..16].copy_from_slice(&req[24..32]);
    r[16..24].copy_from_slice(&now.to_be_bytes());
    r[24..32].copy_from_slice(&now.to_be_bytes());
    Some(r)
}

/// A resend request (`0x55`): the first missing sequence number and how many.
pub fn resend_request(p: &[u8]) -> Option<(u16, u16)> {
    if p.len() < 8 || p[1] & 0x7f != 0x55 {
        return None;
    }
    Some((
        u16::from_be_bytes([p[4], p[5]]),
        u16::from_be_bytes([p[6], p[7]]),
    ))
}

/// A packet sent again, as an answer to a resend request.
pub fn resent(seq: u16, packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + packet.len());
    out.extend_from_slice(&[0x80, 0xd6]);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(packet);
    out
}

/// The wall clock as NTP (seconds since 1900 in the high half, the fraction in the low).
pub fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() + 2_208_988_800;
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

// ---------------------------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------------------------

/// The public key of the AirPort Express, which receivers asking for RSA (`et=1`) hold the
/// private half of: the session's AES key is sent wrapped with it.
const AIRPORT_MODULUS: &str = "59dE8qLieItsH1WgjrcFRKj6eUWqi+bGLOX1HL3U3GhC/j0Qg90u3sG/1CUtwC5vOYvfDmFI6oSFXi5ELabWJmT2dKHzBJKa3k9ok+8t9ucRqMd6DZHJ2YCCLlDRKSKv6kDqnw4UwPdpOMXziC/AMj3Z/lUVX1G7WSHCAWKf1zNS1eLvqr+boEjXuBOitnZ/bDzPHrTOZz0Dew0uowxf/+sG+NCK3eQJVxqcaJ/vEHKIVd2M+5qL71yJQ+87X6oV3eaYvt3zWZYD6z5vYTcrtij2VZ9Zmni/UAaHqn9JdsBWLUEpVviYnhimNVvYFZeCXg/IdTQ+x4IRdiXNv5hEew==";

/// AES-128-CBC over each packet's audio, restarting from the IV for every packet; the
/// tail that doesn't fill a 16-byte block stays in the clear.
pub struct Cipher {
    key: [u8; 16],
    iv: [u8; 16],
}

impl Cipher {
    pub fn new() -> Cipher {
        Cipher {
            key: rand::random(),
            iv: rand::random(),
        }
    }

    pub fn encrypt(&self, payload: &mut [u8]) {
        use aes::cipher::{generic_array::GenericArray, BlockEncryptMut, KeyIvInit};
        let mut enc = cbc::Encryptor::<aes::Aes128>::new(&self.key.into(), &self.iv.into());
        let whole = payload.len() / 16 * 16;
        for block in payload[..whole].chunks_exact_mut(16) {
            enc.encrypt_block_mut(GenericArray::from_mut_slice(block));
        }
    }

    /// The SDP lines that give the receiver the key (RSA-OAEP with SHA-1) and the IV.
    pub fn sdp(&self) -> anyhow::Result<String> {
        use rsa::{BigUint, Oaep, RsaPublicKey};
        let b64 = base64::engine::general_purpose::STANDARD;
        let n = BigUint::from_bytes_be(&b64.decode(AIRPORT_MODULUS)?);
        let key = RsaPublicKey::new(n, BigUint::from(65_537u32))?;
        let wrapped = key.encrypt(
            &mut rsa::rand_core::OsRng,
            Oaep::new::<sha1::Sha1>(),
            &self.key,
        )?;
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
        Ok(format!(
            "a=rsaaeskey:{}\r\na=aesiv:{}\r\n",
            b64.encode(wrapped),
            b64.encode(self.iv)
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// RTSP
// ---------------------------------------------------------------------------------------------

pub struct Response {
    pub status: u16,
    pub reason: String,
    headers: Vec<(String, String)>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        self.status == 200
    }
}

/// An RTSP connection to a receiver.
pub struct Rtsp {
    stream: TcpStream,
    buf: Vec<u8>,
    cseq: u32,
    /// The session's URL (`rtsp://<our address>/<id>`).
    pub url: String,
    client_instance: String,
    active_remote: u32,
    /// The receiver's `Session` once SETUP gave one.
    pub session: Option<String>,
}

impl Rtsp {
    pub async fn connect(addr: SocketAddr) -> anyhow::Result<Rtsp> {
        let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| anyhow!("timed out connecting to {addr}"))?
            .with_context(|| format!("connecting to {addr}"))?;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?.ip();
        let url = format!("rtsp://{}/{}", host_of(local), rand::random::<u32>());
        Ok(Rtsp {
            stream,
            buf: Vec::new(),
            cseq: 0,
            url,
            client_instance: format!("{:016X}", rand::random::<u64>()),
            active_remote: rand::random(),
            session: None,
        })
    }

    pub fn local_ip(&self) -> std::io::Result<IpAddr> {
        Ok(self.stream.local_addr()?.ip())
    }

    pub fn peer_ip(&self) -> std::io::Result<IpAddr> {
        Ok(self.stream.peer_addr()?.ip())
    }

    /// Send a request (to the session's URL, or `*` for OPTIONS) and read the response.
    /// A response other than 200 is not an error here; a broken connection is.
    pub async fn request(
        &mut self,
        method: &str,
        headers: &[(&str, String)],
        body: Option<(&str, &[u8])>,
    ) -> anyhow::Result<Response> {
        tokio::time::timeout(RTSP_TIMEOUT, self.exchange(method, headers, body))
            .await
            .map_err(|_| anyhow!("{method}: no answer"))?
    }

    async fn exchange(
        &mut self,
        method: &str,
        headers: &[(&str, String)],
        body: Option<(&str, &[u8])>,
    ) -> anyhow::Result<Response> {
        self.cseq += 1;
        let url = if method == "OPTIONS" { "*" } else { &self.url };
        let mut req = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: {USER_AGENT}\r\n\
             Client-Instance: {ci}\r\nDACP-ID: {ci}\r\nActive-Remote: {}\r\n",
            self.cseq,
            self.active_remote,
            ci = self.client_instance,
        );
        if let Some(s) = &self.session {
            req.push_str(&format!("Session: {s}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        let mut bytes = match body {
            Some((ctype, b)) => {
                req.push_str(&format!(
                    "Content-Type: {ctype}\r\nContent-Length: {}\r\n\r\n",
                    b.len()
                ));
                let mut bytes = req.into_bytes();
                bytes.extend_from_slice(b);
                bytes
            }
            None => {
                req.push_str("\r\n");
                req.into_bytes()
            }
        };
        self.stream.write_all(&bytes).await?;
        bytes.clear();
        self.read_response().await
    }

    async fn read_response(&mut self) -> anyhow::Result<Response> {
        let end = loop {
            if let Some(i) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            if self.stream.read_buf(&mut self.buf).await? == 0 {
                bail!("the receiver closed the connection");
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..end]).to_string();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let mut parts = status_line.splitn(3, ' ');
        if !parts.next().unwrap_or_default().starts_with("RTSP/") {
            bail!("not an RTSP response: {status_line}");
        }
        let status = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let reason = parts.next().unwrap_or_default().to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        while self.buf.len() < end + 4 + len {
            if self.stream.read_buf(&mut self.buf).await? == 0 {
                bail!("the receiver closed the connection");
            }
        }
        self.buf.drain(..end + 4 + len);
        Ok(Response {
            status,
            reason,
            headers,
        })
    }

    /// Wait until the receiver closes the connection (between requests).
    pub async fn closed(&mut self) {
        loop {
            match self.stream.read_buf(&mut self.buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }

    /// Write a TEARDOWN without waiting for anything: for when the session is dropped.
    pub fn teardown_now(&mut self) {
        self.cseq += 1;
        let mut req = format!("TEARDOWN {} RTSP/1.0\r\nCSeq: {}\r\n", self.url, self.cseq);
        if let Some(s) = &self.session {
            req.push_str(&format!("Session: {s}\r\n"));
        }
        req.push_str("\r\n");
        let _ = self.stream.try_write(req.as_bytes());
    }
}

/// An address as it goes in a URL.
pub fn host_of(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    }
}

/// The SDP of an ANNOUNCE: ALAC, 16-bit stereo 44.1 kHz in frames of 352.
pub fn sdp(rtsp: &Rtsp, cipher: Option<&Cipher>) -> anyhow::Result<String> {
    let local = rtsp.local_ip()?;
    let peer = rtsp.peer_ip()?;
    let fam = |ip: IpAddr| if ip.is_ipv6() { "IP6" } else { "IP4" };
    let session_id = rtsp.url.rsplit('/').next().unwrap_or("1");
    let mut s = format!(
        "v=0\r\no=iTunes {session_id} 0 IN {} {local}\r\ns=iTunes\r\nc=IN {} {peer}\r\nt=0 0\r\n\
         m=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\n\
         a=fmtp:96 {FRAMES_PER_PACKET} 0 16 40 10 14 2 255 0 0 {SAMPLE_RATE}\r\n",
        fam(local),
        fam(peer),
    );
    if let Some(c) = cipher {
        s.push_str(&c.sdp()?);
    }
    Ok(s)
}

/// The ports in SETUP's `Transport` answer: `server_port`, `control_port`, `timing_port`.
pub fn transport_ports(transport: &str) -> Option<(u16, u16, u16)> {
    let port = |name: &str| {
        transport.split(';').find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            (k == name).then(|| v.parse::<u16>().ok()).flatten()
        })
    };
    Some((
        port("server_port")?,
        port("control_port").unwrap_or(0),
        port("timing_port").unwrap_or(0),
    ))
}

/// The receiver's volume for 0–100: -30 dB to 0 dB, and -144 (silence) when muted.
pub fn volume_db(volume: u8, muted: bool) -> f64 {
    if muted || volume == 0 {
        -144.0
    } else {
        -30.0 + 30.0 * f64::from(volume.min(100)) / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads back what [`alac_frame`] writes (frames of 16-bit stereo, verbatim).
    fn decode(frame: &[u8]) -> Vec<i16> {
        let mut pos = 0usize;
        let mut get = |bits: usize| {
            let mut v = 0u32;
            for _ in 0..bits {
                let bit = (frame[pos / 8] >> (7 - pos % 8)) & 1;
                v = (v << 1) | bit as u32;
                pos += 1;
            }
            v
        };
        assert_eq!(get(3), 1, "a channel pair");
        get(4);
        get(12);
        assert_eq!(get(1), 0, "no sample count");
        assert_eq!(get(2), 0);
        assert_eq!(get(1), 1, "uncompressed");
        let n = (frame.len() * 8 - 26) / 16;
        let samples: Vec<i16> = (0..n).map(|_| get(16) as u16 as i16).collect();
        assert_eq!(get(3), 7, "end tag");
        samples
    }

    #[test]
    fn alac_frames_hold_the_samples_verbatim() {
        // the layout, bit by bit: 001 0000 000000000000 0 00 1, then the samples, then 111
        assert_eq!(
            alac_frame(&[0x1234, -1]),
            [0x20, 0x00, 0x02, 0x24, 0x69, 0xff, 0xff, 0xc0]
        );
        let samples: Vec<i16> = (0..FRAMES_PER_PACKET * 2)
            .map(|i| ((i as f64 * 0.05).sin() * 30000.0) as i16)
            .chain([i16::MIN, i16::MAX])
            .collect();
        let frame = alac_frame(&samples);
        // 23 header bits + 16 per sample + 3 end bits, rounded up to bytes
        assert_eq!(frame.len(), (23 + 16 * samples.len() + 3).div_ceil(8));
        assert_eq!(decode(&frame), samples);
    }

    #[test]
    fn packet_headers() {
        let h = rtp_header(true, 0x0102, 0x0304_0506, 0x0708_090a);
        assert_eq!(h, [0x80, 0xe0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(rtp_header(false, 1, 2, 3)[..2], [0x80, 0x60]);

        let s = sync_packet(true, 1000, 0x1122_3344_5566_7788, 88_200);
        assert_eq!(s[..4], [0x90, 0xd4, 0, 7]);
        assert_eq!(u32::from_be_bytes(s[4..8].try_into().unwrap()), 1000);
        assert_eq!(s[8..16], 0x1122_3344_5566_7788u64.to_be_bytes());
        assert_eq!(u32::from_be_bytes(s[16..20].try_into().unwrap()), 89_200);
        assert_eq!(sync_packet(false, 0, 0, 0)[0], 0x80);

        let mut req = [0u8; 32];
        req[..4].copy_from_slice(&[0x80, 0xd2, 0, 7]);
        req[24..32].copy_from_slice(&42u64.to_be_bytes());
        let r = timing_reply(&req, 99).unwrap();
        assert_eq!(r[..4], [0x80, 0xd3, 0, 7]);
        assert_eq!(r[8..16], 42u64.to_be_bytes());
        assert_eq!(r[24..32], 99u64.to_be_bytes());
        assert!(timing_reply(&[0x80, 0xd4], 0).is_none());

        assert_eq!(
            resend_request(&[0x80, 0xd5, 0, 1, 0, 9, 0, 3]),
            Some((9, 3))
        );
        assert_eq!(resent(5, &[1, 2])[..], [0x80, 0xd6, 0, 5, 1, 2]);
    }

    #[test]
    fn setup_answers_and_volume() {
        assert_eq!(
            transport_ports(
                "RTP/AVP/UDP;unicast;mode=record;server_port=6000;control_port=6001;timing_port=6002"
            ),
            Some((6000, 6001, 6002))
        );
        assert_eq!(transport_ports("RTP/AVP/UDP;unicast"), None);
        assert_eq!(volume_db(100, false), 0.0);
        assert_eq!(volume_db(50, false), -15.0);
        assert_eq!(volume_db(50, true), -144.0);
        assert_eq!(volume_db(0, false), -144.0);
    }

    #[test]
    fn encryption_round_trips() {
        use aes::cipher::{generic_array::GenericArray, BlockDecryptMut, KeyIvInit};
        let c = Cipher::new();
        let clear: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut data = clear.clone();
        c.encrypt(&mut data);
        assert_ne!(data[..992], clear[..992]);
        assert_eq!(data[992..], clear[992..], "the partial block stays clear");
        let mut dec = cbc::Decryptor::<aes::Aes128>::new(&c.key.into(), &c.iv.into());
        for block in data[..992].chunks_exact_mut(16) {
            dec.decrypt_block_mut(GenericArray::from_mut_slice(block));
        }
        assert_eq!(data, clear);
    }

    #[test]
    fn the_key_is_wrapped_for_the_airport_express() {
        // The receiver side: the AirPort Express's private exponent, published long ago
        // (shairport); the key wrapped with our modulus must come out of it.
        use rsa::{BigUint, Oaep, RsaPrivateKey};
        const D: &str = "5fAMcvV31gS5pM5BIqqEsBdD7Jlaz8x/SrJ8Cxh/kGZb41nfElmBje7tedOx74ReTd3ayaFVNzteJw2OExUAGi5SfVTN+QAKV2i8mNREaze7vQCyndi1MGITOypud/TuMlBWIpBNpyD7HBLAOZbacToFBgmO2+3s+TbQ+py9WSmrsO2jV5lQL5iU3Lj8VpqJLRd4AySitsMWbjRnCRNLhUBBuGdwa1j+8qDbkit3Yoto5paTx69Dvypz0LcyN3oLoXtE8FHpv3mEncszMlcf2KcJM8LWC97EeZNKPaykC7by83wKnQcQbq3Is2mgPy9ByIAJjordRiQNrGjMU1TzYQ==";
        let b64 = base64::engine::general_purpose::STANDARD;
        let n = BigUint::from_bytes_be(&b64.decode(AIRPORT_MODULUS).unwrap());
        let d = BigUint::from_bytes_be(&b64.decode(D).unwrap());
        let private = RsaPrivateKey::from_components(n, BigUint::from(65_537u32), d, vec![])
            .expect("a consistent key");

        let c = Cipher::new();
        let sdp = c.sdp().unwrap();
        let line = |name: &str| {
            sdp.lines()
                .find_map(|l| l.strip_prefix(name))
                .unwrap()
                .to_string()
        };
        let nopad = base64::engine::general_purpose::STANDARD_NO_PAD;
        let wrapped = nopad.decode(line("a=rsaaeskey:")).unwrap();
        assert_eq!(wrapped.len(), 256);
        let key = private
            .decrypt(Oaep::new::<sha1::Sha1>(), &wrapped)
            .unwrap();
        assert_eq!(key, c.key);
        assert_eq!(nopad.decode(line("a=aesiv:")).unwrap(), c.iv);
    }
}
