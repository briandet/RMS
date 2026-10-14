//! EffNet analysis (pref `effnet`, off by default): what kind of music each track is, by
//! Essentia's Discogs-EffNet model and its heads (`rms_library::effnet`), for mixes and for
//! each track's styles and moods.  The models are MTG's, under CC BY-NC-SA 4.0 (non-commercial),
//! so they are not part of RMS: turning the feature on downloads them (about 30 MB) from
//! essentia.upf.edu into `<data dir>/models/effnet`.  The analysis runs with the sonic analysis
//! (`sonicAnalysis`), on the audio it decodes anyway, about a second and a half per track.

use crate::server::Server;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

pub const PREF: &str = "effnet";

/// Where the feature is at.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Status {
    #[default]
    Off,
    /// Downloading the models: files done of all.
    Downloading(usize, usize),
    Ready,
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Off);
static BUSY: Mutex<bool> = Mutex::new(false);

pub fn status() -> Status {
    STATUS.lock().clone()
}

fn set(s: Status) {
    *STATUS.lock() = s;
}

pub fn enabled(server: &Server) -> bool {
    server.prefs.server.get_bool(PREF)
}

fn dir(server: &Server) -> PathBuf {
    server.config.data_dir.join("models").join("effnet")
}

/// Follow the pref: on, download what is missing, load the models and analyse; off, stop
/// using them (the files stay).
pub fn apply(server: &Arc<Server>) {
    if !enabled(server) {
        server.library.set_effnet(None);
        set(Status::Off);
        return;
    }
    if server.library.effnet().is_some() {
        return;
    }
    {
        let mut busy = BUSY.lock();
        if *busy {
            return;
        }
        *busy = true;
    }
    let s = server.clone();
    tokio::spawn(async move {
        let r = prepare(&s).await;
        *BUSY.lock() = false;
        match r {
            Ok(()) => {
                set(Status::Ready);
                if !s.library.is_scanning() {
                    crate::plugins::sonic::spawn_analysis(&s);
                }
            }
            Err(e) => {
                warn!("EffNet: {e}");
                set(Status::Failed(e));
            }
        }
    });
}

async fn prepare(server: &Arc<Server>) -> Result<(), String> {
    let dir = dir(server);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let files = rms_library::effnet::files();
    let missing: Vec<&(String, String)> = files
        .iter()
        .filter(|(n, _)| !dir.join(n).exists())
        .collect();
    let base = server.config.effnet_url.trim_end_matches('/').to_string();
    for (i, (name, path)) in missing.iter().enumerate() {
        set(Status::Downloading(i, missing.len()));
        let url = format!("{base}/{path}");
        let bytes = download(server, &url).await?;
        let part = dir.join(format!("{name}.part"));
        std::fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", part.display()))?;
        std::fs::rename(&part, dir.join(name)).map_err(|e| e.to_string())?;
    }
    if !missing.is_empty() {
        info!(
            "EffNet: downloaded {} model files into {}",
            missing.len(),
            dir.display()
        );
    }
    set(Status::Downloading(missing.len(), missing.len()));
    let d = dir.clone();
    let models = tokio::task::spawn_blocking(move || rms_library::effnet::Models::load(&d))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("loading the models from {}: {e}", dir.display()))?;
    if !enabled(server) {
        return Err("turned off".into());
    }
    server.library.set_effnet(Some(Arc::new(models)));
    info!("EffNet: models loaded");
    Ok(())
}

async fn download(server: &Server, url: &str) -> Result<Vec<u8>, String> {
    let resp = server
        .http
        .get(url)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    if bytes.is_empty() {
        return Err(format!("{url}: empty"));
    }
    Ok(bytes.to_vec())
}
