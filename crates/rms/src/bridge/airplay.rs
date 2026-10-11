//! AirPlay speakers (AirPlay 1, "RAOP") as players.
//!
//! Speakers are found with mDNS (`_raop._tcp`, instances named `<MAC>@<Name>`) or given by
//! hand in the `airplayHosts` pref (`host[:port]`; with `--no-discovery`, only those).
//! Speakers that only do AirPlay 2 with pairing, or only FairPlay encryption, are left out;
//! those that ask for RSA (`et=1`) get the audio AES-encrypted.  Receivers that insist on
//! an `Apple-Challenge` answer are not supported.
//!
//! A player declares raw PCM at 44.1 kHz, so the server sends it 16-bit stereo samples
//! (transcoding whatever the file is).  On the first track the driver opens an RTSP session
//! with the speaker (OPTIONS, ANNOUNCE with an ALAC SDP, SETUP, RECORD) and keeps it while
//! tracks follow each other; it closes it (TEARDOWN) after a while without music, or when
//! the player goes away.  The samples go out in RTP packets of 352 frames, each one an
//! uncompressed ALAC frame, paced in real time two seconds ahead of when they are heard.
//! Sync packets tell the speaker which frame plays when; it asks for lost packets again,
//! and for the time, on the control and timing ports.
//!
//! A track is reported ended just before its last frame is heard; when the next one comes
//! in time, its packets follow on the same timeline, without a gap.  Pausing flushes the
//! speaker and keeps what it had not played yet, to send again on resume.

mod raop;

use super::devices::{self, Device, DriverIo, Protocol};
use super::{mac_for, FromRenderer, Identity, Pcm, ToRenderer};
use crate::server::Server;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use parking_lot::Mutex;
use raop::{Cipher, Rtsp, FRAMES_PER_PACKET, SAMPLE_RATE};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// How often the discovery loop looks for new speakers (and at the pref).
const CYCLE: Duration = Duration::from_secs(2);
/// The RAOP port when `airplayHosts` gives none.
const DEFAULT_PORT: u16 = 5000;
const SERVICE: &str = "_raop._tcp.local.";

/// Audio is sent this far ahead of when it is heard; it is also the latency the sync
/// packets give the speaker.
const LEAD: Duration = Duration::from_secs(2);
const LATENCY_FRAMES: u32 = 2 * SAMPLE_RATE;
/// A track is reported ended this long before its last frame is heard, so the next one
/// has time to follow it without a gap.
const END_EARLY: Duration = Duration::from_millis(500);
/// Packets sent at most per tick (10 ms): 8 is about six times real time.
const BURST: usize = 8;
/// Frames of a track read ahead from the server.
const READ_AHEAD: usize = 5 * SAMPLE_RATE as usize;
/// Packets kept to answer resend requests (and to replay after a pause).
const HISTORY: usize = 1024;
/// Without music for this long, the session ends (the speaker is free for others).
const IDLE_TEARDOWN: Duration = Duration::from_secs(30);
/// Without a session, how often the speaker is checked for, and how many misses end it.
const PROBE_EVERY: Duration = Duration::from_secs(30);
const PROBE_MISSES: u32 = 2;

/// Find speakers while the protocol is turned on and run a player for each.
pub async fn run(server: Arc<Server>) {
    let proto = Protocol::by_prefix("airplay");
    let mut mdns: Option<Mdns> = None;
    loop {
        if proto.enabled(&server) {
            if mdns.is_none() && !server.config.no_discovery {
                mdns = Mdns::start();
            }
            let configured = proto.configured(&server);
            let mut given = HashSet::new();
            for entry in &configured {
                let (host, port) = split_host(entry);
                let key = format!("airplay:{host}:{port}");
                let Some(addr) = resolve(&host, port).await else {
                    continue;
                };
                given.insert(addr);
                if devices::is_running(&key) || !reachable(addr).await {
                    continue;
                }
                let target = Target {
                    name: format!("AirPlay {host}"),
                    model_name: "AirPlay speaker".into(),
                    addr,
                    encrypt: false,
                };
                start(&server, key, target);
            }
            if let Some(m) = mdns.as_mut() {
                for (key, target) in m.poll() {
                    if given.contains(&target.addr) || devices::is_running(&key) {
                        continue;
                    }
                    if reachable(target.addr).await {
                        start(&server, key, target);
                    }
                }
            }
        } else if let Some(m) = mdns.take() {
            let _ = m.daemon.shutdown();
        }
        tokio::time::sleep(CYCLE).await;
    }
}

/// Where a speaker is and what it wants.
#[derive(Debug, Clone)]
struct Target {
    name: String,
    model_name: String,
    addr: SocketAddr,
    /// Encrypt the audio (the speaker announced RSA, not "none").
    encrypt: bool,
}

fn start(server: &Arc<Server>, key: String, target: Target) {
    let identity = Identity {
        mac: mac_for(&key),
        name: target.name.clone(),
        model: "airplay".into(),
        model_name: target.model_name.clone(),
        formats: vec!["pcm".into()],
        max_sample_rate: SAMPLE_RATE,
        accurate: false,
    };
    devices::found(
        server,
        Device { key, identity },
        Box::new(move |io| Box::pin(drive(target, io))),
    );
}

/// `host[:port]` (`[v6]:port` for IPv6) → host and port.
fn split_host(entry: &str) -> (String, u16) {
    let entry = entry.trim();
    if let Some(rest) = entry.strip_prefix('[') {
        if let Some((h, p)) = rest.split_once(']') {
            let port = p.strip_prefix(':').and_then(|p| p.parse().ok());
            return (h.to_string(), port.unwrap_or(DEFAULT_PORT));
        }
    }
    match entry.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (h.to_string(), port),
            Err(_) => (entry.to_string(), DEFAULT_PORT),
        },
        _ => (entry.to_string(), DEFAULT_PORT),
    }
}

async fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    let mut addrs = tokio::net::lookup_host((host, port)).await.ok()?;
    addrs.next()
}

/// Whether something answers on the RTSP port.
async fn reachable(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// mDNS browsing for `_raop._tcp`.
struct Mdns {
    daemon: mdns_sd::ServiceDaemon,
    events: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
    /// Usable speakers by service instance, with their device keys.
    seen: HashMap<String, (String, Target)>,
    /// Instances already logged as unusable.
    skipped: HashSet<String>,
}

impl Mdns {
    fn start() -> Option<Mdns> {
        let daemon = mdns_sd::ServiceDaemon::new()
            .map_err(|e| warn!("AirPlay discovery: {e}"))
            .ok()?;
        let events = daemon
            .browse(SERVICE)
            .map_err(|e| warn!("AirPlay discovery: {e}"))
            .ok()?;
        Some(Mdns {
            daemon,
            events,
            seen: HashMap::new(),
            skipped: HashSet::new(),
        })
    }

    /// The speakers known now.
    fn poll(&mut self) -> Vec<(String, Target)> {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                mdns_sd::ServiceEvent::ServiceResolved(info) => {
                    let fullname = info.get_fullname().to_string();
                    let txt = |k: &str| info.get_property_val_str(k).map(str::to_string);
                    let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                    let addr = addrs
                        .iter()
                        .find(|a| a.is_ipv4())
                        .or(addrs.first())
                        .map(|ip| SocketAddr::new(*ip, info.get_port()));
                    match usable(&fullname, txt("et"), txt("cn"), txt("pk"), txt("am")) {
                        Ok((key, name, model_name, encrypt)) => {
                            let Some(addr) = addr else { continue };
                            let target = Target {
                                name,
                                model_name,
                                addr,
                                encrypt,
                            };
                            self.seen.insert(fullname, (key, target));
                        }
                        Err(why) => {
                            if self.skipped.insert(fullname.clone()) {
                                info!("AirPlay: leaving out {fullname}: {why}");
                            }
                        }
                    }
                }
                mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                    self.seen.remove(&fullname);
                }
                _ => {}
            }
        }
        self.seen.values().cloned().collect()
    }
}

/// Whether a `_raop._tcp` instance can be played to, from its name and TXT record: its
/// device key, name, model name and whether to encrypt.
fn usable(
    fullname: &str,
    et: Option<String>,
    cn: Option<String>,
    pk: Option<String>,
    am: Option<String>,
) -> Result<(String, String, String, bool), String> {
    let instance = fullname
        .strip_suffix(&format!(".{SERVICE}"))
        .unwrap_or(fullname);
    let (mac, name) = instance
        .split_once('@')
        .ok_or_else(|| "not named <MAC>@<name>".to_string())?;
    let list = |v: &Option<String>| -> Vec<String> {
        v.as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let et = list(&et);
    let has = |l: &[String], x: &str| l.iter().any(|v| v == x);
    // no `et` at all: the oldest speakers, unencrypted
    let encrypt = match (et.is_empty() || has(&et, "0"), has(&et, "1")) {
        (true, _) => false,
        (false, true) => true,
        (false, false) if pk.is_some() => {
            return Err("AirPlay 2 only (pairing required)".into());
        }
        (false, false) => return Err(format!("no supported encryption (et={})", et.join(","))),
    };
    let cn = list(&cn);
    if !cn.is_empty() && !has(&cn, "1") {
        return Err(format!("does not play ALAC (cn={})", cn.join(",")));
    }
    let model_name = am
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "AirPlay speaker".into());
    Ok((
        format!("airplay:{}", mac.to_ascii_uppercase()),
        name.to_string(),
        model_name,
        encrypt,
    ))
}

// ---------------------------------------------------------------------------------------------
// the driver
// ---------------------------------------------------------------------------------------------

/// Play to one speaker until the player goes away or the speaker can't be reached.
async fn drive(target: Target, io: DriverIo) {
    let mut d = Driver::new(target, io);
    match d.run().await {
        Ok(()) => {}
        Err(e) => {
            warn!("AirPlay {}: {e:#}", d.target.name);
            d.report(FromRenderer::Error {
                message: format!("{e:#}"),
            })
            .await;
        }
    }
    if let Some(mut s) = d.session.take() {
        s.teardown().await;
    }
}

/// A packet that went out.
struct Sent {
    seq: u16,
    rtp: u64,
    /// The track it is from, and where in it.
    track: u64,
    pos: u64,
    /// Its samples (without padding), to send again after a pause.
    pcm: Vec<i16>,
    packet: Bytes,
}

type History = Arc<Mutex<VecDeque<Sent>>>;

/// An RTSP session with the speaker, and its UDP sockets.
struct Session {
    rtsp: Rtsp,
    audio: UdpSocket,
    control: Arc<UdpSocket>,
    audio_to: SocketAddr,
    control_to: SocketAddr,
    cipher: Option<Cipher>,
    tasks: Vec<tokio::task::AbortHandle>,
    torn_down: bool,
}

impl Session {
    async fn teardown(&mut self) {
        if !self.torn_down {
            self.torn_down = true;
            let _ = self.rtsp.request("TEARDOWN", &[], None).await;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        // dropped without a teardown (the driver was cancelled): tell the speaker anyway
        if !self.torn_down {
            self.rtsp.teardown_now();
        }
    }
}

/// When the frames that went out are heard: frame `rtp` of the stream is heard at
/// `anchor.0 + (rtp - anchor.1) / rate`.
struct Timeline {
    anchor: Option<(Instant, u64)>,
    next_rtp: u64,
    next_seq: u64,
    ssrc: u32,
    /// The next packet starts a stream (RTP marker bit).
    marker: bool,
    /// Packets went out since the last flush.
    need_flush: bool,
    last_sync: Instant,
}

impl Timeline {
    fn new() -> Timeline {
        Timeline {
            anchor: None,
            // well away from 0, so "the frame heard now" is never negative
            next_rtp: u64::from(rand::random::<u32>() >> 1) + 10 * u64::from(SAMPLE_RATE),
            next_seq: u64::from(rand::random::<u16>()),
            ssrc: rand::random(),
            marker: true,
            need_flush: false,
            last_sync: Instant::now(),
        }
    }

    /// The frame being heard at `now`.
    fn heard(&self, now: Instant) -> Option<u64> {
        let (t0, r0) = self.anchor?;
        let rate = f64::from(SAMPLE_RATE);
        Some(if now >= t0 {
            r0 + ((now - t0).as_secs_f64() * rate) as u64
        } else {
            r0.saturating_sub(((t0 - now).as_secs_f64() * rate) as u64)
        })
    }

    /// When frame `rtp` is heard.
    fn when(&self, rtp: u64) -> Option<Instant> {
        let (t0, r0) = self.anchor?;
        let secs = (rtp as f64 - r0 as f64) / f64::from(SAMPLE_RATE);
        Some(if secs >= 0.0 {
            t0 + Duration::from_secs_f64(secs)
        } else {
            t0.checked_sub(Duration::from_secs_f64(-secs)).unwrap_or(t0)
        })
    }
}

/// A track coming from the server.
struct Track {
    id: u64,
    pcm: Pcm,
    body: Option<BoxStream<'static, reqwest::Result<Bytes>>>,
    /// Bytes short of a whole frame.
    partial: Vec<u8>,
    /// 16-bit stereo samples to send, interleaved.
    samples: VecDeque<i16>,
    /// The frame of the track `samples` starts at.
    pos: u64,
    /// The stream frame the track's first frame is (or would have been) heard at.
    start_rtp: u64,
    /// Elapsed is not less than this (frames): where it resumes after a pause.
    floor: u64,
    sent_any: bool,
    started: bool,
    ended: bool,
    paused: bool,
    last_progress: Instant,
}

impl Track {
    fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    fn wants_data(&self) -> bool {
        self.body.is_some() && self.frames() < READ_AHEAD
    }

    /// Everything the server sent has gone out.
    fn drained(&self) -> bool {
        self.body.is_none() && self.samples.is_empty()
    }

    /// Seconds into the track at `heard`.
    fn elapsed(&self, heard: Option<u64>) -> f64 {
        let at = heard
            .map(|h| h.saturating_sub(self.start_rtp))
            .unwrap_or(0)
            .clamp(self.floor, self.pos.max(self.floor));
        at as f64 / f64::from(SAMPLE_RATE)
    }

    /// Append whole frames of `bytes` (after what was left over) as 16-bit stereo.
    fn push(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        let width = (self.pcm.bits / 8) as usize;
        let frame = width * self.pcm.channels as usize;
        let whole = self.partial.len() / frame * frame;
        for f in self.partial[..whole].chunks_exact(frame) {
            let mut ch = f
                .chunks_exact(width)
                .map(|s| sample16(s, self.pcm.little_endian));
            let left = ch.next().unwrap_or(0);
            let right = ch.next().unwrap_or(left);
            self.samples.push_back(left);
            self.samples.push_back(right);
        }
        self.partial.drain(..whole);
    }
}

/// One sample of 16, 24 or 32 bits → 16 bits.
fn sample16(s: &[u8], little_endian: bool) -> i16 {
    let hi = if little_endian { s[s.len() - 1] } else { s[0] };
    let next = if little_endian { s[s.len() - 2] } else { s[1] };
    i16::from_be_bytes([hi, next])
}

/// Why a PCM format can't be played (it is converted if it isn't 16-bit stereo).
fn unplayable(pcm: &Pcm) -> Option<String> {
    if pcm.rate != SAMPLE_RATE {
        return Some(format!("sample rate {} (only 44100 plays)", pcm.rate));
    }
    if !matches!(pcm.bits, 16 | 24 | 32) || !matches!(pcm.channels, 1 | 2) {
        return Some(format!("{} bits, {} channels", pcm.bits, pcm.channels));
    }
    None
}

struct Driver {
    target: Target,
    io: DriverIo,
    http: reqwest::Client,
    volume: Option<(u8, bool)>,
    session: Option<Session>,
    tl: Timeline,
    track: Option<Track>,
    /// The preloaded track, which follows the current one.
    next: Option<Track>,
    /// What wasn't heard of the track before when it was paused: it goes first on resume.
    tail: VecDeque<i16>,
    history: History,
    track_ids: u64,
    idle_since: Instant,
    last_probe: Instant,
    probe_misses: u32,
}

impl Driver {
    fn new(target: Target, io: DriverIo) -> Driver {
        Driver {
            target,
            io,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            volume: None,
            session: None,
            tl: Timeline::new(),
            track: None,
            next: None,
            tail: VecDeque::new(),
            history: Arc::new(Mutex::new(VecDeque::new())),
            track_ids: 0,
            idle_since: Instant::now(),
            last_probe: Instant::now(),
            probe_misses: 0,
        }
    }

    /// (`&mut`: a `&Driver` held across an await would need it to be `Sync`.)
    async fn report(&mut self, msg: FromRenderer) {
        let _ = self.io.reports.send(msg).await;
    }

    async fn run(&mut self) -> anyhow::Result<()> {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let wants = self.track.as_ref().is_some_and(Track::wants_data);
            tokio::select! {
                cmd = self.io.commands.recv() => match cmd {
                    Some(cmd) => self.command(cmd).await?,
                    None => return Ok(()),
                },
                chunk = next_chunk(&mut self.track), if wants => {
                    if let Some(t) = self.track.as_mut() {
                        match chunk {
                            Some(Ok(bytes)) => t.push(&bytes),
                            Some(Err(e)) => {
                                warn!("AirPlay {}: reading the stream: {e}", self.target.name);
                                t.body = None;
                            }
                            None => t.body = None,
                        }
                    }
                }
                _ = closed(&mut self.session), if self.session.is_some() => {
                    self.session = None;
                    self.tl.anchor = None;
                    if self.track.as_ref().is_some_and(|t| !t.ended) {
                        anyhow::bail!("the speaker closed the connection");
                    }
                    // between tracks: the next one opens a new session
                    info!("AirPlay {}: the speaker ended the session", self.target.name);
                    self.last_probe = Instant::now();
                }
                _ = tick.tick() => {
                    if !self.tick().await? {
                        info!("AirPlay {}: can't be reached any more", self.target.name);
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn command(&mut self, cmd: ToRenderer) -> anyhow::Result<()> {
        debug!("AirPlay {}: {cmd:?}", self.target.name);
        match cmd {
            ToRenderer::Load {
                url,
                format,
                pcm,
                autostart,
            } => {
                self.next = None;
                self.tail.clear();
                // what still plays of a track that didn't end is cut
                if self.track.take().is_some_and(|t| !t.ended) {
                    self.flush().await?;
                }
                let Some(mut track) = self.open(&url, &format, pcm).await else {
                    return Ok(());
                };
                if self.session.is_none() && !self.open_session().await? {
                    return Ok(());
                }
                track.paused = !autostart;
                self.track = Some(track);
                if !autostart {
                    self.report(FromRenderer::Ready).await;
                }
            }
            ToRenderer::Preload { url, format, pcm } => {
                self.next = self.open(&url, &format, pcm).await;
            }
            ToRenderer::Pause => {
                let Some(t) = self.track.as_mut().filter(|t| !t.paused) else {
                    return Ok(());
                };
                t.paused = true;
                let started = t.started;
                if t.sent_any {
                    self.pause_stream().await?;
                }
                if started {
                    let elapsed = self.track.as_ref().map_or(0.0, |t| t.elapsed(None));
                    self.report(FromRenderer::Progress {
                        elapsed,
                        duration: None,
                        jiffies: None,
                    })
                    .await;
                }
                self.report(FromRenderer::Paused).await;
            }
            ToRenderer::Resume { .. } => {
                let Some(t) = self.track.as_mut().filter(|t| t.paused) else {
                    return Ok(());
                };
                t.paused = false;
                t.last_progress = Instant::now();
                if t.started {
                    self.report(FromRenderer::Resumed).await;
                }
            }
            ToRenderer::Stop => {
                self.next = None;
                self.tail.clear();
                // a track that ended plays out; one that didn't is cut
                if self.track.take().is_some_and(|t| !t.ended) {
                    self.flush().await?;
                }
                self.idle_since = Instant::now();
            }
            // (only for renderers with their own clock)
            ToRenderer::Hold { .. } | ToRenderer::Skip { .. } => {}
            ToRenderer::Volume { volume, muted } => {
                // (the server repeats it, e.g. while it fades on pause)
                if self.volume != Some((volume, muted)) {
                    self.volume = Some((volume, muted));
                    self.send_volume().await?;
                }
            }
        }
        Ok(())
    }

    /// Start fetching a track; reports an error if it can't be played.
    async fn open(&mut self, url: &str, format: &str, pcm: Option<Pcm>) -> Option<Track> {
        let pcm = pcm.unwrap_or(Pcm {
            bits: 16,
            rate: SAMPLE_RATE,
            channels: 2,
            little_endian: true,
        });
        let problem = if format != "pcm" {
            Some(format!("format {format} (only pcm plays)"))
        } else {
            unplayable(&pcm)
        };
        if let Some(p) = problem {
            self.report(FromRenderer::Error { message: p }).await;
            return None;
        }
        let full = format!("{}{}", self.io.base_url, url);
        let resp = match self.http.get(&full).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let message = format!("{full}: {}", r.status());
                self.report(FromRenderer::Error { message }).await;
                return None;
            }
            Err(e) => {
                let message = format!("{full}: {e}");
                self.report(FromRenderer::Error { message }).await;
                return None;
            }
        };
        self.track_ids += 1;
        Some(Track {
            id: self.track_ids,
            pcm,
            body: Some(resp.bytes_stream().boxed()),
            partial: Vec::new(),
            samples: VecDeque::new(),
            pos: 0,
            start_rtp: 0,
            floor: 0,
            sent_any: false,
            started: false,
            ended: false,
            paused: false,
            last_progress: Instant::now(),
        })
    }

    /// Set up a session.  `false` (with an error reported) if the speaker refused; an
    /// error if it can't be reached.
    async fn open_session(&mut self) -> anyhow::Result<bool> {
        let mut rtsp = Rtsp::connect(self.target.addr).await?;
        let refused = |what: &str, r: &raop::Response| {
            format!(
                "{} refused {what}: {} {}",
                self.target.name, r.status, r.reason
            )
        };
        let r = rtsp.request("OPTIONS", &[], None).await?;
        if !r.ok() {
            let message = refused("OPTIONS", &r);
            self.report(FromRenderer::Error { message }).await;
            return Ok(false);
        }
        let cipher = self.target.encrypt.then(Cipher::new);
        let sdp = raop::sdp(&rtsp, cipher.as_ref())?;
        let r = rtsp
            .request("ANNOUNCE", &[], Some(("application/sdp", sdp.as_bytes())))
            .await?;
        if !r.ok() {
            // 453: busy playing something else
            let message = refused("to play", &r);
            self.report(FromRenderer::Error { message }).await;
            return Ok(false);
        }
        let any: IpAddr = if self.target.addr.is_ipv6() {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let audio = UdpSocket::bind((any, 0)).await?;
        let control = Arc::new(UdpSocket::bind((any, 0)).await?);
        let timing = Arc::new(UdpSocket::bind((any, 0)).await?);
        let transport = format!(
            "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
            control.local_addr()?.port(),
            timing.local_addr()?.port()
        );
        let r = rtsp
            .request("SETUP", &[("Transport", transport)], None)
            .await?;
        let ports = r.header("Transport").and_then(raop::transport_ports);
        let (Some((server_port, control_port, _)), true) = (ports, r.ok()) else {
            let message = refused("SETUP", &r);
            self.report(FromRenderer::Error { message }).await;
            return Ok(false);
        };
        rtsp.session = r
            .header("Session")
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());
        let ip = self.target.addr.ip();
        let control_to = SocketAddr::new(ip, control_port);
        let tasks = vec![
            tokio::spawn(answer_timing(timing)).abort_handle(),
            tokio::spawn(answer_resends(control.clone(), self.history.clone())).abort_handle(),
        ];
        let mut session = Session {
            rtsp,
            audio,
            control,
            audio_to: SocketAddr::new(ip, server_port),
            control_to,
            cipher,
            tasks,
            torn_down: false,
        };
        let info = format!(
            "seq={};rtptime={}",
            self.tl.next_seq as u16, self.tl.next_rtp as u32
        );
        let r = session
            .rtsp
            .request(
                "RECORD",
                &[("Range", "npt=0-".into()), ("RTP-Info", info)],
                None,
            )
            .await?;
        if !r.ok() {
            let message = refused("RECORD", &r);
            self.report(FromRenderer::Error { message }).await;
            session.teardown().await;
            return Ok(false);
        }
        info!("AirPlay {}: session started", self.target.name);
        self.session = Some(session);
        self.tl.anchor = None;
        self.tl.marker = true;
        self.tl.need_flush = false;
        self.history.lock().clear();
        self.send_volume().await?;
        Ok(true)
    }

    async fn send_volume(&mut self) -> anyhow::Result<()> {
        let (Some(s), Some((volume, muted))) = (self.session.as_mut(), self.volume) else {
            return Ok(());
        };
        let body = format!("volume: {:.6}\r\n", raop::volume_db(volume, muted));
        let r = s
            .rtsp
            .request(
                "SET_PARAMETER",
                &[],
                Some(("text/parameters", body.as_bytes())),
            )
            .await?;
        if !r.ok() {
            warn!("AirPlay {}: volume refused: {}", self.target.name, r.status);
        }
        Ok(())
    }

    /// Drop what the speaker holds and hasn't played; the stream starts over.
    async fn flush(&mut self) -> anyhow::Result<()> {
        self.tl.anchor = None;
        let Some(s) = self.session.as_mut() else {
            return Ok(());
        };
        if !self.tl.need_flush {
            return Ok(());
        }
        self.tl.need_flush = false;
        self.tl.marker = true;
        let info = format!(
            "seq={};rtptime={}",
            self.tl.next_seq as u16, self.tl.next_rtp as u32
        );
        let r = s.rtsp.request("FLUSH", &[("RTP-Info", info)], None).await?;
        if !r.ok() {
            warn!("AirPlay {}: flush refused: {}", self.target.name, r.status);
        }
        Ok(())
    }

    /// Pause: flush, and put back what wasn't heard yet, to play it on resume.
    async fn pause_stream(&mut self) -> anyhow::Result<()> {
        let heard = self.tl.heard(Instant::now());
        self.flush().await?;
        let Some(t) = self.track.as_mut() else {
            return Ok(());
        };
        let at = t.elapsed(heard);
        let mut history = self.history.lock();
        let (unheard, before): (Vec<&Sent>, Vec<&Sent>) = history
            .iter()
            .filter(|s| Some(s.rtp + FRAMES_PER_PACKET as u64) > heard)
            .partition(|s| s.track == t.id);
        // the end of the track before (paused just as this one was to start)
        self.tail = before.iter().flat_map(|s| s.pcm.clone()).collect();
        if !t.started {
            t.start_rtp = u64::MAX;
        }
        if let Some(first) = unheard.first() {
            let mut again: VecDeque<i16> = unheard.iter().flat_map(|s| s.pcm.clone()).collect();
            again.extend(t.samples.drain(..));
            t.samples = again;
            t.pos = first.pos;
            t.floor = first.pos;
        } else {
            t.floor = (at * f64::from(SAMPLE_RATE)) as u64;
        }
        history.clear();
        Ok(())
    }

    /// Send what is due, report what is heard.  `false` when the speaker is gone.
    async fn tick(&mut self) -> anyhow::Result<bool> {
        let now = Instant::now();
        if self.session.is_none() {
            return Ok(self.probe(now).await);
        }
        self.send_due(now).await?;
        let heard = self.tl.heard(now);
        let Some(t) = self.track.as_mut() else {
            return self.idle(now).await;
        };
        let playing = !t.paused && t.sent_any;
        if playing && !t.started && heard.is_some_and(|h| h >= t.start_rtp) {
            t.started = true;
            t.last_progress = now;
            self.report(FromRenderer::Started).await;
            return Ok(true);
        }
        if t.started && !t.ended && !t.paused && now - t.last_progress >= Duration::from_secs(1) {
            t.last_progress = now;
            let elapsed = t.elapsed(heard);
            self.report(FromRenderer::Progress {
                elapsed,
                duration: None,
                jiffies: None,
            })
            .await;
            return Ok(true);
        }
        if !t.drained() || !self.tail.is_empty() || t.ended || t.paused {
            return if t.ended {
                self.idle(now).await
            } else {
                Ok(true)
            };
        }
        // all of it went out: the preloaded track follows; or the end is near
        if t.started && self.next.is_some() {
            self.track = self.next.take();
            return Ok(true);
        }
        let left = self
            .tl
            .when(self.tl.next_rtp)
            .map(|w| w.saturating_duration_since(now));
        if t.started && left.is_none_or(|l| l <= END_EARLY) {
            t.ended = true;
            self.idle_since = now;
            self.report(FromRenderer::Ended).await;
        } else if !t.sent_any {
            // nothing in the stream
            t.ended = true;
            self.idle_since = now;
            self.report(FromRenderer::Started).await;
            self.report(FromRenderer::Ended).await;
        }
        Ok(true)
    }

    /// The session ends after a while without music.
    async fn idle(&mut self, now: Instant) -> anyhow::Result<bool> {
        let still = self.tl.when(self.tl.next_rtp).is_some_and(|w| w > now);
        if !still && now - self.idle_since >= IDLE_TEARDOWN {
            if let Some(mut s) = self.session.take() {
                info!("AirPlay {}: idle, ending the session", self.target.name);
                s.teardown().await;
            }
            self.tl.anchor = None;
            self.last_probe = now;
        }
        Ok(true)
    }

    /// Without a session: is the speaker still there?
    async fn probe(&mut self, now: Instant) -> bool {
        if now - self.last_probe < PROBE_EVERY {
            return true;
        }
        self.last_probe = now;
        if reachable(self.target.addr).await {
            self.probe_misses = 0;
        } else {
            self.probe_misses += 1;
        }
        self.probe_misses < PROBE_MISSES
    }

    /// Send the packets that are due: up to [`LEAD`] ahead of what is heard.
    async fn send_due(&mut self, now: Instant) -> anyhow::Result<()> {
        // catching up (a track that follows another) goes at most this fast, so a burst
        // doesn't overrun the speaker's socket
        for _ in 0..BURST {
            let Some(t) = self.track.as_ref() else {
                return Ok(());
            };
            let tail = self.tail.len() / 2;
            let n = if tail > 0 { tail } else { t.frames() }.min(FRAMES_PER_PACKET);
            let ready = n == FRAMES_PER_PACKET || (n > 0 && (tail > 0 || t.body.is_none()));
            if t.paused || !ready {
                break;
            }
            // the speaker ran out (a track that took long to come): start over.  While it
            // still plays what came before, the stream goes on where that ends.
            if self.tl.when(self.tl.next_rtp).is_some_and(|w| w <= now) {
                debug!("AirPlay {}: ran out, starting over", self.target.name);
                self.flush().await?;
            }
            if self.tl.anchor.is_none() {
                self.tl.anchor = Some((now + LEAD, self.tl.next_rtp));
                self.tl.marker = true;
                self.sync(now, true).await;
            }
            if self
                .tl
                .when(self.tl.next_rtp)
                .is_some_and(|w| w > now + LEAD)
            {
                break;
            }
            self.send_packet(n, tail > 0).await?;
        }
        if now - self.tl.last_sync >= Duration::from_secs(1)
            && self.tl.when(self.tl.next_rtp).is_some_and(|w| w > now)
        {
            self.sync(now, false).await;
        }
        Ok(())
    }

    async fn sync(&mut self, now: Instant, first: bool) {
        self.tl.last_sync = now;
        let (Some(s), Some(heard)) = (self.session.as_ref(), self.tl.heard(now)) else {
            return;
        };
        let p = raop::sync_packet(first, heard as u32, raop::ntp_now(), LATENCY_FRAMES);
        let _ = s.control.send_to(&p, s.control_to).await;
    }

    /// Send the next `n` frames of the track (or of the tail before it) as one packet.
    async fn send_packet(&mut self, n: usize, from_tail: bool) -> anyhow::Result<()> {
        let (Some(s), Some(t)) = (self.session.as_ref(), self.track.as_mut()) else {
            return Ok(());
        };
        let pcm: Vec<i16> = if from_tail {
            self.tail.drain(..n * 2).collect()
        } else {
            t.samples.drain(..n * 2).collect()
        };
        let mut samples = pcm.clone();
        samples.resize(FRAMES_PER_PACKET * 2, 0);
        let mut payload = raop::alac_frame(&samples);
        if let Some(c) = &s.cipher {
            c.encrypt(&mut payload);
        }
        let seq = self.tl.next_seq as u16;
        let header = raop::rtp_header(self.tl.marker, seq, self.tl.next_rtp as u32, self.tl.ssrc);
        let mut packet = Vec::with_capacity(header.len() + payload.len());
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&payload);
        if let Err(e) = s.audio.send_to(&packet, s.audio_to).await {
            debug!("AirPlay {}: sending audio: {e}", self.target.name);
        }

        let mut history = self.history.lock();
        history.push_back(Sent {
            seq,
            rtp: self.tl.next_rtp,
            track: if from_tail { 0 } else { t.id },
            pos: t.pos,
            pcm,
            packet: packet.into(),
        });
        if history.len() > HISTORY {
            history.pop_front();
        }
        if !from_tail {
            t.start_rtp = self.tl.next_rtp - t.pos;
            t.sent_any = true;
            t.pos += n as u64;
        }
        self.tl.next_seq += 1;
        self.tl.next_rtp += FRAMES_PER_PACKET as u64;
        self.tl.marker = false;
        self.tl.need_flush = true;
        Ok(())
    }
}

async fn next_chunk(track: &mut Option<Track>) -> Option<reqwest::Result<Bytes>> {
    match track.as_mut().and_then(|t| t.body.as_mut()) {
        Some(body) => body.next().await,
        None => std::future::pending().await,
    }
}

async fn closed(session: &mut Option<Session>) {
    match session.as_mut() {
        Some(s) => s.rtsp.closed().await,
        None => std::future::pending().await,
    }
}

/// Answer the speaker's timing requests.
async fn answer_timing(sock: Arc<UdpSocket>) {
    let mut buf = [0u8; 128];
    while let Ok((n, from)) = sock.recv_from(&mut buf).await {
        if let Some(reply) = raop::timing_reply(&buf[..n], raop::ntp_now()) {
            let _ = sock.send_to(&reply, from).await;
        }
    }
}

/// Send again the packets the speaker missed.
async fn answer_resends(sock: Arc<UdpSocket>, history: History) {
    let mut buf = [0u8; 128];
    while let Ok((n, from)) = sock.recv_from(&mut buf).await {
        let Some((first, count)) = raop::resend_request(&buf[..n]) else {
            continue;
        };
        for i in 0..count {
            let seq = first.wrapping_add(i);
            let packet = history
                .lock()
                .iter()
                .find(|s| s.seq == seq)
                .map(|s| s.packet.clone());
            if let Some(p) = packet {
                let _ = sock.send_to(&raop::resent(seq, &p), from).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_ports() {
        assert_eq!(split_host("10.0.0.5"), ("10.0.0.5".into(), 5000));
        assert_eq!(split_host(" 10.0.0.5:7000 "), ("10.0.0.5".into(), 7000));
        assert_eq!(split_host("speaker.local"), ("speaker.local".into(), 5000));
        assert_eq!(split_host("[fe80::1]:5001"), ("fe80::1".into(), 5001));
        assert_eq!(split_host("fe80::1"), ("fe80::1".into(), 5000));
    }

    #[test]
    fn which_speakers_are_used() {
        let s = |v: &str| Some(v.to_string());
        let name = "AABBCCDDEEFF@Kitchen._raop._tcp.local.";
        let (key, n, model, enc) =
            usable(name, s("0,3,5"), s("0,1,2,3"), None, s("AirPort4,107")).unwrap();
        assert_eq!(key, "airplay:AABBCCDDEEFF");
        assert_eq!(n, "Kitchen");
        assert_eq!(model, "AirPort4,107");
        assert!(!enc);
        // RSA only: encrypted
        assert!(usable(name, s("1"), None, None, None).unwrap().3);
        // nothing said: the oldest speakers, in the clear
        assert!(!usable(name, None, None, None, None).unwrap().3);
        // AirPlay 2 with pairing, FairPlay only, no ALAC, odd names: left out
        assert!(usable(name, s("3,4,5"), s("1"), s("abcdef"), None).is_err());
        assert!(usable(name, s("3"), None, None, None).is_err());
        assert!(usable(name, s("0"), s("0,2"), None, None).is_err());
        assert!(usable("Kitchen._raop._tcp.local.", s("0"), None, None, None).is_err());
    }

    #[test]
    fn samples_become_16_bit_stereo() {
        let pcm = |bits, channels, little_endian| Pcm {
            bits,
            rate: 44100,
            channels,
            little_endian,
        };
        let mut t = Track {
            id: 1,
            pcm: pcm(16, 2, true),
            body: None,
            partial: Vec::new(),
            samples: VecDeque::new(),
            pos: 0,
            start_rtp: 0,
            floor: 0,
            sent_any: false,
            started: false,
            ended: false,
            paused: false,
            last_progress: Instant::now(),
        };
        // a frame split across chunks
        t.push(&[0x34, 0x12, 0xff]);
        t.push(&[0xff, 0x01]);
        assert_eq!(t.samples, [0x1234, -1]);
        assert_eq!(t.partial, [0x01]);

        t.pcm = pcm(24, 1, false);
        t.partial.clear();
        t.samples.clear();
        t.push(&[0x12, 0x34, 0x56, 0x80, 0x00, 0x00]);
        assert_eq!(t.samples, [0x1234, 0x1234, i16::MIN, i16::MIN]);

        assert!(unplayable(&pcm(16, 2, true)).is_none());
        assert!(unplayable(&Pcm {
            rate: 48000,
            ..pcm(16, 2, true)
        })
        .is_some());
        assert!(unplayable(&pcm(8, 2, true)).is_some());
        assert!(unplayable(&pcm(16, 6, true)).is_some());
    }

    #[test]
    fn the_timeline_maps_frames_to_time() {
        let mut tl = Timeline::new();
        assert!(tl.heard(Instant::now()).is_none());
        let t0 = Instant::now() + Duration::from_secs(2);
        tl.anchor = Some((t0, 1_000_000));
        assert_eq!(tl.heard(t0), Some(1_000_000));
        assert_eq!(tl.heard(t0 + Duration::from_secs(1)), Some(1_044_100));
        assert_eq!(tl.heard(t0 - Duration::from_secs(1)), Some(955_900));
        let w = tl.when(1_044_100).unwrap();
        assert!((w - t0).as_secs_f64() - 1.0 < 1e-6);
    }
}
