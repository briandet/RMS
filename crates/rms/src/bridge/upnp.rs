//! UPnP / DLNA media renderers as players.
//!
//! Discovery: an SSDP search for `MediaRenderer` devices every minute while the protocol
//! is turned on (pref `bridgeUpnp`), plus the description URLs listed in `upnpRenderers`
//! for networks where multicast doesn't reach.  Each renderer's description gives its
//! name and the control URLs of its services; its ConnectionManager says which formats
//! it plays (FLAC and MP3 are the ones offered).
//!
//! The driver plays what the bridge asks for with AVTransport SOAP calls
//! (`SetAVTransportURI` + `Play`, `SetNextAVTransportURI` for the next track, `Pause`,
//! `Stop`) and RenderingControl for the volume.  UPnP has no push notifications we can
//! rely on without running an event server, so the driver polls the transport state and
//! position every second and turns what changed into reports: a track started, the
//! position, paused or resumed (also from the device's own buttons), the end of the music.

use super::devices::{self, Device, DriverIo, Protocol};
use super::{mac_for, FromRenderer, Identity, ToRenderer};
use crate::server::Server;
use quick_xml::escape::escape;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

const MEDIA_RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const SSDP_ADDR: &str = "239.255.255.250:1900";
/// How long SSDP answers are collected.
const SEARCH_FOR: Duration = Duration::from_secs(3);
/// How often devices are looked for.
const SEARCH_EVERY: Duration = Duration::from_secs(60);
/// How often a playing renderer is asked what it is doing.  Not a whole second: positions
/// come in whole seconds, and polls that fall at different points of the renderer's seconds
/// tell where in a second it is (see [`super::PositionClock`]).
const POLL_EVERY: Duration = Duration::from_millis(750);
/// A renderer that doesn't answer for this long is gone.
const GIVE_UP_AFTER: Duration = Duration::from_secs(30);
/// A track that hasn't started this long after `Play` failed.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout of one request to a renderer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Find renderers while the protocol is turned on and run a player for each.
pub async fn run(server: Arc<Server>) {
    let protocol = Protocol::by_prefix("upnp");
    // description URL → the device's key, so running devices aren't described again
    let mut known: HashMap<String, String> = HashMap::new();
    // when devices were last looked for, and the configured list then
    let mut last: Option<(Instant, Vec<String>)> = None;
    loop {
        if protocol.enabled(&server) {
            let configured = protocol.configured(&server);
            let due = match &last {
                None => true,
                Some((at, list)) => at.elapsed() >= SEARCH_EVERY || *list != configured,
            };
            if due {
                last = Some((Instant::now(), configured.clone()));
                for location in &configured {
                    add(&server, location, true, &mut known).await;
                }
                for location in search().await {
                    if !configured.contains(&location) {
                        add(&server, &location, false, &mut known).await;
                    }
                }
            }
        } else {
            last = None;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// SSDP: the description URLs of the media renderers that answer a search.
async fn search() -> Vec<String> {
    let sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            debug!("ssdp: {e}");
            return Vec::new();
        }
    };
    let _ = sock.set_multicast_ttl_v4(4);
    let request = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {MEDIA_RENDERER}\r\n\r\n"
    );
    // twice, as UDP may lose one
    for _ in 0..2 {
        if let Err(e) = sock.send_to(request.as_bytes(), SSDP_ADDR).await {
            debug!("ssdp: {e}");
            return Vec::new();
        }
    }
    let deadline = tokio::time::Instant::now() + SEARCH_FOR;
    let mut found = Vec::new();
    let mut buf = [0u8; 2048];
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        let answer = String::from_utf8_lossy(&buf[..n]);
        if let Some(location) = header(&answer, "location") {
            if !found.contains(&location) {
                found.push(location);
            }
        }
    }
    found
}

/// A header of an SSDP message.
fn header(message: &str, name: &str) -> Option<String> {
    message.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

/// Describe the device at `location` and run a player for it, unless it runs already.
async fn add(
    server: &Arc<Server>,
    location: &str,
    configured: bool,
    known: &mut HashMap<String, String>,
) {
    if known
        .get(location)
        .is_some_and(|key| devices::is_running(key))
    {
        return;
    }
    let base_url = devices::base_url(server);
    if same_host(location, &base_url) {
        debug!("upnp: {location} is this server");
        return;
    }
    let renderer = match describe(&server.http, location).await {
        Ok(r) => r,
        Err(e) if configured => {
            warn!("upnp renderer {location}: {e}");
            return;
        }
        Err(e) => {
            debug!("upnp: {location}: {e}");
            return;
        }
    };
    let key = format!("upnp:{}", renderer.udn);
    known.insert(location.to_string(), key.clone());
    if devices::is_running(&key) {
        return;
    }
    let identity = Identity {
        mac: mac_for(&key),
        name: renderer.name.clone(),
        model: "upnp".into(),
        model_name: renderer.model_name.clone(),
        formats: renderer
            .formats
            .iter()
            .map(|(code, _)| code.clone())
            .collect(),
        max_sample_rate: 96_000,
        accurate: false,
    };
    let http = server.http.clone();
    let ip = url::Url::parse(location)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_string()))
        .and_then(|h| h.parse().ok());
    devices::found(
        server,
        Device { key, identity, ip },
        Box::new(move |io| Box::pin(drive(http, renderer, io))),
    );
}

/// Whether two URLs name the same host and port.
fn same_host(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => {
            a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

/// A UPnP service of a device: its type (`urn:schemas-upnp-org:service:AVTransport:1`)
/// and control URL.
#[derive(Debug, Clone, PartialEq)]
struct Service {
    kind: String,
    control: String,
}

/// What a renderer's description and ConnectionManager say about it.
#[derive(Debug, Clone)]
struct Renderer {
    udn: String,
    name: String,
    model_name: String,
    av_transport: Service,
    rendering_control: Option<Service>,
    connection_manager: Option<Service>,
    /// Formats it plays, best first: (LMS code, the MIME type it gave for it).
    formats: Vec<(String, String)>,
}

/// Fetch and read a renderer's description, and ask it what it plays.
async fn describe(http: &reqwest::Client, location: &str) -> anyhow::Result<Renderer> {
    let xml = http
        .get(location)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let mut renderer = parse_description(&xml, location)?;
    let Some(cm) = &renderer.connection_manager else {
        anyhow::bail!("no ConnectionManager service");
    };
    let info = soap(http, cm, "GetProtocolInfo", &[])
        .await
        .map_err(|e| anyhow::anyhow!("GetProtocolInfo: {e}"))?;
    renderer.formats = formats_of(info.get("Sink").map(String::as_str).unwrap_or(""));
    if renderer.formats.is_empty() {
        anyhow::bail!("plays neither FLAC nor MP3");
    }
    Ok(renderer)
}

/// Read a device description: the media renderer in it (the root device or one embedded
/// in it) and its services' control URLs, made absolute.
fn parse_description(xml: &str, location: &str) -> anyhow::Result<Renderer> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let named = |node: roxmltree::Node, name: &str| -> Option<String> {
        node.children()
            .find(|c| c.is_element() && c.tag_name().name() == name)
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
    };
    let base = named(root, "URLBase")
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| location.to_string());
    let base = url::Url::parse(&base).or_else(|_| url::Url::parse(location))?;
    let devices: Vec<_> = root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "device")
        .collect();
    let device = devices
        .iter()
        .find(|d| named(**d, "deviceType").is_some_and(|t| t.contains(":MediaRenderer:")))
        .or(devices.first())
        .copied()
        .ok_or_else(|| anyhow::anyhow!("no device in the description"))?;
    let udn = named(device, "UDN").filter(|u| !u.is_empty());
    let udn = udn.ok_or_else(|| anyhow::anyhow!("the device has no UDN"))?;

    // the device's own services, not those of devices embedded in it
    let services: Vec<Service> = device
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "serviceList")
        .flat_map(|list| list.children().filter(|s| s.is_element()))
        .filter_map(|s| {
            let kind = named(s, "serviceType")?;
            let control = base.join(&named(s, "controlURL")?).ok()?.to_string();
            Some(Service { kind, control })
        })
        .collect();
    let service = |name: &str| {
        let pattern = format!(":service:{name}:");
        services.iter().find(|s| s.kind.contains(&pattern)).cloned()
    };
    let av_transport =
        service("AVTransport").ok_or_else(|| anyhow::anyhow!("no AVTransport service"))?;
    let name = named(device, "friendlyName")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "UPnP renderer".into());
    let model_name = named(device, "modelName")
        .filter(|n| !n.is_empty())
        .or_else(|| named(device, "manufacturer").filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "UPnP renderer".into());
    Ok(Renderer {
        udn,
        name,
        model_name,
        av_transport,
        rendering_control: service("RenderingControl"),
        connection_manager: service("ConnectionManager"),
        formats: Vec::new(),
    })
}

/// The formats offered to a renderer, from the protocols its ConnectionManager can sink
/// (`http-get:*:audio/flac:*,http-get:*:audio/mpeg:DLNA.ORG_PN=MP3,…`).
fn formats_of(sink: &str) -> Vec<(String, String)> {
    let mimes: Vec<String> = sink
        .split(',')
        .filter_map(|p| p.trim().split(':').nth(2))
        .map(|m| m.trim().to_ascii_lowercase())
        .collect();
    let mut formats = Vec::new();
    if let Some(m) = mimes
        .iter()
        .find(|m| *m == "audio/flac" || *m == "audio/x-flac")
    {
        formats.push(("flc".to_string(), m.clone()));
    }
    if mimes.iter().any(|m| m == "audio/mpeg") {
        formats.push(("mp3".to_string(), "audio/mpeg".to_string()));
    }
    formats
}

/// Why a SOAP call failed.
#[derive(Debug)]
enum SoapError {
    /// No answer: the device is unreachable or broken.
    Unreachable(String),
    /// The device answered with a fault.
    Fault(String),
}

impl fmt::Display for SoapError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SoapError::Unreachable(e) => write!(f, "no answer: {e}"),
            SoapError::Fault(e) => write!(f, "{e}"),
        }
    }
}

/// The body of a SOAP request.  Argument values are XML-escaped.
fn soap_body(service: &str, action: &str, args: &[(&str, &str)]) -> String {
    let mut body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"{}\">",
        escape(service)
    );
    for (name, value) in args {
        body.push_str(&format!("<{name}>{}</{name}>", escape(*value)));
    }
    body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
    body
}

/// Call `action` on `service`; returns the response's arguments.
async fn soap(
    http: &reqwest::Client,
    service: &Service,
    action: &str,
    args: &[(&str, &str)],
) -> Result<HashMap<String, String>, SoapError> {
    let response = http
        .post(&service.control)
        .timeout(REQUEST_TIMEOUT)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPAction", format!("\"{}#{action}\"", service.kind))
        .body(soap_body(&service.kind, action, args))
        .send()
        .await
        .map_err(|e| SoapError::Unreachable(e.to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| SoapError::Unreachable(e.to_string()))?;
    let doc = roxmltree::Document::parse(&text);
    if !status.is_success() {
        let detail = doc.ok().and_then(|doc| {
            let field = |name: &str| {
                doc.descendants()
                    .find(|n| n.tag_name().name() == name)
                    .and_then(|n| n.text())
                    .map(|t| t.trim().to_string())
            };
            let code = field("errorCode")?;
            Some(match field("errorDescription") {
                Some(d) => format!("{d} ({code})"),
                None => format!("error {code}"),
            })
        });
        return Err(SoapError::Fault(format!(
            "{action}: {}",
            detail.unwrap_or_else(|| format!("HTTP {status}"))
        )));
    }
    let doc = doc.map_err(|e| SoapError::Fault(format!("{action}: {e}")))?;
    let answer = format!("{action}Response");
    let values = doc
        .descendants()
        .find(|n| n.tag_name().name() == answer)
        .map(|n| {
            n.children()
                .filter(|c| c.is_element())
                .map(|c| {
                    let text = c.text().unwrap_or("").trim().to_string();
                    (c.tag_name().name().to_string(), text)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(values)
}

/// `H:MM:SS`, `H:MM:SS.fff` or `H:MM:SS.F0/F1` in seconds; `None` for
/// `NOT_IMPLEMENTED` and the like.
fn parse_time(t: &str) -> Option<f64> {
    let mut parts = t.trim().trim_start_matches('+').split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    let mut seconds: f64 = whole.parse().ok()?;
    if let Some((num, den)) = fraction.split_once('/') {
        let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
        if den > 0.0 {
            seconds += num / den;
        }
    } else if !fraction.is_empty() {
        seconds += format!("0.{fraction}").parse::<f64>().ok()?;
    }
    Some(h * 3600.0 + m * 60.0 + seconds)
}

/// DIDL-Lite metadata for a track: renderers want to know what kind of item and stream
/// they are given before they fetch it.
fn didl(uri: &str, mime: &str) -> String {
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
         <item id=\"1\" parentID=\"0\" restricted=\"1\">\
         <dc:title>Rusty Music Server</dc:title>\
         <upnp:class>object.item.audioItem.musicTrack</upnp:class>\
         <res protocolInfo=\"http-get:*:{}:*\">{}</res></item></DIDL-Lite>",
        escape(mime),
        escape(uri)
    )
}

/// A stream given to the renderer.
#[derive(Debug, Clone)]
struct Track {
    uri: String,
    mime: String,
}

/// Where the driver is with the current track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Nothing loaded (or stopped by us).
    Idle,
    /// Loaded without autostart; `Ready` is reported once the device has it.
    Loading,
    /// Loaded and reported ready, waiting for `Resume`.
    Cued,
    /// Asked to play; `Started` is reported once it plays.
    Starting,
    Playing,
    Paused,
}

/// What the renderer said it is doing.
struct Status {
    state: String,
    elapsed: Option<f64>,
    /// `elapsed` is in whole seconds (`RelTime` without a fraction, as most renderers give it).
    whole: bool,
    /// When `elapsed` was true (halfway through the request).
    at: Instant,
    duration: Option<f64>,
    track_uri: String,
}

struct Driver {
    http: reqwest::Client,
    renderer: Renderer,
    base_url: String,
    reports: tokio::sync::mpsc::Sender<FromRenderer>,
    phase: Phase,
    /// The track loaded or playing.
    current: Option<Track>,
    /// The next track, given to the renderer with `SetNextAVTransportURI`.
    next: Option<Track>,
    /// The next track the renderer didn't take: played when the current one ends.
    fallback: Option<Track>,
    /// When `Play` was asked for (for [`START_TIMEOUT`]).
    play_asked: Instant,
    /// When the renderer last answered.
    last_answer: Instant,
    /// The last position the renderer gave while playing.
    elapsed: f64,
    /// The position to a fraction of a second.
    position: super::PositionClock,
    /// Streams given out, to make each URI distinct.
    loads: u64,
    /// The volume last set.
    volume: Option<(u8, bool)>,
}

/// Play what the bridge asks for on the renderer until the bridge ends or the renderer
/// stops answering.
async fn drive(http: reqwest::Client, renderer: Renderer, io: DriverIo) {
    let DriverIo {
        mut commands,
        reports,
        base_url,
    } = io;
    let name = renderer.name.clone();
    let mut d = Driver {
        http,
        renderer,
        base_url,
        reports,
        phase: Phase::Idle,
        current: None,
        next: None,
        fallback: None,
        play_asked: Instant::now(),
        last_answer: Instant::now(),
        elapsed: 0.0,
        position: super::PositionClock::new(),
        loads: 0,
        volume: None,
    };
    let mut tick = tokio::time::interval(POLL_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    // the player is gone (turned off, or the server dropped it)
                    if d.phase != Phase::Idle {
                        let _ = d.call_av("Stop", &[]).await;
                    }
                    return;
                };
                let poll = !matches!(command, ToRenderer::Volume { .. });
                d.command(command).await;
                if poll {
                    d.poll().await;
                }
            }
            _ = tick.tick() => d.poll().await,
        }
        if d.last_answer.elapsed() > GIVE_UP_AFTER {
            info!("{name}: not answering any more");
            return;
        }
    }
}

impl Driver {
    async fn report(&self, report: FromRenderer) {
        let _ = self.reports.send(report).await;
    }

    async fn call(
        &mut self,
        service: Service,
        action: &str,
        args: &[(&str, &str)],
    ) -> Result<HashMap<String, String>, SoapError> {
        let result = soap(&self.http, &service, action, args).await;
        match &result {
            Ok(_) | Err(SoapError::Fault(_)) => self.last_answer = Instant::now(),
            Err(SoapError::Unreachable(e)) => {
                debug!("{}: {action}: {e}", self.renderer.name)
            }
        }
        result
    }

    /// An AVTransport action on instance 0.
    async fn call_av(
        &mut self,
        action: &str,
        args: &[(&str, &str)],
    ) -> Result<HashMap<String, String>, SoapError> {
        let mut all = vec![("InstanceID", "0")];
        all.extend_from_slice(args);
        let service = self.renderer.av_transport.clone();
        self.call(service, action, &all).await
    }

    /// The renderer's URI and MIME type for a stream the bridge names.
    fn track(&mut self, url: &str, format: &str) -> Track {
        let mime = self
            .renderer
            .formats
            .iter()
            .find(|(code, _)| code == format)
            .map(|(_, mime)| mime.clone())
            .unwrap_or_else(|| "audio/mpeg".into());
        let uri = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            // every track has the same stream URL; a counter makes the next one look
            // different from the current one, to the renderer and in its TrackURI
            self.loads += 1;
            let separator = if url.contains('?') { '&' } else { '?' };
            format!("{}{url}{separator}load={}", self.base_url, self.loads)
        };
        Track { uri, mime }
    }

    async fn set_uri(&mut self, track: &Track) -> Result<(), SoapError> {
        let metadata = didl(&track.uri, &track.mime);
        let args = [
            ("CurrentURI", track.uri.as_str()),
            ("CurrentURIMetaData", metadata.as_str()),
        ];
        match self.call_av("SetAVTransportURI", &args).await {
            Err(SoapError::Fault(e)) => {
                // some renderers take a new URI only when stopped
                debug!("{}: {e}; stopping first", self.renderer.name);
                let _ = self.call_av("Stop", &[]).await;
                self.call_av("SetAVTransportURI", &args).await.map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    async fn play(&mut self) -> Result<(), SoapError> {
        self.play_asked = Instant::now();
        self.call_av("Play", &[("Speed", "1")]).await.map(|_| ())
    }

    /// Load `track` and play it (or only get it ready).
    async fn load(&mut self, track: Track, autostart: bool) {
        self.next = None;
        self.fallback = None;
        self.elapsed = 0.0;
        self.current = Some(track.clone());
        let mut result = self.set_uri(&track).await;
        if result.is_ok() && autostart {
            result = self.play().await;
        }
        match result {
            Ok(()) => {
                self.phase = if autostart {
                    Phase::Starting
                } else {
                    Phase::Loading
                };
            }
            Err(e) => {
                warn!("{}: {e}", self.renderer.name);
                self.phase = Phase::Idle;
                self.current = None;
                self.report(FromRenderer::Error {
                    message: e.to_string(),
                })
                .await;
            }
        }
    }

    async fn command(&mut self, command: ToRenderer) {
        match command {
            ToRenderer::Load {
                url,
                format,
                autostart,
                ..
            } => {
                let track = self.track(&url, &format);
                debug!("{}: load {}", self.renderer.name, track.uri);
                self.load(track, autostart).await;
            }
            ToRenderer::Preload { url, format, .. } => {
                let track = self.track(&url, &format);
                let metadata = didl(&track.uri, &track.mime);
                let args = [
                    ("NextURI", track.uri.as_str()),
                    ("NextURIMetaData", metadata.as_str()),
                ];
                match self.call_av("SetNextAVTransportURI", &args).await {
                    Ok(_) => {
                        self.next = Some(track);
                        self.fallback = None;
                    }
                    Err(e) => {
                        debug!("{}: {e}; playing it after this one", self.renderer.name);
                        self.next = None;
                        self.fallback = Some(track);
                    }
                }
            }
            ToRenderer::Pause => {
                let _ = self.call_av("Pause", &[]).await;
            }
            ToRenderer::Resume { .. } => match self.phase {
                Phase::Cued | Phase::Loading => {
                    if self.play().await.is_ok() {
                        self.phase = Phase::Starting;
                    }
                }
                Phase::Paused | Phase::Playing | Phase::Starting => {
                    let _ = self.play().await;
                }
                Phase::Idle => {}
            },
            ToRenderer::Stop => {
                self.phase = Phase::Idle;
                self.current = None;
                self.next = None;
                self.fallback = None;
                let _ = self.call_av("Stop", &[]).await;
            }
            // (only for renderers with their own clock)
            ToRenderer::Hold { .. } | ToRenderer::Skip { .. } => {}
            ToRenderer::Volume { volume, muted } => {
                if self.volume == Some((volume, muted)) {
                    return;
                }
                self.volume = Some((volume, muted));
                let Some(rc) = self.renderer.rendering_control.clone() else {
                    return;
                };
                let volume = volume.to_string();
                let mute = if muted { "1" } else { "0" };
                for (action, name, value) in [
                    ("SetVolume", "DesiredVolume", volume.as_str()),
                    ("SetMute", "DesiredMute", mute),
                ] {
                    let args = [("InstanceID", "0"), ("Channel", "Master"), (name, value)];
                    if let Err(e) = self.call(rc.clone(), action, &args).await {
                        debug!("{}: {e}", self.renderer.name);
                    }
                }
            }
        }
    }

    /// Ask the renderer what it is doing and report what changed.
    async fn poll(&mut self) {
        let Ok(info) = self.call_av("GetTransportInfo", &[]).await else {
            return;
        };
        let asked = Instant::now();
        let Ok(position) = self.call_av("GetPositionInfo", &[]).await else {
            return;
        };
        let at = asked + asked.elapsed() / 2;
        let field = |m: &HashMap<String, String>, k: &str| m.get(k).cloned().unwrap_or_default();
        let status = Status {
            state: field(&info, "CurrentTransportState"),
            elapsed: parse_time(&field(&position, "RelTime")),
            whole: !field(&position, "RelTime").contains('.'),
            at,
            duration: parse_time(&field(&position, "TrackDuration")).filter(|d| *d > 0.0),
            track_uri: field(&position, "TrackURI"),
        };
        self.update(status).await;
    }

    async fn update(&mut self, s: Status) {
        let is_current = |t: &Option<Track>| t.as_ref().is_some_and(|t| t.uri == s.track_uri);
        match s.state.as_str() {
            "PLAYING" => match self.phase {
                Phase::Starting | Phase::Loading | Phase::Cued => {
                    // a renderer may still report the track it played before for a
                    // moment (or report the URI rewritten: then trust it after a while)
                    if s.track_uri.is_empty()
                        || is_current(&self.current)
                        || self.play_asked.elapsed() > Duration::from_secs(3)
                    {
                        self.phase = Phase::Playing;
                        self.elapsed = 0.0;
                        self.position.reset();
                        self.report(FromRenderer::Started).await;
                    }
                }
                Phase::Paused => {
                    self.phase = Phase::Playing;
                    self.position.reset();
                    self.report(FromRenderer::Resumed).await;
                }
                Phase::Playing => {
                    // it went on to the next track by itself; renderers that don't say
                    // which track they play give it away by starting over
                    let restarted =
                        s.track_uri.is_empty() && s.elapsed.is_some_and(|e| e + 1.0 < self.elapsed);
                    if self.next.is_some() && (is_current(&self.next) || restarted) {
                        self.current = self.next.take();
                        self.elapsed = 0.0;
                        self.position.reset();
                        self.report(FromRenderer::Started).await;
                    }
                }
                Phase::Idle => {}
            },
            "PAUSED_PLAYBACK" => match self.phase {
                Phase::Loading => {
                    self.phase = Phase::Cued;
                    self.report(FromRenderer::Ready).await;
                }
                Phase::Starting => {
                    self.phase = Phase::Paused;
                    self.report(FromRenderer::Started).await;
                    self.report(FromRenderer::Paused).await;
                }
                Phase::Playing => {
                    self.phase = Phase::Paused;
                    self.report(FromRenderer::Paused).await;
                }
                _ => {}
            },
            "STOPPED" | "NO_MEDIA_PRESENT" => match self.phase {
                Phase::Loading if s.state == "STOPPED" => {
                    self.phase = Phase::Cued;
                    self.report(FromRenderer::Ready).await;
                }
                Phase::Starting if self.play_asked.elapsed() > START_TIMEOUT => {
                    self.phase = Phase::Idle;
                    self.current = None;
                    self.report(FromRenderer::Error {
                        message: format!("{} did not start playing", self.renderer.name),
                    })
                    .await;
                }
                Phase::Playing | Phase::Paused => self.ended().await,
                _ => {}
            },
            // TRANSITIONING and the like: wait and see
            _ => {}
        }
        if self.phase == Phase::Playing {
            if let Some(reported) = s.elapsed {
                self.elapsed = reported;
                let elapsed = self
                    .position
                    .report(reported, s.whole, s.at, Instant::now());
                self.report(FromRenderer::Progress {
                    elapsed,
                    duration: s.duration,
                    jiffies: None,
                })
                .await;
            }
        }
    }

    /// The current track ended by itself: play the next one if the renderer didn't take
    /// it, or report the end of the music.
    async fn ended(&mut self) {
        let follow = self.fallback.take().or_else(|| self.next.take());
        self.current = None;
        self.phase = Phase::Idle;
        if let Some(track) = follow {
            self.current = Some(track.clone());
            self.elapsed = 0.0;
            match self.set_uri(&track).await {
                Ok(()) if self.play().await.is_ok() => {
                    // `Started` once it plays
                    self.phase = Phase::Starting;
                    return;
                }
                Ok(()) => {}
                Err(e) => warn!("{}: {e}", self.renderer.name),
            }
            self.current = None;
        }
        self.report(FromRenderer::Ended).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>Living &amp; Room</friendlyName>
    <manufacturer>Acme</manufacturer>
    <modelName>Streamer 2</modelName>
    <UDN>uuid:1234</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <controlURL>/ctl/avt</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <controlURL>ctl/rc</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <controlURL>http://10.0.0.2:8080/cm</controlURL>
      </service>
    </serviceList>
  </device>
</root>"#;

    #[test]
    fn descriptions() {
        let r = parse_description(DESCRIPTION, "http://10.0.0.2:8080/dev/desc.xml").unwrap();
        assert_eq!(r.udn, "uuid:1234");
        assert_eq!(r.name, "Living & Room");
        assert_eq!(r.model_name, "Streamer 2");
        assert_eq!(r.av_transport.control, "http://10.0.0.2:8080/ctl/avt");
        assert_eq!(
            r.rendering_control.unwrap().control,
            "http://10.0.0.2:8080/dev/ctl/rc"
        );
        assert_eq!(
            r.connection_manager.unwrap().control,
            "http://10.0.0.2:8080/cm"
        );
        // URLBase wins over the description's own URL
        let with_base = DESCRIPTION.replace(
            "<specVersion>",
            "<URLBase>http://10.0.0.3:1/x/</URLBase><specVersion>",
        );
        let r = parse_description(&with_base, "http://10.0.0.2:8080/dev/desc.xml").unwrap();
        assert_eq!(r.av_transport.control, "http://10.0.0.3:1/ctl/avt");
        assert!(parse_description("<root/>", "http://x/").is_err());
        assert!(parse_description("not xml", "http://x/").is_err());
    }

    #[test]
    fn formats() {
        let sink = "http-get:*:audio/L16;rate=44100;channels=2:*,http-get:*:audio/mpeg:DLNA.ORG_PN=MP3,http-get:*:audio/x-flac:*";
        assert_eq!(
            formats_of(sink),
            vec![
                ("flc".to_string(), "audio/x-flac".to_string()),
                ("mp3".to_string(), "audio/mpeg".to_string())
            ]
        );
        assert!(formats_of("http-get:*:audio/L16:*").is_empty());
        assert!(formats_of("").is_empty());
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("0:00:00"), Some(0.0));
        assert_eq!(parse_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_time("00:01:02.500"), Some(62.5));
        assert_eq!(parse_time("0:00:01.1/4"), Some(1.25));
        assert_eq!(parse_time("NOT_IMPLEMENTED"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn soap_requests_escape_their_arguments() {
        let metadata = didl("http://h/s?a=1&b=2", "audio/flac");
        assert!(metadata.contains(
            "<res protocolInfo=\"http-get:*:audio/flac:*\">http://h/s?a=1&amp;b=2</res>"
        ));
        let body = soap_body(
            "urn:schemas-upnp-org:service:AVTransport:1",
            "SetAVTransportURI",
            &[("InstanceID", "0"), ("CurrentURIMetaData", &metadata)],
        );
        // the metadata is XML inside XML: escaped once more
        assert!(body.contains("&lt;DIDL-Lite"));
        assert!(body.contains("a=1&amp;amp;b=2"));
        let doc = roxmltree::Document::parse(&body).unwrap();
        let arg = doc
            .descendants()
            .find(|n| n.tag_name().name() == "CurrentURIMetaData")
            .unwrap();
        assert_eq!(arg.text(), Some(metadata.as_str()));
    }

    #[test]
    fn ssdp_answers() {
        let answer = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://10.0.0.2:49152/desc.xml\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        assert_eq!(
            header(answer, "LOCATION").as_deref(),
            Some("http://10.0.0.2:49152/desc.xml")
        );
        assert!(same_host(
            "http://127.0.0.1:9000/x",
            "http://127.0.0.1:9000"
        ));
        assert!(!same_host(
            "http://127.0.0.1:9001/x",
            "http://127.0.0.1:9000"
        ));
    }
}
