//! HTTPS: the web UI, the APIs and the streams on a second port (`--https-port`, 9443), with
//! the same routes as the HTTP port.  Browsers keep their newer abilities to secure pages:
//! installing the web UI as an app (service workers), and for the browser player AudioWorklet
//! and WebCodecs.  Squeezeboxes and the LMS clients stay on HTTP.
//!
//! The certificate is the one given (`--tls-cert`, `--tls-key`; reloaded when the files
//! change, so a renewal by certbot or `tailscale cert` is picked up), or one the server issues
//! itself from a *local certificate authority* it creates on first start (`<data dir>/tls`).
//! A device trusts the server once that authority's certificate is installed on it
//! (`/tls/rms-ca.crt`).  The authority can only vouch for names on the local network
//! (`localhost`, this host's name, `.local`, `.lan`, `.home.arpa`, … and private addresses):
//! its name constraints say so, and browsers enforce them, so its key could not be used to
//! impersonate other sites.  The server's own certificate names the host and its current
//! addresses, and is issued again when they change and before it expires.

use crate::server::Server;
use anyhow::Context;
use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::serve::{Listener, ListenerExt};
use chrono::Datelike;
use parking_lot::{Mutex, RwLock};
use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, KeyPair, KeyUsagePurpose, NameConstraints,
    SanType,
};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::{ClientHello, ResolvesServerCert};
use tokio_rustls::rustls::sign::CertifiedKey;
use tracing::{debug, info, warn};

/// Lifetime of the server's own certificate (Apple devices refuse longer ones).
const SERVER_DAYS: i64 = 397;
/// Issue it again this long before it expires.
const RENEW_BEFORE: Duration = Duration::from_secs(30 * 86400);
const CA_YEARS: i32 = 20;
/// How often the certificate is checked (addresses changed, renewal, files replaced).
const CHECK_EVERY: Duration = Duration::from_secs(600);

/// Names the local authority may vouch for (and their subdomains).
const LOCAL_DOMAINS: &[&str] = &[
    "localhost",
    "local",
    "lan",
    "home",
    "home.arpa",
    "internal",
    "localdomain",
];
/// Addresses it may vouch for: private, loopback, link-local and CGNAT (Tailscale) ranges.
const LOCAL_NETS: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "100.64.0.0/10",
    "::1/128",
    "fc00::/7",
    "fe80::/10",
];

/// A day as certificates give validity.
macro_rules! date {
    ($d:expr) => {{
        let d: chrono::NaiveDate = $d;
        rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
    }};
}

/// The port HTTPS listens on, once it does.
static PORT: Mutex<Option<u16>> = Mutex::new(None);
/// The local authority's certificate (DER), when the server uses it.
static CA: RwLock<Option<Vec<u8>>> = RwLock::new(None);

/// The HTTPS port, if the server listens on one.
pub fn port() -> Option<u16> {
    *PORT.lock()
}

/// Whether the certificate comes from the server's local authority (which devices install).
pub fn local_ca() -> bool {
    CA.read().is_some()
}

/// `GET /tls/rms-ca.crt` (DER, which phones install with a tap) and `/tls/rms-ca.pem`.
pub async fn ca_certificate(pem: bool) -> Response {
    let Some(der) = CA.read().clone() else {
        return (StatusCode::NOT_FOUND, "no local certificate authority").into_response();
    };
    let (body, ct, name) = if pem {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap_or_default());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        (pem.into_bytes(), "application/x-pem-file", "rms-ca.pem")
    } else {
        (der, "application/x-x509-ca-cert", "rms-ca.crt")
    };
    let mut r = Response::new(axum::body::Body::from(body));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    r
}

/// Serve `app` over HTTPS until shutdown.  Not being able to (the port is taken, no
/// certificate) is logged and leaves HTTP running.
pub async fn serve(
    server: Arc<Server>,
    app: IntoMakeServiceWithConnectInfo<axum::Router, SocketAddr>,
) -> anyhow::Result<()> {
    let port = server.config.https_port;
    if port == 0 {
        return Ok(());
    }
    let source = Source::of(&server);
    let first = match source.load() {
        Ok(c) => c,
        Err(e) => {
            warn!("HTTPS is off: {e:#}");
            return Ok(());
        }
    };
    let addr = format!("{}:{}", server.config.bind_addr, port);
    let tcp = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            warn!("HTTPS is off: can't listen on {addr}: {e}");
            return Ok(());
        }
    };
    let resolver = Arc::new(Resolver(RwLock::new(first.key)));
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_cert_resolver(resolver.clone());
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    *PORT.lock() = Some(port);
    info!("HTTPS listening on https://{addr}/");

    // keep the certificate current
    let mut stamp = first.stamp;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(CHECK_EVERY).await;
            if source.stamp() == stamp {
                continue;
            }
            match source.load() {
                Ok(c) => {
                    info!("HTTPS: certificate updated");
                    stamp = c.stamp;
                    *resolver.0.write() = c.key;
                }
                Err(e) => warn!("HTTPS: keeping the current certificate: {e:#}"),
            }
        }
    });

    // TLS handshakes run on their own, so a slow client holds up nobody else
    let (tx, rx) = mpsc::channel(64);
    let local = tcp.local_addr()?;
    tokio::spawn(async move {
        loop {
            let (sock, peer) = match tcp.accept().await {
                Ok(s) => s,
                Err(e) => {
                    debug!("HTTPS accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(sock)).await {
                    Ok(Ok(tls)) => {
                        let _ = tx.send((tls, peer)).await;
                    }
                    Ok(Err(e)) => debug!("HTTPS handshake with {peer}: {e}"),
                    Err(_) => debug!("HTTPS handshake with {peer}: timed out"),
                }
            });
        }
    });
    let shutdown = server.shutdown.clone();
    axum::serve(TlsListener { rx, local }.tap_io(|_| {}), app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

/// Connections whose TLS handshake is done.
struct TlsListener {
    rx: mpsc::Receiver<(tokio_rustls::server::TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(c) => c,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[derive(Debug)]
struct Resolver(RwLock<Arc<CertifiedKey>>);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.read().clone())
    }
}

/// A certificate ready to serve, and what it was made from (to tell when to make it again).
struct Loaded {
    key: Arc<CertifiedKey>,
    stamp: Stamp,
}

/// What a certificate depends on: the files' modification times, or the names and the day.
#[derive(Debug, Clone, PartialEq)]
enum Stamp {
    Files(Option<SystemTime>, Option<SystemTime>),
    Local(Vec<String>, u64),
}

enum Source {
    Files { cert: PathBuf, key: PathBuf },
    Local { dir: PathBuf, names: Names },
}

/// Where this host can be reached by name: its host name, the advertised address.
#[derive(Clone)]
struct Names {
    host: Option<String>,
    advertised: Option<String>,
}

impl Source {
    fn of(server: &Server) -> Source {
        match (&server.config.tls_cert, &server.config.tls_key) {
            (Some(cert), Some(key)) => Source::Files {
                cert: cert.clone(),
                key: key.clone(),
            },
            _ => Source::Local {
                dir: server.config.data_dir.join("tls"),
                names: Names {
                    host: hostname::get()
                        .ok()
                        .and_then(|h| h.into_string().ok())
                        .map(|h| h.trim().to_ascii_lowercase())
                        .filter(|h| is_dns_name(h)),
                    advertised: server.config.http_addr.clone(),
                },
            },
        }
    }

    fn stamp(&self) -> Stamp {
        match self {
            Source::Files { cert, key } => {
                let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
                Stamp::Files(m(cert), m(key))
            }
            // the names now, and the day (renewal is checked once a day)
            Source::Local { names, .. } => {
                Stamp::Local(wanted_names(names), crate::util::now() as u64 / 86400)
            }
        }
    }

    fn load(&self) -> anyhow::Result<Loaded> {
        let stamp = self.stamp();
        let key = match self {
            Source::Files { cert, key } => {
                let chain = CertificateDer::pem_file_iter(cert)
                    .with_context(|| format!("reading {}", cert.display()))?
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("reading {}", cert.display()))?;
                anyhow::ensure!(!chain.is_empty(), "no certificate in {}", cert.display());
                let key = PrivateKeyDer::from_pem_file(key)
                    .with_context(|| format!("reading {}", key.display()))?;
                *CA.write() = None;
                certified(chain, &key)?
            }
            Source::Local { dir, names } => local_certificate(dir, names)?,
        };
        Ok(Loaded { key, stamp })
    }
}

fn certified(
    chain: Vec<CertificateDer<'static>>,
    key: &PrivateKeyDer<'_>,
) -> anyhow::Result<Arc<CertifiedKey>> {
    let signer = rustls::crypto::ring::sign::any_supported_type(key)
        .map_err(|e| anyhow::anyhow!("unusable private key: {e}"))?;
    Ok(Arc::new(CertifiedKey::new(chain, signer)))
}

/// The local authority, as stored.
#[derive(Serialize, Deserialize)]
struct CaInfo {
    /// Its common name.
    name: String,
    /// The names it may vouch for (and their subdomains).
    domains: Vec<String>,
}

/// The server's certificate, as stored.
#[derive(Serialize, Deserialize)]
struct ServerInfo {
    names: Vec<String>,
    /// Unix time it expires.
    expires: i64,
}

/// The server's certificate from the local authority: the stored one while it names the
/// host's current addresses and is not about to expire, else a new one.
fn local_certificate(dir: &Path, names: &Names) -> anyhow::Result<Arc<CertifiedKey>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let (ca_info, ca_key, ca_der) = authority(dir, names)?;
    let ca_issuer = ca_params(&ca_info)?.self_signed(&ca_key)?;
    *CA.write() = Some(ca_der.clone());

    let names: Vec<String> = wanted_names(names)
        .into_iter()
        .filter(|n| permitted(&ca_info.domains, n))
        .collect();
    let (crt, key, info) = (
        dir.join("server.crt"),
        dir.join("server.key"),
        dir.join("server.json"),
    );
    let now = crate::util::now() as i64;
    let stored = std::fs::read(&info)
        .ok()
        .and_then(|b| serde_json::from_slice::<ServerInfo>(&b).ok());
    if let Some(s) = stored {
        if s.names == names && s.expires - now > RENEW_BEFORE.as_secs() as i64 {
            if let (Ok(c), Ok(k)) = (
                CertificateDer::from_pem_file(&crt),
                PrivateKeyDer::from_pem_file(&key),
            ) {
                return certified(vec![c, CertificateDer::from(ca_der)], &k);
            }
        }
    }

    let mut p = CertificateParams::default();
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name.push(
        DnType::CommonName,
        names
            .iter()
            .find(|n| n.parse::<IpAddr>().is_err())
            .cloned()
            .unwrap_or_else(|| "rms".into()),
    );
    p.subject_alt_names = names
        .iter()
        .map(|n| match n.parse::<IpAddr>() {
            Ok(ip) => Ok(SanType::IpAddress(ip)),
            Err(_) => Ok(SanType::DnsName(n.clone().try_into()?)),
        })
        .collect::<Result<_, rcgen::Error>>()?;
    let today = chrono::Utc::now().date_naive();
    p.not_before = date!(today - chrono::Days::new(1));
    p.not_after = date!(today + chrono::Days::new(SERVER_DAYS as u64));
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.use_authority_key_identifier_extension = true;
    let server_key = KeyPair::generate()?;
    let cert = p.signed_by(&server_key, &ca_issuer, &ca_key)?;
    write_private(&key, server_key.serialize_pem().as_bytes())?;
    std::fs::write(&crt, cert.pem())?;
    let s = ServerInfo {
        names: names.clone(),
        expires: now + SERVER_DAYS * 86400,
    };
    std::fs::write(&info, serde_json::to_vec_pretty(&s)?)?;
    info!("HTTPS: issued a certificate for {}", names.join(", "));
    certified(
        vec![cert.der().clone(), CertificateDer::from(ca_der)],
        &PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes())
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    )
}

/// The local authority: its description, key and certificate (DER), made on first use.
fn authority(dir: &Path, names: &Names) -> anyhow::Result<(CaInfo, KeyPair, Vec<u8>)> {
    let (info, key, crt) = (dir.join("ca.json"), dir.join("ca.key"), dir.join("ca.crt"));
    if let (Ok(i), Ok(k), Ok(c)) = (
        std::fs::read(&info),
        std::fs::read_to_string(&key),
        CertificateDer::from_pem_file(&crt),
    ) {
        let i: CaInfo = serde_json::from_slice(&i).context("reading ca.json")?;
        return Ok((i, KeyPair::from_pem(&k)?, c.to_vec()));
    }
    let mut domains: Vec<String> = LOCAL_DOMAINS.iter().map(|d| d.to_string()).collect();
    for n in [&names.host, &names.advertised].into_iter().flatten() {
        if is_dns_name(n) && !domains.iter().any(|d| d == n) {
            domains.push(n.clone());
        }
    }
    let host = names.host.clone().unwrap_or_else(|| "this server".into());
    let info_v = CaInfo {
        name: format!("Rusty Music Server on {host}"),
        domains,
    };
    let ca_key = KeyPair::generate()?;
    let mut p = ca_params(&info_v)?;
    let today = chrono::Utc::now().date_naive();
    p.not_before = date!(today - chrono::Days::new(1));
    p.not_after = date!(today.with_year(today.year() + CA_YEARS).unwrap_or(today));
    let cert = p.self_signed(&ca_key)?;
    write_private(&key, ca_key.serialize_pem().as_bytes())?;
    std::fs::write(&crt, cert.pem())?;
    std::fs::write(&info, serde_json::to_vec_pretty(&info_v)?)?;
    info!(
        "HTTPS: created a local certificate authority ({}); install {} on your devices",
        info_v.name,
        crt.display()
    );
    Ok((info_v, ca_key, cert.der().to_vec()))
}

/// The authority's certificate parameters (also used to sign with: the issuer's name and
/// key identifier come from them).
fn ca_params(info: &CaInfo) -> anyhow::Result<CertificateParams> {
    let mut p = CertificateParams::default();
    p.distinguished_name = DistinguishedName::new();
    p.distinguished_name
        .push(DnType::CommonName, info.name.clone());
    p.distinguished_name
        .push(DnType::OrganizationName, "Rusty Music Server");
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let mut permitted: Vec<GeneralSubtree> = info
        .domains
        .iter()
        .map(|d| GeneralSubtree::DnsName(d.clone()))
        .collect();
    for net in LOCAL_NETS {
        let cidr: CidrSubnet = net
            .parse()
            .map_err(|_| anyhow::anyhow!("bad network {net}"))?;
        permitted.push(GeneralSubtree::IpAddress(cidr));
    }
    p.name_constraints = Some(NameConstraints {
        permitted_subtrees: permitted,
        excluded_subtrees: Vec::new(),
    });
    Ok(p)
}

/// The names the server's certificate should carry: `localhost`, the host name (also under
/// `.local`), the advertised address, and the addresses of its network interfaces.
fn wanted_names(names: &Names) -> Vec<String> {
    let mut out = vec!["localhost".to_string()];
    if let Some(h) = &names.host {
        out.push(h.clone());
        if !h.contains('.') {
            out.push(format!("{h}.local"));
        }
    }
    if let Some(a) = &names.advertised {
        let a = a.trim().to_ascii_lowercase();
        let a = a.split(':').next().unwrap_or_default().to_string();
        if is_dns_name(&a) || a.parse::<IpAddr>().is_ok() {
            out.push(a);
        }
    }
    out.push("127.0.0.1".into());
    out.push("::1".into());
    if let Ok(ifs) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in ifs {
            // (IPv6 link-local addresses need a zone, which URLs can't carry)
            if let IpAddr::V6(v6) = ip {
                if v6.segments()[0] & 0xffc0 == 0xfe80 {
                    continue;
                }
            }
            out.push(ip.to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| seen.insert(n.clone()));
    out.sort();
    out
}

/// Whether the authority may vouch for `name` (a DNS name, or an address).
fn permitted(domains: &[String], name: &str) -> bool {
    match name.parse::<IpAddr>() {
        Ok(ip) => LOCAL_NETS.iter().any(|n| in_net(ip, n)),
        Err(_) => domains
            .iter()
            .any(|d| name == d || name.ends_with(&format!(".{d}"))),
    }
}

fn in_net(ip: IpAddr, net: &str) -> bool {
    let Some((base, bits)) = net.split_once('/') else {
        return false;
    };
    let (Ok(base), Ok(bits)) = (base.parse::<IpAddr>(), bits.parse::<u32>()) else {
        return false;
    };
    match (ip, base) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

fn is_dns_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.parse::<IpAddr>().is_err()
        && s.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Write a private key readable by the server's user only.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_authority_may_vouch_for() {
        let d: Vec<String> = ["local", "lan", "nas"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(permitted(&d, "nas"));
        assert!(permitted(&d, "nas.local"));
        assert!(permitted(&d, "music.lan"));
        assert!(!permitted(&d, "example.com"));
        assert!(!permitted(&d, "local.example.com"));
        assert!(permitted(&d, "192.168.1.10"));
        assert!(permitted(&d, "10.1.2.3"));
        assert!(permitted(&d, "172.31.255.1"));
        assert!(!permitted(&d, "172.32.0.1"));
        assert!(permitted(&d, "100.101.102.103"));
        assert!(!permitted(&d, "8.8.8.8"));
        assert!(permitted(&d, "fd00::1"));
        assert!(!permitted(&d, "2001:db8::1"));
    }

    #[test]
    fn a_certificate_is_issued_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let names = Names {
            host: Some("musicbox".into()),
            advertised: None,
        };
        local_certificate(dir.path(), &names).unwrap();
        let first = std::fs::read(dir.path().join("server.crt")).unwrap();
        let ca = std::fs::read(dir.path().join("ca.crt")).unwrap();
        // the same names: the same certificate
        local_certificate(dir.path(), &names).unwrap();
        assert_eq!(std::fs::read(dir.path().join("server.crt")).unwrap(), first);
        // another name: a new certificate from the same authority
        let renamed = Names {
            host: Some("musicbox".into()),
            advertised: Some("music.lan".into()),
        };
        local_certificate(dir.path(), &renamed).unwrap();
        assert_ne!(std::fs::read(dir.path().join("server.crt")).unwrap(), first);
        assert_eq!(std::fs::read(dir.path().join("ca.crt")).unwrap(), ca);
        let info: ServerInfo =
            serde_json::from_slice(&std::fs::read(dir.path().join("server.json")).unwrap())
                .unwrap();
        assert!(info.names.contains(&"music.lan".to_string()));
        assert!(info.names.contains(&"musicbox.local".to_string()));
    }
}
