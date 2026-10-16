//! Chromecast (Google Cast) devices as players.
//!
//! Devices are found with mDNS (`_googlecast._tcp`) and from the `chromecastHosts` pref
//! (`host[:port]`, for networks where multicast doesn't reach).  Each one is driven with the
//! Cast v2 protocol: JSON messages wrapped in a small protobuf (`CastMessage`), framed by a
//! 4-byte length, over TLS to port 8009.  The device's certificate is self-signed, so it is
//! not checked — the connection only carries play commands and URLs of the server's own
//! streams.
//!
//! On the first track the driver launches the Default Media Receiver, an app every Cast
//! device has, and tells it to `LOAD` the stream URL; the device fetches the audio from
//! the server and reports its state in `MEDIA_STATUS` messages, which become the bridge's
//! [`FromRenderer`] reports.  The next track is loaded when the current one finishes.
//!
//! With the player's visualiser on (`visualizer`; when unset, on for devices with a screen)
//! and the `pmviz` helper installed, the device plays its sync group's
//! visualiser stream instead ([`crate::viz`]): video with the music in it, the tracks one
//! after another in one stream.  The TV that starts it feeds it the tracks; the group's
//! other visualiser TVs join it.  Where each track starts in the stream turns the device's
//! position into the track it plays and how far into it.

use super::devices::{self, Device, DriverIo, Protocol};
use super::{mac_for, FromRenderer, Identity, ToRenderer};
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
/// The Default Media Receiver.
const APP_ID: &str = "CC1AD845";

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
        formats: vec!["flc".into(), "mp3".into(), "aac".into()],
        max_sample_rate: 96_000,
        accurate: false,
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
    autoplay: bool,
    /// What the server is streaming (title, artist, artwork), shown on screens.
    metadata: Option<Value>,
    /// Its length from the library, for devices that don't report it.
    duration: Option<f64>,
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

/// The Default Media Receiver as it runs on the device.
#[derive(Debug, Clone, PartialEq)]
struct App {
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
    /// The device has a screen (unknown: given by address).
    video: Option<bool>,
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
        Cast {
            video,
            server,
            mac,
            tls,
            io,
            next_request: 1,
            app: None,
            launching: None,
            queued: None,
            media: None,
            next: None,
            volume: None,
            sent_volume: None,
            viz: None,
        }
    }

    /// Relay until the device goes away or the bridge ends.
    async fn run(mut self) -> anyhow::Result<()> {
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
            tokio::select! {
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
                        self.media_request(json!({"type": "GET_STATUS", "mediaSessionId": id}))
                            .await?;
                    }
                }
            }
        }
    }

    async fn on_command(&mut self, cmd: ToRenderer) -> anyhow::Result<()> {
        debug!("chromecast {}: {cmd:?}", self.mac);
        match cmd {
            ToRenderer::Load {
                url,
                format,
                autostart,
                ..
            } => {
                self.next = None;
                self.leave_viz();
                if self.visualizer_on() {
                    match self.viz_item(&url, autostart) {
                        Ok(item) => return self.load_or_launch(item).await,
                        Err(e) => debug!("chromecast {}: no visualiser: {e}", self.mac),
                    }
                }
                let item = self.item(&url, &format, autostart);
                self.load_or_launch(item).await?;
            }
            ToRenderer::Preload { url, format, .. } => {
                if self.viz.is_some() {
                    self.viz_add(&url);
                } else {
                    self.next = Some(self.item(&url, &format, true));
                }
            }
            ToRenderer::Pause => self.play(false).await?,
            ToRenderer::Resume { .. } => self.play(true).await?,
            ToRenderer::Stop => {
                self.queued = None;
                self.next = None;
                self.leave_viz();
                if let Some(id) = self.media.take().and_then(|m| m.session) {
                    self.media_request(json!({"type": "STOP", "mediaSessionId": id}))
                        .await?;
                }
            }
            // (only for renderers with their own clock)
            ToRenderer::Hold { .. } | ToRenderer::Skip { .. } => {}
            ToRenderer::Volume { volume, muted } => {
                self.volume = Some((volume, muted));
                self.apply_volume().await?;
            }
        }
        Ok(())
    }

    /// Load `item`, launching the media receiver first if it isn't running.
    async fn load_or_launch(&mut self, item: Item) -> anyhow::Result<()> {
        if self.app.is_some() {
            return self.load(item).await;
        }
        self.media = None;
        self.queued = Some(item);
        if self.launching.is_none() {
            self.launching = Some(Instant::now());
            self.request(
                RECEIVER,
                NS_RECEIVER,
                json!({"type": "LAUNCH", "appId": APP_ID}),
            )
            .await?;
        }
        Ok(())
    }

    /// The player's `visualizer` setting; when unset, on for devices with a screen (used
    /// where pmviz is installed; plain audio plays otherwise).
    fn visualizer_on(&self) -> bool {
        let prefs = self.server.prefs.player(&self.mac.to_string());
        if prefs.exists("visualizer") {
            prefs.get_bool("visualizer")
        } else {
            self.video == Some(true)
        }
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
            autoplay,
            metadata: streaming(&self.server, &self.mac, &self.io.base_url).map(|(m, _)| m),
            duration: None,
        };
        self.viz = Some(Viz {
            session,
            owner,
            token,
            songs: Vec::new(),
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
        let Some(v) = self.viz.as_mut() else { return };
        v.songs.push((song, duration));
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
    fn viz_progress(&mut self, at: f64, reports: &mut Vec<FromRenderer>) {
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
        }
        reports.push(FromRenderer::Progress {
            elapsed: (t - start).max(0.0),
            duration: v.songs[i].1,
            jiffies: None,
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
                    let message =
                        format!("could not start the media receiver: {}", payload["reason"]);
                    self.report(FromRenderer::Error { message }).await?;
                }
            }
            (NS_MEDIA, "MEDIA_STATUS") => self.on_media_status(&payload).await?,
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
        let app = payload["status"]["applications"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["appId"] == APP_ID)
            .and_then(|a| {
                Some(App {
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
                self.app = Some(app);
                self.launching = None;
                self.sent_volume = None;
                self.apply_volume().await?;
                if let Some(item) = self.queued.take() {
                    self.load(item).await?;
                }
            }
            Some(_) => {}
            None if self.app.is_some() => self.app_gone().await?,
            None => {}
        }
        Ok(())
    }

    /// Another app took over the device, or the media receiver quit.
    async fn app_gone(&mut self) -> anyhow::Result<()> {
        self.app = None;
        self.next = None;
        if self.media.take().is_some_and(|m| m.started) {
            self.report(FromRenderer::Ended).await?;
        }
        Ok(())
    }

    async fn on_media_status(&mut self, payload: &Value) -> anyhow::Result<()> {
        let request = payload["requestId"].as_i64();
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
            let elapsed = st["currentTime"].as_f64();
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
                        "CANCELLED" => {
                            self.media = None;
                            self.next = None;
                            return self.report(FromRenderer::Ended).await;
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
                    self.viz_progress(elapsed, &mut v);
                    v.extend(reports);
                    reports = v;
                } else {
                    reports.push(FromRenderer::Progress {
                        elapsed,
                        duration: progress.0,
                        jiffies: None,
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
        Ok(())
    }

    /// The current track played to its end: on to the next one, if there is one.
    async fn finished(&mut self) -> anyhow::Result<()> {
        self.media = None;
        match self.next.take() {
            Some(item) => self.load(item).await,
            None => self.report(FromRenderer::Ended).await,
        }
    }

    async fn load(&mut self, item: Item) -> anyhow::Result<()> {
        let Some(app) = self.app.clone() else {
            return Ok(());
        };
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
        debug!("chromecast {}: load {}", self.mac, item.url);
        let request = self
            .request(
                &app.transport,
                NS_MEDIA,
                json!({
                    "type": "LOAD",
                    "sessionId": app.session,
                    "media": media,
                    "autoplay": item.autoplay,
                    "currentTime": 0,
                }),
            )
            .await?;
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
            .await
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
    fn item(&self, url: &str, format: &str, autoplay: bool) -> Item {
        let (metadata, duration) = streaming(&self.server, &self.mac, &self.io.base_url)
            .map_or((None, None), |(m, d)| (Some(m), d));
        Item {
            url: format!("{}{}", self.io.base_url, url),
            format: format.to_string(),
            autoplay,
            metadata,
            duration,
        }
    }

    async fn report(&mut self, r: FromRenderer) -> anyhow::Result<()> {
        self.io
            .reports
            .send(r)
            .await
            .map_err(|_| anyhow::anyhow!("the player is gone"))
    }

    async fn media_request(&mut self, payload: Value) -> anyhow::Result<()> {
        if let Some(app) = self.app.clone() {
            self.request(&app.transport, NS_MEDIA, payload).await?;
        }
        Ok(())
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
                json!({"type": "LAUNCH", "appId": APP_ID, "requestId": 1, "x": "é".repeat(100)})
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
