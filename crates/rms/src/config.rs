//! Command line / environment configuration.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "rms",
    version,
    about = "Rusty Music Server — Lyrion/Squeezebox compatible music server"
)]
pub struct Config {
    /// Directory for preferences, database, cache and logs.
    #[arg(long, env = "RMS_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// Music folder(s) to scan.  Can be repeated.  Overrides the saved preference when given.
    #[arg(long = "music", env = "RMS_MUSIC_DIR", value_delimiter = ',')]
    pub music_dirs: Vec<PathBuf>,

    /// Playlist folder.
    #[arg(long, env = "RMS_PLAYLIST_DIR")]
    pub playlist_dir: Option<PathBuf>,

    /// Address to bind all listeners to.
    #[arg(long, env = "RMS_BIND", default_value = "0.0.0.0")]
    pub bind_addr: String,

    /// HTTP (web UI / JSON-RPC / streaming) port.  Overrides the saved preference when given.
    #[arg(long, env = "RMS_HTTP_PORT")]
    pub http_port: Option<u16>,

    /// HTTPS port for the web UI and API (the same as on the HTTP port); 0 turns it off.
    /// Browsers allow installing the web UI as an app, and its best audio, only over HTTPS.
    #[arg(long, env = "RMS_HTTPS_PORT", default_value_t = 9443)]
    pub https_port: u16,

    /// Certificate chain (PEM) for HTTPS, e.g. from Let's Encrypt or `tailscale cert`.  Without
    /// one the server issues its own from a local certificate authority, which devices trust
    /// once its certificate (`/tls/rms-ca.crt`) is installed.  Reloaded when the file changes.
    #[arg(long, env = "RMS_TLS_CERT", requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,

    /// Private key (PEM) of `--tls-cert`.
    #[arg(long, env = "RMS_TLS_KEY", requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,

    /// MusicBrainz web service to identify recordings with (a local mirror needs no rate limit).
    #[arg(
        long,
        env = "RMS_MUSICBRAINZ_URL",
        default_value = "https://musicbrainz.org"
    )]
    pub musicbrainz_url: String,

    /// ListenBrainz "labs" API, for similar recordings and artists.
    #[arg(
        long,
        env = "RMS_LISTENBRAINZ_URL",
        default_value = "https://labs.api.listenbrainz.org",
        hide = true
    )]
    pub listenbrainz_url: String,

    /// AcoustID web service, for recognising recordings by their sound.
    #[arg(
        long,
        env = "RMS_ACOUSTID_URL",
        default_value = "https://api.acoustid.org",
        hide = true
    )]
    pub acoustid_url: String,

    /// Telnet CLI port.  Overrides the saved preference when given.
    #[arg(long, env = "RMS_CLI_PORT")]
    pub cli_port: Option<u16>,

    /// Advertised HTTP address (IPAD in discovery).  Normally auto-detected by players.
    #[arg(long, env = "RMS_HTTP_ADDR")]
    pub http_addr: Option<String>,

    /// Display name of this server / library.
    #[arg(long, env = "RMS_NAME")]
    pub name: Option<String>,

    /// Run a library scan at startup.
    #[arg(long)]
    pub rescan: bool,

    /// Wipe the library database and rescan at startup.
    #[arg(long)]
    pub wipe: bool,

    /// Run only the scanner, then exit.
    #[arg(long)]
    pub scan_only: bool,

    /// Path to a convert.conf-style transcoding table (defaults to the built-in table).
    #[arg(long, env = "RMS_CONVERT_CONF")]
    pub convert_conf: Option<PathBuf>,

    /// Slimproto (player control) TCP port.  Players always use 3483; change only for testing.
    #[arg(long, env = "RMS_SLIMPROTO_PORT", default_value_t = 3483)]
    pub slimproto_port: u16,

    /// Disable the UDP discovery responder.
    #[arg(long)]
    pub no_discovery: bool,

    /// Log filter (tracing EnvFilter syntax).
    #[arg(long, env = "RMS_LOG", default_value = "info,rms=info")]
    pub log: String,
}

impl Config {
    pub fn prefs_dir(&self) -> PathBuf {
        self.data_dir.join("prefs")
    }
    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("library.sqlite")
    }
}
