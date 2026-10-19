//! Chromecast (Google Cast) devices as players.
//!
//! Devices are found with mDNS (`_googlecast._tcp`) and from the `chromecastHosts` pref
//! (`host[:port]`, for networks where multicast doesn't reach).  Each one is driven with the
//! Cast v2 protocol: JSON messages wrapped in a small protobuf (`CastMessage`), framed by a
//! 4-byte length, over TLS to port 8009.  The device's certificate is self-signed, so it is
//! not checked — the connection only carries play commands and URLs of the server's own
//! streams.
//!
//! On the first track the driver launches RMS's own receiver app (`receiver/`, a page on
//! GitHub Pages registered as a Cast app; `visualizerReceiverApp` names another), and tells
//! it to `LOAD` the stream URL; the device fetches the audio from the server and reports
//! its state in `MEDIA_STATUS` messages, which become the bridge's [`FromRenderer`]
//! reports.  The next track is loaded when the current one finishes.  With each `LOAD`
//! go the server's visualiser settings and what to show (`customData.rms`): MilkDrop drawn
//! on the TV (`draw`), the server's visualiser video (`video`), or the cover (`off`).
//! The app also answers on its own namespace ([`NS_VIZ`]).
//!
//! With the player's visualiser set to the video (`visualizer`; when unset, for devices
//! with a screen) and the `pmviz` helper installed, the device plays its sync group's
//! visualiser stream instead ([`crate::viz`]): video with the music in it, the tracks one
//! after another in one stream.  The TV that starts it feeds it the tracks; the group's
//! other visualiser TVs join it.  Where each track starts in the stream turns the device's
//! position into the track it plays and how far into it, and the app is told each new
//! track (`TRACK`) for its track info.
//!
//! The driver keeps time like a Squeezebox (an *accurate* renderer, see [`super`]), so a
//! Chromecast plays in step with the players it is synced with.  Its clock is its own
//! ([`CastClock`]).  Once a second it asks the app where playback is (`PING`); the app
//! answers with the position being heard, less what the audio takes to leave the browser,
//! and the answer is taken as true halfway between asking and hearing back.  (Until the
//! app has answered, the positions in `MEDIA_STATUS` serve.)  A synced start (`Resume` at
//! a moment) sends `PLAY` that much earlier than the moment as the device takes to start,
//! which is learned from how it started before.  A sync adjustment plays a little faster
//! or slower (`SET_PLAYBACK_RATE`, 5% or 10%) until the difference is made up, rather than
//! skipping or pausing.  What the device's sound takes to come out after that (a TV's
//! speakers, a soundbar) is the player's `playDelay`.

use super::cast_clock::{CastClock, ClockLink};
use super::devices::{self, Device, DriverIo, Protocol};
use super::{mac_for, FromRenderer, Identity, Pcm, ToRenderer};
use crate::server::Server;
use bytes::{Buf, BytesMut};
use serde_json::{json, Value};
use slimproto::MacAddress;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;
use tracing::{debug, info, warn};

const SERVICE: &str = "_googlecast._tcp.local.";
const DEFAULT_PORT: u16 = 8009;

/// How often discovery looks at the prefs and what it found.
const CYCLE: Duration = Duration::from_secs(1);
/// How long to wait before trying an unreachable device again.
const RETRY: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Answers to `PING`s asked one after another when the receiver app starts: the quickest
/// of them lines the clocks up, even when some take longer.
const LINK_FIRST: usize = 8;
/// How long a receiver that's ready waits for those answers before playing all the same.
const LINK_WAIT: Duration = Duration::from_secs(2);
/// How long the receiver app may take to close when the driver closes it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(5);
/// A device silent for this long is gone.
const SILENCE: Duration = Duration::from_secs(30);
/// How long the Default Media Receiver may take to start.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
const SENDER: &str = "sender-0";
const RECEIVER: &str = "receiver-0";
/// `supportedMediaCommands`: the receiver changes the playback rate.
const PLAYBACK_RATE: u64 = 8192;
/// How long the receiver app may take to say what it can do before it's taken to have
/// only Cast's media handling.
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

/// Sync adjustments smaller than this (seconds) are left alone: no one hears them, and
/// they're within what the position can be known to.
const MIN_ADJUST: f64 = 0.025;
/// What the device takes from `PLAY` to playing, until it has been seen (seconds): rather
/// too little than too much, since a player ahead of the others makes them skip.
const START_LATENCY: f64 = 0.15;

/// What RMS's receiver app says (how fast it draws, what failed).
const NS_VIZ: &str = "urn:x-cast:net.rms.visualizer";

/// A device to connect to.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    key: String,
    host: String,
    port: u16,
    name: String,
    model_name: String,
    /// It has a screen (mDNS `ca`, capability bit 0); unknown for devices given by address.
    video: Option<bool>,
}

/// Find devices while the protocol is turned on and run a player for each.
pub async fn run(server: Arc<Server>) {
    let proto = Protocol::by_prefix("chromecast");
    let mut mdns: Option<Mdns> = None;
    // devices mDNS found, by fullname
    let mut seen: HashMap<String, Candidate> = HashMap::new();
    let mut tried: HashMap<String, Instant> = HashMap::new();
    loop {
        if !proto.enabled(&server) {
            if let Some(m) = mdns.take() {
                let _ = m.daemon.shutdown();
            }
            seen.clear();
            tried.clear();
            tokio::time::sleep(CYCLE).await;
            continue;
        }
        if mdns.is_none() {
            mdns = Mdns::start();
        }

        // what mDNS has to say for one cycle
        let deadline = tokio::time::Instant::now() + CYCLE;
        let mut stopped = false;
        match &mdns {
            Some(m) => loop {
                match tokio::time::timeout_at(deadline, m.events.recv_async()).await {
                    Ok(Ok(ev)) => on_mdns(&mut seen, ev),
                    Ok(Err(_)) => {
                        stopped = true;
                        break;
                    }
                    Err(_) => break,
                }
            },
            None => tokio::time::sleep_until(deadline).await,
        }
        if stopped {
            warn!("chromecast: mDNS stopped");
            mdns = None;
            tokio::time::sleep_until(deadline).await;
        }

        let mut candidates: Vec<Candidate> = seen.values().cloned().collect();
        for entry in proto.configured(&server) {
            let Some((host, port)) = parse_host(&entry) else {
                warn!("chromecast: can't use {entry:?}");
                continue;
            };
            // the same device as one mDNS found: that one (with its name)
            if candidates.iter().any(|c| c.host == host && c.port == port) {
                continue;
            }
            candidates.push(Candidate {
                key: format!("chromecast:{host}:{port}"),
                name: host.clone(),
                model_name: "Chromecast".into(),
                host,
                port,
                video: None,
            });
        }
        for c in candidates {
            if devices::is_running(&c.key) || tried.get(&c.key).is_some_and(|t| t.elapsed() < RETRY)
            {
                continue;
            }
            tried.insert(c.key.clone(), Instant::now());
            crate::util::spawn(attempt(server.clone(), c));
        }
    }
}

struct Mdns {
    daemon: mdns_sd::ServiceDaemon,
    events: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
}

impl Mdns {
    fn start() -> Option<Mdns> {
        let start = || -> mdns_sd::Result<Mdns> {
            let daemon = mdns_sd::ServiceDaemon::new()?;
            let events = daemon.browse(SERVICE)?;
            Ok(Mdns { daemon, events })
        };
        start()
            .map_err(|e| warn!("chromecast: no mDNS discovery: {e}"))
            .ok()
    }
}

fn on_mdns(seen: &mut HashMap<String, Candidate>, ev: mdns_sd::ServiceEvent) {
    use mdns_sd::ServiceEvent;
    match ev {
        ServiceEvent::ServiceResolved(info) => {
            let addrs = info.get_addresses();
            let Some(ip) = addrs
                .iter()
                .find(|a| a.is_ipv4())
                .or_else(|| addrs.iter().next())
            else {
                return;
            };
            let fullname = info.get_fullname().to_string();
            let id = info
                .get_property_val_str("id")
                .filter(|s| !s.is_empty())
                .unwrap_or(&fullname);
            let name = info
                .get_property_val_str("fn")
                .filter(|s| !s.is_empty())
                .unwrap_or("Chromecast");
            let model = info
                .get_property_val_str("md")
                .filter(|s| !s.is_empty())
                .unwrap_or("Chromecast");
            let video = info
                .get_property_val_str("ca")
                .and_then(|ca| ca.parse::<u32>().ok())
                .map(|ca| ca & 1 != 0);
            let c = Candidate {
                key: format!("chromecast:{id}"),
                host: ip.to_string(),
                port: info.get_port(),
                name: name.to_string(),
                model_name: model.to_string(),
                video,
            };
            if seen.get(&fullname) != Some(&c) {
                debug!("chromecast: {} at {}:{}", c.name, c.host, c.port);
            }
            seen.insert(fullname, c);
        }
        ServiceEvent::ServiceRemoved(_, fullname) => {
            seen.remove(&fullname);
        }
        _ => {}
    }
}

/// `host`, `host:port`, `[v6]:port` → host and port.
fn parse_host(s: &str) -> Option<(String, u16)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(a) = s.parse::<std::net::SocketAddr>() {
        return Some((a.ip().to_string(), a.port()));
    }
    if let Ok(ip) = s.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        return Some((ip.to_string(), DEFAULT_PORT));
    }
    match s.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => {
            Some((host.to_string(), port.parse().ok()?))
        }
        Some(_) => None,
        None => Some((s.to_string(), DEFAULT_PORT)),
    }
}

/// Connect to a device and, when it answers, run a player for it.
async fn attempt(server: Arc<Server>, c: Candidate) {
    let tls = match tokio::time::timeout(CONNECT_TIMEOUT, connect(&c.host, c.port)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(e)) => {
            debug!("chromecast: {}:{}: {e}", c.host, c.port);
            return;
        }
        Err(_) => {
            debug!("chromecast: {}:{}: no answer", c.host, c.port);
            return;
        }
    };
    let mac = mac_for(&c.key);
    let identity = Identity {
        mac,
        name: c.name.clone(),
        model: "chromecast".into(),
        model_name: c.model_name.clone(),
        // FLAC whatever the file: the receiver's Web Audio player decodes it (and Cast's
        // own media handling plays it too).  Raw samples where a synced player can't take
        // FLAC (a browser without WebCodecs, AirPlay): the Web Audio player takes them as
        // they are, Cast's media handling in a WAV header.
        formats: vec!["flc".into(), "pcm".into()],
        max_sample_rate: 96_000,
        accurate: true,
    };
    let server2 = server.clone();
    let name = c.name.clone();
    let video = c.video;
    let ip = c
        .host
        .parse()
        .ok()
        .or_else(|| tls.get_ref().0.peer_addr().ok().map(|a| a.ip()));
    devices::found(
        &server,
        Device {
            key: c.key,
            identity,
            ip,
        },
        Box::new(move |io| {
            Box::pin(async move {
                if let Err(e) = Cast::new(server2, mac, tls, io, video).run().await {
                    info!("{name}: {e}");
                }
            })
        }),
    );
}

async fn connect(host: &str, port: u16) -> std::io::Result<TlsStream<TcpStream>> {
    use rustls::pki_types::ServerName;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).map_err(std::io::Error::other)?;
    let sock = TcpStream::connect((host, port)).await?;
    sock.set_nodelay(true)?;
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, sock)
        .await
}

/// Takes any certificate: Cast devices present self-signed ones.  (Handshake signatures
/// are still checked, against the certificate given.)
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ---- the wire format -------------------------------------------------------------------

/// The protobuf `CastMessage` (protocol version CASTV2_1_0, string payloads only).
#[derive(Debug, Clone, Default, PartialEq)]
struct CastMessage {
    source: String,
    destination: String,
    namespace: String,
    payload: String,
}

impl CastMessage {
    /// The frame: a 4-byte big-endian length, then the protobuf.
    fn encode(&self) -> Vec<u8> {
        let mut pb = Vec::with_capacity(64 + self.payload.len());
        put_varint(&mut pb, 1 << 3); // protocol_version
        put_varint(&mut pb, 0);
        for (field, s) in [
            (2, &self.source),
            (3, &self.destination),
            (4, &self.namespace),
        ] {
            put_string(&mut pb, field, s);
        }
        put_varint(&mut pb, 5 << 3); // payload_type: STRING
        put_varint(&mut pb, 0);
        put_string(&mut pb, 6, &self.payload);
        let mut frame = (pb.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&pb);
        frame
    }

    /// Parse the protobuf (without the length).
    fn decode(mut b: &[u8]) -> Option<CastMessage> {
        let mut m = CastMessage::default();
        while !b.is_empty() {
            let key = get_varint(&mut b)?;
            match key & 7 {
                0 => {
                    get_varint(&mut b)?;
                }
                1 => b = b.get(8..)?,
                5 => b = b.get(4..)?,
                2 => {
                    let len = get_varint(&mut b)? as usize;
                    let bytes = b.get(..len)?;
                    b = &b[len..];
                    let s = || String::from_utf8_lossy(bytes).into_owned();
                    match key >> 3 {
                        2 => m.source = s(),
                        3 => m.destination = s(),
                        4 => m.namespace = s(),
                        6 => m.payload = s(),
                        _ => {} // payload_binary and anything newer
                    }
                }
                _ => return None,
            }
        }
        Some(m)
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_string(out: &mut Vec<u8>, field: u64, s: &str) {
    put_varint(out, field << 3 | 2);
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_varint(b: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = b.split_first()?;
        *b = rest;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Split one frame off `buf`; an error for a frame too big to be a Cast message.
fn next_message(buf: &mut BytesMut) -> anyhow::Result<Option<CastMessage>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    anyhow::ensure!(len <= 1 << 20, "a {len}-byte message");
    if buf.len() < 4 + len {
        return Ok(None);
    }
    buf.advance(4);
    let frame = buf.split_to(len);
    CastMessage::decode(&frame)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("a malformed message"))
}

// ---- the driver ------------------------------------------------------------------------

/// Something to play.
#[derive(Debug, Clone)]
struct Item {
    /// Absolute: the device fetches it.
    url: String,
    format: String,
    /// What raw samples (`pcm`) are like.
    pcm: Option<Pcm>,
    autoplay: bool,
    /// What the server is streaming (title, artist, artwork), shown on screens.
    metadata: Option<Value>,
    /// Its length from the library, for devices that don't report it.
    duration: Option<f64>,
    /// For RMS's receiver app: the visualiser settings.
    custom: Option<Value>,
}

/// The track loaded on the device.
#[derive(Debug)]
struct Media {
    item: Item,
    /// The `LOAD`'s requestId; its answer carries the media session.
    request: i64,
    session: Option<i64>,
    started: bool,
    paused: bool,
    ready: bool,
    /// Asked to play or pause before the media session was known.
    want_play: Option<bool>,
    duration: Option<f64>,
}

/// The media receiver app as it runs on the device.
#[derive(Debug, Clone, PartialEq)]
struct App {
    id: String,
    transport: String,
    session: String,
}

struct Cast {
    server: Arc<Server>,
    mac: MacAddress,
    tls: TlsStream<TcpStream>,
    io: DriverIo,
    next_request: i64,
    app: Option<App>,
    /// The receiver app to play in (RMS's, or the one set in the settings).
    want_app: String,
    /// What the receiver shows with the audio: `draw`, `video` or `off`.
    visual: &'static str,
    /// Messages for the receiver app, sent once the current status is handled.
    outbox: Vec<Value>,
    /// The last `PING`: its id and when it went (clock seconds).
    ping: Option<(i64, f64)>,
    /// The receiver answers `PING`s (so its own positions are used).
    pongs: bool,
    /// The receiver app plays on Web Audio (as its `HELLO` said; `None`: not yet asked).
    engine: Option<bool>,
    /// When the `HELLO` went, while its answer is awaited.
    hello_asked: Option<Instant>,
    /// The track plays on the receiver's Web Audio player (the server's messages go to it).
    engine_active: bool,
    /// How the receiver's clock lines up with ours.
    link: ClockLink,
    /// Since when the receiver's `ready` waits for the clocks to line up.
    ready_held: Option<Instant>,
    /// The device has said what runs on it (until then, nothing is launched).
    status_seen: bool,
    /// Another app was on it when it connected, before the player was there to turn off:
    /// why, to turn it off once it is (and launch nothing meanwhile).
    off_pending: Option<String>,
    /// Whether the receiver app is on screen (as it says): Google TV keeps it playing behind
    /// the home screen after Home is pressed.
    visible: Option<bool>,
    /// Since when the receiver app is being closed by the driver itself (the player turned
    /// off, or to launch it afresh in front): its going isn't someone taking the TV.
    closing: Option<Instant>,
    /// When a LAUNCH was sent and the app isn't there yet.
    launching: Option<Instant>,
    /// To load once the app runs.
    queued: Option<Item>,
    media: Option<Media>,
    /// To load when the current track finishes.
    next: Option<Item>,
    volume: Option<(u8, bool)>,
    /// What the device was last told.
    sent_volume: Option<(u8, bool)>,
    /// Playing the group's visualiser stream.
    viz: Option<Viz>,
    /// Where the device's playback is, on the clock the player reports.
    clock: CastClock,
    /// What the device takes from `PLAY` to playing (seconds), as last seen.
    start_latency: f64,
    /// A synced start: `PLAY` then.
    start_at: Option<tokio::time::Instant>,
    /// When (clock seconds) the `PLAY` of a synced start went, to see how long it took.
    play_sent: Option<f64>,
    /// A sync adjustment: playing at `rate` until then.
    adjusting: Option<tokio::time::Instant>,
    /// The playback rate the device was told.
    rate: f64,
    /// The receiver changes the playback rate (as its status says).
    rate_ok: bool,
    /// The last `GET_STATUS`: its requestId, and when it went (clock seconds).
    status_asked: Option<(i64, f64)>,
}

/// The visualiser stream the device plays.
struct Viz {
    session: Arc<crate::viz::Session>,
    /// This TV feeds the session the tracks (it started it).
    owner: bool,
    /// Names this TV's reader of the stream.
    token: String,
    /// The tracks as they came (loaded, then preloaded), with their length.
    songs: Vec<(u64, Option<f64>)>,
    /// Their Cast metadata, for the receiver's track info.
    meta: Vec<Option<Value>>,
    /// Index in `songs` of the track last reported started.
    playing: Option<usize>,
    /// How many of `songs` are all fetched (decoded into the stream, or read).
    fetched: Arc<std::sync::atomic::AtomicUsize>,
    /// How many of those the server was told about: a track's Fetched waits until it
    /// plays (the server only takes it from a playing track).
    told_fetched: usize,
}

impl Cast {
    fn new(
        server: Arc<Server>,
        mac: MacAddress,
        tls: TlsStream<TcpStream>,
        io: DriverIo,
        video: Option<bool>,
    ) -> Cast {
        crate::viz::note_screen(mac, video);
        Cast {
            server,
            mac,
            tls,
            io,
            next_request: 1,
            app: None,
            want_app: String::new(),
            visual: "off",
            outbox: Vec::new(),
            ping: None,
            pongs: false,
            engine: None,
            hello_asked: None,
            engine_active: false,
            ready_held: None,
            status_seen: false,
            off_pending: None,
            visible: None,
            closing: None,
            link: ClockLink::new(),
            launching: None,
            queued: None,
            media: None,
            next: None,
            volume: None,
            sent_volume: None,
            viz: None,
            clock: CastClock::new(),
            start_latency: START_LATENCY,
            start_at: None,
            play_sent: None,
            adjusting: None,
            rate: 1.0,
            rate_ok: false,
            status_asked: None,
        }
    }

    /// Relay until the device goes away or the bridge ends.
    async fn run(mut self) -> anyhow::Result<()> {
        // the player's clock, before anything else (see `super::run`)
        let jiffies = self.clock.jiffies(Instant::now());
        self.report(FromRenderer::Clock { jiffies }).await?;
        self.send(RECEIVER, NS_CONNECTION, json!({"type": "CONNECT"}))
            .await?;
        self.request(RECEIVER, NS_RECEIVER, json!({"type": "GET_STATUS"}))
            .await?;
        let mut buf = BytesMut::with_capacity(8192);
        let mut heard = Instant::now();
        let mut pinged = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let timer = [self.start_at, self.adjusting].into_iter().flatten().min();
            tokio::select! {
                _ = async {
                    match timer {
                        Some(t) => tokio::time::sleep_until(t).await,
                        None => std::future::pending().await,
                    }
                } => self.on_timer().await?,
                n = self.tls.read_buf(&mut buf) => {
                    anyhow::ensure!(n? > 0, "the device closed the connection");
                    heard = Instant::now();
                    while let Some(msg) = next_message(&mut buf)? {
                        self.on_message(msg).await?;
                    }
                }
                cmd = self.io.commands.recv() => {
                    // the bridge ended: the player is gone
                    let Some(cmd) = cmd else { return Ok(()) };
                    self.on_command(cmd).await?;
                }
                _ = tick.tick() => {
                    anyhow::ensure!(heard.elapsed() < SILENCE, "the device stopped answering");
                    if pinged.elapsed() >= PING_EVERY {
                        pinged = Instant::now();
                        self.send(RECEIVER, NS_HEARTBEAT, json!({"type": "PING"})).await?;
                    }
                    if self.launching.is_some_and(|t| t.elapsed() > LAUNCH_TIMEOUT) {
                        self.launching = None;
                        if self.queued.take().is_some() {
                            self.report(FromRenderer::Error {
                                message: "the media receiver did not start".into(),
                            })
                            .await?;
                        }
                    }
                    // the position, while playing
                    let playing = self
                        .media
                        .as_ref()
                        .filter(|m| m.started && !m.paused)
                        .and_then(|m| m.session);
                    if let Some(id) = playing {
                        let sent = self.clock.secs(Instant::now());
                        let request = self
                            .media_request(json!({"type": "GET_STATUS", "mediaSessionId": id}))
                            .await?;
                        self.status_asked = request.map(|r| (r, sent));
                    }
                    // the receiver app's clock (and, for Cast's media, the position heard)
                    self.ping().await?;
                    // the player turned off: the TV is free again, as a Squeezebox's speakers
                    if let Some(p) = self.server.player(&self.mac).filter(|p| !p.power()) {
                        if self.app.as_ref().is_some_and(|a| a.id == self.want_app) && self.closing.is_none() {
                            self.close_app("the player was turned off").await?;
                            forget_paused(&p);
                        }
                    }
                    if self.closing.is_some_and(|t| t.elapsed() > CLOSE_TIMEOUT) {
                        // it didn't close: play in it as it is, rather than not at all
                        self.closing = None;
                        self.visible = None;
                        if let Some(item) = self.queued.take() {
                            self.load_or_launch(item).await?;
                        }
                    }
                    if let Some(why) = self.off_pending.take() {
                        if !self.power_off(&why) {
                            self.off_pending = Some(why);
                        }
                    }
                    if self.ready_held.is_some_and(|t| t.elapsed() >= LINK_WAIT) {
                        self.ready_held = None;
                        self.report(FromRenderer::Ready).await?;
                    }
                    // an app that doesn't say what it can do has only Cast's media
                    if self.hello_asked.is_some_and(|t| t.elapsed() > HELLO_TIMEOUT) {
                        self.on_hello(&json!({"engine": false})).await?;
                    }
                }
            }
        }
    }

    async fn on_command(&mut self, cmd: ToRenderer) -> anyhow::Result<()> {
        debug!("chromecast {}: {cmd:?}", self.mac);
        // the receiver's Web Audio player takes the server's messages as they are
        if self.engine_active {
            match &cmd {
                ToRenderer::Preload { url, format, pcm } => {
                    let item = self.item(url, format, *pcm, true);
                    return self.engine_load(item, true).await;
                }
                ToRenderer::Pause
                | ToRenderer::Resume { .. }
                | ToRenderer::Hold { .. }
                | ToRenderer::Skip { .. } => return self.render_command(&cmd).await,
                ToRenderer::Stop => {
                    self.engine_active = false;
                    self.ready_held = None;
                    self.queued = None;
                    return self.render_command(&cmd).await;
                }
                // (a new track decides again; the volume is the device's)
                ToRenderer::Load { .. } | ToRenderer::Volume { .. } => {}
            }
        }
        match cmd {
            ToRenderer::Load {
                url,
                format,
                pcm,
                autostart,
            } => {
                self.next = None;
                self.start_at = None;
                self.leave_viz();
                let app = crate::viz::receiver_app(&self.server);
                if app != self.want_app {
                    self.launching = None;
                    self.want_app = app;
                }
                let mode = crate::viz::mode(&self.server, &self.mac);
                if mode == crate::viz::Mode::Video {
                    match self.viz_item(&wav_url(&url, pcm.as_ref()), autostart) {
                        Ok(item) => {
                            self.visual = "video";
                            return self.load_or_launch(item).await;
                        }
                        Err(e) => debug!("chromecast {}: no visualiser video: {e}", self.mac),
                    }
                }
                self.visual = match mode {
                    crate::viz::Mode::Tv => "draw",
                    _ => "off",
                };
                let item = self.item(&url, &format, pcm, autostart);
                self.load_or_launch(item).await?;
            }
            ToRenderer::Preload { url, format, pcm } => {
                if self.viz.is_some() {
                    self.viz_add(&wav_url(&url, pcm.as_ref()));
                } else {
                    self.next = Some(self.item(&url, &format, pcm, true));
                }
            }
            ToRenderer::Pause => {
                self.start_at = None;
                self.end_adjustment().await?;
                self.play(false).await?
            }
            ToRenderer::Resume { at: None } => {
                self.start_at = None;
                self.play(true).await?
            }
            // a synced start: as much before the moment as the device takes to start
            ToRenderer::Resume { at: Some(at) } => {
                let now = Instant::now();
                let moment = CastClock::instant_of(at, self.clock.jiffies(now), now);
                let play = moment
                    .checked_sub(Duration::from_secs_f64(self.start_latency))
                    .unwrap_or(now);
                if play <= now {
                    self.synced_play().await?;
                } else {
                    self.start_at = Some(tokio::time::Instant::from_std(play));
                }
            }
            ToRenderer::Hold { ms } => self.adjust(-f64::from(ms) / 1000.0).await?,
            ToRenderer::Skip { ms } => self.adjust(f64::from(ms) / 1000.0).await?,
            ToRenderer::Stop => {
                self.queued = None;
                self.next = None;
                self.start_at = None;
                self.adjusting = None;
                self.leave_viz();
                if let Some(id) = self.media.take().and_then(|m| m.session) {
                    self.media_request(json!({"type": "STOP", "mediaSessionId": id}))
                        .await?;
                }
            }
            ToRenderer::Volume { volume, muted } => {
                self.volume = Some((volume, muted));
                self.apply_volume().await?;
            }
        }
        Ok(())
    }

    /// Load `item`, launching the media receiver app first if it isn't running.
    async fn load_or_launch(&mut self, item: Item) -> anyhow::Result<()> {
        // behind Google TV's home screen, where launching it again doesn't bring it back
        // (nothing a sender asks does): closed and launched afresh, it comes up in front
        if self.visible == Some(false) && self.app.as_ref().is_some_and(|a| a.id == self.want_app) {
            self.queued = Some(item);
            return self
                .close_app("it's behind the home screen: closing it to start it afresh")
                .await;
        }
        if self.closing.is_some() {
            // (launched once it has closed)
            self.queued = Some(item);
            return Ok(());
        }
        if self.app.as_ref().is_some_and(|a| a.id == self.want_app) {
            if self.engine.is_none() {
                // what the app can do comes first
                self.queued = Some(item);
                return Ok(());
            }
            return self.load(item).await;
        }
        // (the other receiver app gives way to this one without a report)
        self.app = None;
        self.media = None;
        self.queued = Some(item);
        // not before the device has said what's on it: it may be busy with another app
        if !self.status_seen {
            return Ok(());
        }
        if self.off_pending.is_some() {
            self.queued = None;
            return Ok(());
        }
        self.launch().await
    }

    /// Close the receiver app (its going is then the driver's own doing).
    async fn close_app(&mut self, why: &str) -> anyhow::Result<()> {
        let Some(app) = self.app.clone().filter(|a| a.id == self.want_app) else {
            return Ok(());
        };
        if self.closing.is_some() {
            return Ok(());
        }
        info!("chromecast {}: closing the receiver: {why}", self.mac);
        self.closing = Some(Instant::now());
        self.request(
            RECEIVER,
            NS_RECEIVER,
            json!({"type": "STOP", "sessionId": app.session}),
        )
        .await?;
        Ok(())
    }

    /// Start the receiver app, for the queued track.
    async fn launch(&mut self) -> anyhow::Result<()> {
        // not for a player that's off (turned off as another app took the TV, with the
        // group's track already on its way): playing to it turns it on first
        if self.server.player(&self.mac).is_some_and(|p| !p.power()) {
            self.queued = None;
            return Ok(());
        }
        if self.launching.is_none() {
            self.launching = Some(Instant::now());
            let app = self.want_app.clone();
            self.request(
                RECEIVER,
                NS_RECEIVER,
                json!({"type": "LAUNCH", "appId": app}),
            )
            .await?;
        }
        Ok(())
    }

    /// The sync group (its master), and the id and length of the track about to stream.
    fn group_song(&self) -> Option<(String, u64, Option<f64>)> {
        let p = self.server.player(&self.mac)?;
        let (_, spec) = p.pending_stream()?;
        let key = p.controller().lock().master_id.to_string();
        let duration = super::now_streaming(&self.server, &self.mac, &self.io.base_url)
            .and_then(|t| t.duration);
        Some((key, spec.song_id, duration))
    }

    /// Start or join the group's visualiser stream with the track at `url` (the player's
    /// stream of it), and what to load for it.
    fn viz_item(&mut self, url: &str, autoplay: bool) -> Result<Item, String> {
        let (key, song, duration) = self.group_song().ok_or("no track to play")?;
        let (session, owner) = crate::viz::start_or_join(&self.server, &key, song)?;
        let token = self.mac.to_string().replace(':', "");
        info!(
            "chromecast {}: visualiser {} ({})",
            self.mac,
            session.id,
            if owner { "starts it" } else { "joins" }
        );
        let item = Item {
            url: format!("{}/viz/{}.mp4?r={token}", self.io.base_url, session.id),
            format: "mp4v".into(),
            pcm: None,
            autoplay,
            metadata: streaming(&self.server, &self.mac, &self.io.base_url).map(|(m, _)| m),
            duration: None,
            custom: Some(self.custom("video")),
        };
        self.viz = Some(Viz {
            session,
            owner,
            token,
            songs: Vec::new(),
            meta: Vec::new(),
            playing: None,
            fetched: Default::default(),
            told_fetched: 0,
        });
        self.viz_track(url, song, duration);
        Ok(item)
    }

    /// The next track (preloaded) joins the visualiser stream.
    fn viz_add(&mut self, url: &str) {
        let Some((_, song, duration)) = self.group_song() else {
            return;
        };
        self.viz_track(url, song, duration);
    }

    /// Note a track of the visualiser stream; the TV that feeds it decodes it into the
    /// stream, the others read their own stream of it (the group's one read of the track
    /// waits for every player).  Either says when it is all fetched, so the next can come.
    fn viz_track(&mut self, url: &str, song: u64, duration: Option<f64>) {
        let meta = streaming(&self.server, &self.mac, &self.io.base_url).map(|(m, _)| m);
        let Some(v) = self.viz.as_mut() else { return };
        v.songs.push((song, duration));
        v.meta.push(meta);
        let count = v.fetched.clone();
        let fetched = move || {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        };
        let url = format!("{}{}", self.io.base_url, url);
        if v.owner {
            v.session.feed(&self.server.http, song, url, fetched);
        } else {
            let http = self.server.http.clone();
            crate::util::spawn(async move {
                if let Ok(mut r) = http.get(&url).send().await {
                    while let Ok(Some(_)) = r.chunk().await {}
                }
                fetched();
            });
        }
    }

    /// Stop playing the visualiser stream (and stop it, when this TV feeds it).
    fn leave_viz(&mut self) {
        if let Some(v) = self.viz.take() {
            if v.owner {
                v.session.stop();
            }
        }
    }

    /// The device is `at` seconds into its visualiser stream: the reports for the track
    /// that is (a track started, how far into it).
    fn viz_progress(&mut self, at: f64, jiffies: u32, reports: &mut Vec<FromRenderer>) {
        // the song the sync group plays, as the server has it
        let group_song = self
            .server
            .player(&self.mac)
            .and_then(|p| p.controller().lock().playing_song().map(|s| s.id));
        let Some(v) = self.viz.as_mut() else { return };
        let t = at + v.session.joined_at(&v.token).unwrap_or(0.0);
        // the last track whose audio starts by now
        let Some((i, start)) = v
            .songs
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(i, (song, _))| v.session.start_of(*song).map(|s| (i, s)))
            .find(|(_, s)| *s <= t + 0.02)
        else {
            return;
        };
        let from = v.playing.map_or(0, |p| p + 1);
        for _ in from..=i {
            reports.push(FromRenderer::Started);
        }
        if from <= i {
            v.playing = Some(i);
            // the receiver shows the track that now plays in the stream
            if let Some(m) = v.meta.get(i).cloned().flatten() {
                let joined = v.session.joined_at(&v.token).unwrap_or(0.0);
                self.outbox.push(json!({
                    "type": "TRACK",
                    "title": m["title"],
                    "artist": m["artist"],
                    "album": m["albumName"],
                    "art": m["images"][0]["url"],
                    "duration": v.songs[i].1,
                    "start": (start - joined).max(0.0),
                }));
            }
        }
        reports.push(FromRenderer::Progress {
            elapsed: (t - start).max(0.0),
            duration: v.songs[i].1,
            jiffies: Some(jiffies),
        });
        // the playing track is all fetched: the next one can come.  Only once the group
        // plays it (its master's start counted): asked for earlier, by a TV of the group
        // ahead of the master, the server would find nothing more to stream.
        v.told_fetched = v.told_fetched.max(i);
        if v.told_fetched == i
            && v.fetched.load(std::sync::atomic::Ordering::SeqCst) > i
            && group_song == Some(v.songs[i].0)
        {
            v.told_fetched = i + 1;
            reports.push(FromRenderer::Fetched);
        }
    }

    async fn on_message(&mut self, msg: CastMessage) -> anyhow::Result<()> {
        let Ok(payload) = serde_json::from_str::<Value>(&msg.payload) else {
            return Ok(());
        };
        let kind = payload["type"].as_str().unwrap_or_default();
        match (msg.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                self.send(&msg.source, NS_HEARTBEAT, json!({"type": "PONG"}))
                    .await?;
            }
            (NS_CONNECTION, "CLOSE") => {
                if self.app.as_ref().is_some_and(|a| a.transport == msg.source) {
                    self.app_gone().await?;
                } else if msg.source == RECEIVER {
                    anyhow::bail!("the device closed the connection");
                }
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => self.on_receiver_status(&payload).await?,
            (NS_RECEIVER, "LAUNCH_ERROR") => {
                self.launching = None;
                if self.queued.take().is_some() {
                    // RMS's own app is published; another, while it isn't, starts only on
                    // devices added in the Cast developer console
                    let hint = if self.want_app == crate::viz::DEFAULT_RECEIVER_APP {
                        ""
                    } else {
                        ": is it published, or this device's serial number added in the Cast \
                         console?"
                    };
                    let message = format!(
                        "the device wouldn't start the receiver app {} ({}){hint}",
                        self.want_app, payload["reason"]
                    );
                    self.report(FromRenderer::Error { message }).await?;
                }
            }
            (NS_MEDIA, "MEDIA_STATUS") => self.on_media_status(&payload).await?,
            (NS_VIZ, "PONG") => {
                self.on_pong(&payload);
                // a few answers straight away, so the clocks line up before the first start
                if self.ping.is_none() && self.link.len() < LINK_FIRST {
                    self.ping().await?;
                } else if self.link.len() >= LINK_FIRST && self.ready_held.take().is_some() {
                    self.report(FromRenderer::Ready).await?;
                }
            }
            (NS_VIZ, "HELLO") => {
                self.visible = payload["visible"].as_bool();
                self.on_hello(&payload).await?
            }
            (NS_VIZ, "VISIBLE") => {
                self.visible = payload["visible"].as_bool();
                debug!(
                    "chromecast {}: receiver visible: {:?}",
                    self.mac, self.visible
                );
            }
            (NS_VIZ, "SLOW") => {
                let why = format!("{} fps at {}", payload["fps"], payload["size"]);
                self.too_slow(&why);
            }
            (NS_VIZ, "REPORT") => self.on_engine_report(&payload).await?,
            (NS_VIZ, "STATS") => info!(
                "chromecast {}: the TV draws at {} fps ({}), preset {}",
                self.mac, payload["fps"], payload["size"], payload["preset"]
            ),
            (NS_VIZ, "ERROR") => warn!(
                "chromecast {}: the TV's visualiser: {}",
                self.mac,
                payload["message"].as_str().unwrap_or_default()
            ),
            (NS_MEDIA, "LOAD_FAILED" | "LOAD_CANCELLED" | "INVALID_REQUEST") => {
                let ours = self
                    .media
                    .as_ref()
                    .is_some_and(|m| payload["requestId"].as_i64() == Some(m.request));
                if ours {
                    self.media = None;
                    let message = format!("the device could not play it ({kind})");
                    self.report(FromRenderer::Error { message }).await?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn on_receiver_status(&mut self, payload: &Value) -> anyhow::Result<()> {
        let apps = payload["status"]["applications"].as_array();
        // when it connects: an app of its own on it (Netflix, YouTube) isn't to be taken
        // over by a sync group that plays; the player is off until it's asked to play
        if !std::mem::replace(&mut self.status_seen, true) {
            let busy = apps
                .into_iter()
                .flatten()
                .find(|a| a["appId"] != self.want_app.as_str() && a["isIdleScreen"] != true);
            if let Some(other) = busy {
                let name = other["displayName"].as_str().unwrap_or("another app");
                self.queued = None;
                let why = format!("{name} is playing on it");
                if !self.power_off(&why) {
                    self.off_pending = Some(why);
                }
            } else if self.queued.is_some() && self.app.is_none() {
                self.launch().await?;
            }
        }
        let app = apps
            .into_iter()
            .flatten()
            .find(|a| a["appId"] == self.want_app.as_str())
            .and_then(|a| {
                Some(App {
                    id: self.want_app.clone(),
                    transport: a["transportId"].as_str()?.to_string(),
                    session: a["sessionId"].as_str().unwrap_or_default().to_string(),
                })
            });
        match app {
            Some(app) if self.app.as_ref() != Some(&app) => {
                debug!(
                    "chromecast {}: media receiver at {}",
                    self.mac, app.transport
                );
                self.send(&app.transport, NS_CONNECTION, json!({"type": "CONNECT"}))
                    .await?;
                let transport = app.transport.clone();
                self.app = Some(app);
                self.pongs = false;
                self.launching = None;
                self.sent_volume = None;
                self.engine = None;
                self.engine_active = false;
                self.ready_held = None;
                self.link.reset();
                self.apply_volume().await?;
                // what it can do (the queued track waits for the answer), and the time
                self.hello_asked = Some(Instant::now());
                self.send(&transport, NS_VIZ, json!({"type": "HELLO"}))
                    .await?;
                self.ping().await?;
            }
            Some(_) => {}
            None if self.app.is_some() => self.app_gone().await?,
            None => {}
        }
        Ok(())
    }

    /// Another app took over the device, or the receiver app was closed on it (it never
    /// closes it itself): someone is using the TV for something else.  The player turns
    /// off, as a Squeezebox would, so the queue doesn't go on to its next track and take
    /// the TV back; in a sync group the others play on.  Playing to it again turns it on.
    async fn app_gone(&mut self) -> anyhow::Result<()> {
        self.app = None;
        self.next = None;
        self.engine = None;
        self.hello_asked = None;
        self.ready_held = None;
        self.visible = None;
        let playing = std::mem::take(&mut self.engine_active) || self.media.take().is_some();
        self.launching = None;
        if self.closing.take().is_some() {
            // closed by the driver: launched afresh if there's a track for it
            if self.queued.is_some() {
                self.launch().await?;
            }
            return Ok(());
        }
        self.queued = None;
        if playing {
            self.power_off("another app took over, or the receiver was closed");
        }
        Ok(())
    }

    /// Turn the player off (see [`Self::app_gone`]); false while the server hasn't the player.
    fn power_off(&self, why: &str) -> bool {
        let Some(p) = self.server.player(&self.mac) else {
            return false;
        };
        if !p.power() {
            return true;
        }
        info!("chromecast {}: turned off: {why}", self.mac);
        self.server.set_power(&p, false, true);
        forget_paused(&p);
        true
    }

    async fn on_media_status(&mut self, payload: &Value) -> anyhow::Result<()> {
        let request = payload["requestId"].as_i64();
        // when the status was true: halfway through our GET_STATUS, or just now
        let now = Instant::now();
        let received = self.clock.secs(now);
        let sent = match self.status_asked {
            Some((id, at)) if request == Some(id) => at,
            _ => received - 0.02,
        };
        let jiffies = self.clock.jiffies(now);
        for st in payload["status"].as_array().into_iter().flatten() {
            let Some(m) = self.media.as_mut() else {
                return Ok(());
            };
            let session = st["mediaSessionId"].as_i64();
            if m.session.is_none() && request == Some(m.request) {
                m.session = session;
            }
            if m.session.is_none() || m.session != session {
                continue; // someone else's media, or the one this load replaced
            }
            if let Some(d) = st["media"]["duration"].as_f64().filter(|d| *d > 0.0) {
                m.duration = Some(d);
            }
            if let Some(c) = st["supportedMediaCommands"].as_u64() {
                self.rate_ok = c & PLAYBACK_RATE != 0;
            }
            // where it is, on the player's clock
            let state = st["playerState"].as_str().unwrap_or_default();
            let elapsed = st["currentTime"].as_f64().map(|pos| {
                let rate = match state {
                    "PLAYING" => st["playbackRate"].as_f64().unwrap_or(self.rate),
                    _ => 0.0,
                };
                // (the receiver's own answers are better, when it gives them)
                if !self.pongs {
                    self.clock.observe(pos, rate, sent, received);
                }
                self.clock.position(received).unwrap_or(pos)
            });
            if self.clock.samples() >= 3 {
                self.learn_start();
            }
            let Some(m) = self.media.as_mut() else {
                return Ok(());
            };
            let mut reports = Vec::new();
            let viz = self.viz.is_some();
            match st["playerState"].as_str().unwrap_or_default() {
                "PLAYING" => {
                    if !m.started {
                        m.started = true;
                        // (with the visualiser: once the stream reaches the track)
                        if !viz {
                            reports.push(FromRenderer::Started);
                        }
                    } else if m.paused {
                        reports.push(FromRenderer::Resumed);
                    }
                    m.paused = false;
                }
                "PAUSED" => {
                    if !m.started {
                        if !m.ready {
                            m.ready = true;
                            reports.push(FromRenderer::Ready);
                        }
                    } else if !m.paused {
                        m.paused = true;
                        reports.push(FromRenderer::Paused);
                    }
                }
                "IDLE" => {
                    match st["idleReason"].as_str().unwrap_or_default() {
                        "FINISHED" => return self.finished().await,
                        // stopped on the device (its remote, Google Home): as when another
                        // app takes over, not the end of the track
                        "CANCELLED" => {
                            self.media = None;
                            self.next = None;
                            if self.closing.is_none() {
                                self.power_off("stopped on the device");
                            }
                            return Ok(());
                        }
                        "ERROR" => {
                            self.media = None;
                            self.next = None;
                            let message = "the device could not play it".to_string();
                            return self.report(FromRenderer::Error { message }).await;
                        }
                        _ => {} // interrupted by the next LOAD, or not started yet
                    }
                }
                _ => {} // buffering
            }
            let started = m.started;
            let progress = (m.duration.or(m.item.duration), m.want_play.take(), m.paused);
            if let (true, Some(elapsed)) = (started, elapsed) {
                if viz {
                    // Started goes before the Resumed/Paused of this status
                    let mut v = Vec::new();
                    self.viz_progress(elapsed, jiffies, &mut v);
                    v.extend(reports);
                    reports = v;
                } else {
                    reports.push(FromRenderer::Progress {
                        elapsed,
                        duration: progress.0,
                        jiffies: Some(jiffies),
                    });
                }
            }
            // asked before the device said which media session it was
            if let Some(play) = progress.1 {
                if play == progress.2 || !started {
                    self.play(play).await?;
                }
            }
            for r in reports {
                self.report(r).await?;
            }
        }
        for msg in std::mem::take(&mut self.outbox) {
            if let Some(app) = self.app.clone() {
                self.send(&app.transport, NS_VIZ, msg).await?;
            }
        }
        Ok(())
    }

    /// The current track played to its end: on to the next one, if there is one.
    async fn finished(&mut self) -> anyhow::Result<()> {
        self.media = None;
        match self.next.take() {
            Some(mut item) => {
                // (read when the next track was asked for, the server may not have had it)
                if item.metadata.is_none() {
                    let now = streaming(&self.server, &self.mac, &self.io.base_url);
                    (item.metadata, item.duration) =
                        now.map_or((None, None), |(m, d)| (Some(m), d));
                }
                self.load(item).await
            }
            None => self.report(FromRenderer::Ended).await,
        }
    }

    async fn load(&mut self, item: Item) -> anyhow::Result<()> {
        let Some(app) = self.app.clone() else {
            return Ok(());
        };
        // automatic, now the receiver has said whether there's a screen (a device given
        // by address): draw on it, or show the cover
        let mut item = item;
        if item.format != "mp4v" && crate::viz::setting(&self.server, &self.mac).is_none() {
            let visual = match crate::viz::mode(&self.server, &self.mac) {
                crate::viz::Mode::Tv => "draw",
                _ => "off",
            };
            if visual != self.visual {
                self.visual = visual;
                item.custom = Some(self.custom(visual));
            }
        }
        // audio on the receiver's Web Audio player, where it has one
        if self.engine == Some(true) && item.format != "mp4v" {
            return self.engine_load(item, false).await;
        }
        if std::mem::take(&mut self.engine_active) {
            self.render(json!({"cmd": {"type": "stop"}})).await?;
        }
        // raw samples go in a WAV header, which can't say they're big-endian
        if item.pcm.is_some_and(|p| !p.little_endian) {
            let message = "Cast's media handling can't play big-endian raw samples".into();
            return self.report(FromRenderer::Error { message }).await;
        }
        item.url = wav_url(&item.url, item.pcm.as_ref());
        let content_type = content_type(&item.format);
        let mut media = json!({
            "contentId": item.url,
            "contentUrl": item.url,
            "contentType": content_type,
            "streamType": "BUFFERED",
        });
        if let Some(meta) = &item.metadata {
            media["metadata"] = meta.clone();
        }
        if let Some(d) = item.duration {
            media["duration"] = json!(d);
        }
        let mut load = json!({
            "type": "LOAD",
            "sessionId": app.session,
            "media": media,
            "autoplay": item.autoplay,
            "currentTime": 0,
        });
        if let Some(c) = &item.custom {
            load["customData"] = c.clone();
        }
        debug!("chromecast {}: load {}", self.mac, item.url);
        let request = self.request(&app.transport, NS_MEDIA, load).await?;
        // a new media session: from its start, at the normal rate
        self.clock.reset();
        self.rate = 1.0;
        self.adjusting = None;
        self.play_sent = None;
        self.media = Some(Media {
            item,
            request,
            session: None,
            started: false,
            paused: false,
            ready: false,
            want_play: None,
            duration: None,
        });
        Ok(())
    }

    async fn play(&mut self, play: bool) -> anyhow::Result<()> {
        let Some(m) = self.media.as_mut() else {
            return Ok(());
        };
        let Some(id) = m.session else {
            m.want_play = Some(play);
            return Ok(());
        };
        let kind = if play { "PLAY" } else { "PAUSE" };
        self.media_request(json!({"type": kind, "mediaSessionId": id}))
            .await?;
        Ok(())
    }

    /// The receiver's answer to a `PING`: the position being heard, true halfway between
    /// asking and hearing back.
    fn on_pong(&mut self, p: &Value) {
        let Some((id, sent)) = self.ping else { return };
        if p["id"].as_i64() != Some(id) {
            return;
        }
        self.ping = None;
        if let Some(v) = p["visible"].as_bool() {
            self.visible = Some(v);
        }
        let received = self.clock.secs(Instant::now());
        if let Some(t) = p["t"].as_f64() {
            self.link.add(sent, received, t);
        }
        if self.engine_active {
            return;
        }
        let Some(pos) = p["position"].as_f64() else {
            return;
        };
        let rate = match p["playing"].as_bool() {
            Some(true) => p["rate"].as_f64().unwrap_or(self.rate),
            _ => 0.0,
        };
        self.pongs = true;
        self.clock.observe(pos, rate, sent, received);
        if self.clock.samples() >= 3 {
            self.learn_start();
        }
    }

    /// The TV doesn't keep up drawing the visualiser: automatic shows something else from
    /// the next track.
    fn too_slow(&mut self, why: &str) {
        if crate::viz::setting(&self.server, &self.mac).is_some()
            || crate::viz::too_slow(&self.server, &self.mac)
        {
            return;
        }
        crate::viz::set_too_slow(&self.server, &self.mac);
        let instead = match crate::viz::config(&self.server) {
            Ok(_) => "the visualiser video from the server",
            Err(_) => "the cover",
        };
        info!(
            "chromecast {}: the TV can't keep up drawing the visualiser ({why}): {instead} from \
             the next track",
            self.mac
        );
    }

    /// Ask the receiver app the time.
    async fn ping(&mut self) -> anyhow::Result<()> {
        let Some(app) = self.app.clone() else {
            return Ok(());
        };
        let id = self.next_request;
        self.next_request += 1;
        let sent = self.clock.secs(Instant::now());
        self.send(&app.transport, NS_VIZ, json!({"type": "PING", "id": id}))
            .await?;
        self.ping = Some((id, sent));
        Ok(())
    }

    /// What the receiver app can do: play on Web Audio (protocol 3), or only Cast's media.
    async fn on_hello(&mut self, p: &Value) -> anyhow::Result<()> {
        if self.hello_asked.take().is_none() {
            return Ok(());
        }
        let engine = p["engine"] == true && p["version"].as_u64().is_some_and(|v| v >= 3);
        debug!(
            "chromecast {}: the receiver plays on {} (screen {}, WebGL 2 {}, {})",
            self.mac,
            if engine {
                "Web Audio"
            } else {
                "Cast's media element"
            },
            p["screen"],
            p["webgl2"],
            p["size"]
        );
        self.engine = Some(engine);
        // what discovery couldn't say (a device given by address)
        if let Some(screen) = p["screen"].as_bool() {
            crate::viz::note_screen(self.mac, Some(screen));
        }
        if p["screen"] == true && p["webgl2"] == false {
            self.too_slow("it has no WebGL 2");
        }
        if let Some(item) = self.queued.take() {
            self.load(item).await?;
        }
        Ok(())
    }

    /// A message for the receiver's Web Audio player.
    async fn render(&mut self, mut msg: Value) -> anyhow::Result<()> {
        let Some(app) = self.app.clone() else {
            return Ok(());
        };
        msg["type"] = json!("RENDER");
        self.send(&app.transport, NS_VIZ, msg).await
    }

    /// The server's command for the receiver's Web Audio player, its moment (our clock's
    /// jiffies) put on the receiver's clock.
    async fn render_command(&mut self, cmd: &ToRenderer) -> anyhow::Result<()> {
        let mut cmd = serde_json::to_value(cmd)?;
        if let Some(at) = cmd["at"].as_u64() {
            let now = Instant::now();
            let moment = CastClock::instant_of(at as u32, self.clock.jiffies(now), now);
            cmd["at"] = match self.link.offset() {
                Some(offset) => {
                    let ms = self.clock.secs(moment) * 1000.0 + offset;
                    json!((ms.round().max(0.0) as u64) as u32)
                }
                // (not lined up yet: now, and sync adjustments do the rest)
                None => Value::Null,
            };
        }
        self.render(json!({ "cmd": cmd })).await
    }

    /// What the receiver's Web Audio player says, its moments put on our clock.
    async fn on_engine_report(&mut self, p: &Value) -> anyhow::Result<()> {
        if !self.engine_active {
            return Ok(());
        }
        let Ok(mut msg) = serde_json::from_value::<FromRenderer>(p["msg"].clone()) else {
            return Ok(());
        };
        match &mut msg {
            // (the player's clock is ours)
            FromRenderer::Clock { .. } => return Ok(()),
            // a synced start is a moment of its clock: not before the clocks line up
            FromRenderer::Ready if self.link.len() < LINK_FIRST => {
                self.ready_held = Some(Instant::now());
                return Ok(());
            }
            FromRenderer::Progress { jiffies, .. } => {
                *jiffies = jiffies.zip(self.link.offset()).map(|(j, offset)| {
                    let ours = f64::from(j) - offset;
                    (ours.round().max(0.0) as u64) as u32
                });
                if jiffies.is_none() {
                    // not lined up yet: as of now
                    *jiffies = Some(self.clock.jiffies(Instant::now()));
                }
            }
            _ => {}
        }
        self.report(msg).await
    }

    /// Load `item` on the receiver's Web Audio player.
    async fn engine_load(&mut self, item: Item, preload: bool) -> anyhow::Result<()> {
        let track = track_info(&item.metadata, item.duration);
        let mut cmd = if preload {
            json!({"type": "preload", "url": item.url, "format": item.format})
        } else {
            json!({"type": "load", "url": item.url, "format": item.format, "autostart": item.autoplay})
        };
        if let Some(pcm) = item.pcm {
            cmd["pcm"] = json!(pcm);
        }
        let mut msg = json!({"cmd": cmd, "track": track});
        if !preload {
            self.engine_active = true;
            self.ready_held = None;
            self.media = None;
            if let Some(rms) = item.custom.as_ref().map(|c| c["rms"].clone()) {
                msg["visual"] = rms["visualizer"].clone();
                msg["rms"] = rms;
            }
        }
        self.render(msg).await
    }

    /// How long the device took to start after the `PLAY` of a synced start, for the next
    /// one (once its position has been seen).
    fn learn_start(&mut self) {
        let (Some(sent), Some(origin)) = (self.play_sent, self.clock.origin()) else {
            return;
        };
        self.play_sent = None;
        let took = origin - sent;
        if (0.0..2.0).contains(&took) {
            self.start_latency = (self.start_latency + took) / 2.0;
            debug!(
                "chromecast {}: started {:.0} ms after PLAY (now taken as {:.0} ms)",
                self.mac,
                took * 1000.0,
                self.start_latency * 1000.0
            );
        }
    }

    /// The `PLAY` of a synced start: how long the device then takes is learned.
    async fn synced_play(&mut self) -> anyhow::Result<()> {
        self.start_at = None;
        // (from the start of a track: a resume's position isn't where it started)
        if self.media.as_ref().is_some_and(|m| !m.started) && self.viz.is_none() {
            self.play_sent = Some(self.clock.secs(Instant::now()));
        }
        self.play(true).await
    }

    async fn on_timer(&mut self) -> anyhow::Result<()> {
        let now = tokio::time::Instant::now();
        if self.start_at.is_some_and(|t| t <= now) {
            self.synced_play().await?;
        }
        if self.adjusting.is_some_and(|t| t <= now) {
            self.end_adjustment().await?;
        }
        Ok(())
    }

    /// A sync adjustment: `secs` to make up (positive: this player is behind).  It plays
    /// a little faster or slower until then; the latest adjustment replaces one under way,
    /// since the server measures what is left of it.
    async fn adjust(&mut self, secs: f64) -> anyhow::Result<()> {
        let playing = self
            .media
            .as_ref()
            .is_some_and(|m| m.started && !m.paused && m.session.is_some());
        if !playing || secs.abs() < MIN_ADJUST {
            return Ok(());
        }
        if !self.rate_ok {
            debug!(
                "chromecast {}: can't change the playback rate: {:.0} ms out of sync",
                self.mac,
                secs * 1000.0
            );
            return Ok(());
        }
        // (before the rate changes the line the start is reckoned from)
        self.learn_start();
        let step = if secs.abs() > 1.0 { 0.10 } else { 0.05 };
        let rate = 1.0 + step * secs.signum();
        let over = secs.abs() / step;
        debug!(
            "chromecast {}: sync: {:+.0} ms at {rate} over {over:.1} s",
            self.mac,
            secs * 1000.0
        );
        self.set_rate(rate).await?;
        self.adjusting = Some(tokio::time::Instant::now() + Duration::from_secs_f64(over));
        Ok(())
    }

    async fn end_adjustment(&mut self) -> anyhow::Result<()> {
        if self.adjusting.take().is_some() || self.rate != 1.0 {
            self.set_rate(1.0).await?;
        }
        Ok(())
    }

    async fn set_rate(&mut self, rate: f64) -> anyhow::Result<()> {
        let Some(id) = self.media.as_ref().and_then(|m| m.session) else {
            return Ok(());
        };
        self.rate = rate;
        let now = self.clock.secs(Instant::now());
        self.clock.set_rate(rate, now);
        self.media_request(
            json!({"type": "SET_PLAYBACK_RATE", "mediaSessionId": id, "playbackRate": rate}),
        )
        .await?;
        Ok(())
    }

    async fn apply_volume(&mut self) -> anyhow::Result<()> {
        // the device's volume is left alone until it plays for us
        if self.app.is_none() || self.volume == self.sent_volume {
            return Ok(());
        }
        let Some((volume, muted)) = self.volume else {
            return Ok(());
        };
        let level = f64::from(volume) / 100.0;
        // one change per message: devices refuse level and muted together
        if self.sent_volume.map(|v| v.0) != Some(volume) {
            self.request(
                RECEIVER,
                NS_RECEIVER,
                json!({"type": "SET_VOLUME", "volume": {"level": level}}),
            )
            .await?;
        }
        if self.sent_volume.map(|v| v.1) != Some(muted) {
            self.request(
                RECEIVER,
                NS_RECEIVER,
                json!({"type": "SET_VOLUME", "volume": {"muted": muted}}),
            )
            .await?;
        }
        self.sent_volume = self.volume;
        Ok(())
    }

    /// What to load for a stream URL the bridge gave.
    fn item(&self, url: &str, format: &str, pcm: Option<Pcm>, autoplay: bool) -> Item {
        let (metadata, duration) = streaming(&self.server, &self.mac, &self.io.base_url)
            .map_or((None, None), |(m, d)| (Some(m), d));
        Item {
            url: format!("{}{}", self.io.base_url, url),
            format: format.to_string(),
            pcm,
            autoplay,
            metadata,
            duration,
            custom: Some(self.custom(self.visual)),
        }
    }

    /// What the receiver app is told with a track: the server's visualiser settings and
    /// what to show (`visual`).
    fn custom(&self, visual: &str) -> Value {
        let mut rms = crate::viz::receiver_settings(&self.server);
        rms["visualizer"] = json!(visual);
        json!({ "rms": rms })
    }

    async fn report(&mut self, r: FromRenderer) -> anyhow::Result<()> {
        self.io
            .reports
            .send(r)
            .await
            .map_err(|_| anyhow::anyhow!("the player is gone"))
    }

    /// A request to the media session's app, if it runs; returns its requestId.
    async fn media_request(&mut self, payload: Value) -> anyhow::Result<Option<i64>> {
        let Some(app) = self.app.clone() else {
            return Ok(None);
        };
        Ok(Some(self.request(&app.transport, NS_MEDIA, payload).await?))
    }

    /// Send a request (with a new requestId, which it returns).
    async fn request(&mut self, to: &str, ns: &str, mut payload: Value) -> anyhow::Result<i64> {
        let id = self.next_request;
        self.next_request += 1;
        payload["requestId"] = json!(id);
        self.send(to, ns, payload).await?;
        Ok(id)
    }

    async fn send(&mut self, to: &str, ns: &str, payload: Value) -> anyhow::Result<()> {
        let msg = CastMessage {
            source: SENDER.into(),
            destination: to.into(),
            namespace: ns.into(),
            payload: payload.to_string(),
        };
        self.tls.write_all(&msg.encode()).await?;
        self.tls.flush().await?;
        Ok(())
    }
}

/// The Cast metadata (a music track) of what the server is about to stream to the
/// player, and its length from where the stream starts.
fn streaming(server: &Server, mac: &MacAddress, base_url: &str) -> Option<(Value, Option<f64>)> {
    let t = super::now_streaming(server, mac, base_url)?;
    let mut meta = json!({"metadataType": 3, "title": t.title});
    if let Some(a) = &t.artist {
        meta["artist"] = json!(a);
    }
    if let Some(a) = &t.album {
        meta["albumName"] = json!(a);
    }
    if let Some(n) = t.track_number {
        meta["trackNumber"] = json!(n);
    }
    if let Some(art) = &t.artwork {
        meta["images"] = json!([{ "url": art }]);
    }
    Some((meta, t.duration))
}

/// The track info the receiver shows, from a track's Cast metadata.
fn track_info(meta: &Option<Value>, duration: Option<f64>) -> Value {
    let m = meta.clone().unwrap_or(Value::Null);
    json!({
        "title": m["title"],
        "artist": m["artist"],
        "album": m["albumName"],
        "art": m["images"][0]["url"],
        "duration": duration,
    })
}

/// What a player that's off on its own was paused at is gone from the TV with the receiver:
/// playing again starts the track afresh, rather than resuming nothing.
fn forget_paused(p: &Arc<crate::player::Player>) {
    let ctrl = p.controller();
    let mut c = ctrl.lock();
    let alone = c.all_players().iter().all(|o| o.id == p.id || !o.power());
    if alone && c.is_paused() {
        c.stop();
    }
}

/// The stream at `url`, for a device that can't take raw samples (`pcm`) bare: in a WAV
/// header.
fn wav_url(url: &str, pcm: Option<&Pcm>) -> String {
    match pcm {
        Some(p) => format!("{url}&wav={},{},{}", p.rate, p.bits, p.channels),
        None => url.to_string(),
    }
}

/// The MIME type a Cast receiver expects for an LMS format code.
fn content_type(format: &str) -> &'static str {
    match format {
        "flc" => "audio/flac",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "ops" => "audio/ogg; codecs=opus",
        "pcm" => "audio/wav",
        // the visualiser's video stream
        "mp4v" => "video/mp4",
        _ => "audio/mpeg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_messages_round_trip() {
        let m = CastMessage {
            source: SENDER.into(),
            destination: RECEIVER.into(),
            namespace: NS_RECEIVER.into(),
            payload:
                json!({"type": "LAUNCH", "appId": "CC1AD845", "requestId": 1, "x": "é".repeat(100)})
                    .to_string(),
        };
        let frame = m.encode();
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        // version and payload type come first, as the devices expect
        assert_eq!(&frame[4..6], &[0x08, 0x00]);
        let mut buf = BytesMut::from(&frame[..frame.len() - 1]);
        assert_eq!(next_message(&mut buf).unwrap(), None);
        buf.extend_from_slice(&frame[frame.len() - 1..]);
        buf.extend_from_slice(&frame);
        assert_eq!(next_message(&mut buf).unwrap(), Some(m.clone()));
        assert_eq!(next_message(&mut buf).unwrap(), Some(m));
        assert!(buf.is_empty());
        assert!(CastMessage::decode(&[0x12, 0x05, b'a']).is_none());
    }

    #[test]
    fn varints() {
        for v in [0, 1, 127, 128, 300, 1 << 20, u64::MAX] {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            let mut s = &b[..];
            assert_eq!(get_varint(&mut s), Some(v));
            assert!(s.is_empty());
        }
    }

    #[test]
    fn hosts() {
        let h = |s: &str| parse_host(s);
        assert_eq!(h("10.0.0.5"), Some(("10.0.0.5".into(), 8009)));
        assert_eq!(h(" 10.0.0.5:8010 "), Some(("10.0.0.5".into(), 8010)));
        assert_eq!(h("kitchen.local"), Some(("kitchen.local".into(), 8009)));
        assert_eq!(h("kitchen.local:9"), Some(("kitchen.local".into(), 9)));
        assert_eq!(h("[::1]:8009"), Some(("::1".into(), 8009)));
        assert_eq!(h("fe80::1"), Some(("fe80::1".into(), 8009)));
        assert_eq!(h("host:x"), None);
        assert_eq!(h(""), None);
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("flc"), "audio/flac");
        assert_eq!(content_type("mp3"), "audio/mpeg");
    }
}
