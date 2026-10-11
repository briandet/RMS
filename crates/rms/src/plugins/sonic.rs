//! Sonic analysis (pref `sonicAnalysis`, on by default; not part of LMS): after a scan, local
//! tracks are analysed with bliss in the background so "Don't Stop The Music" and "Create Mix"
//! can pick songs that sound like the seeds.  See `rms_library::sonic`.

use crate::server::Server;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::warn;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Set when the pref is turned off, to stop a running analysis.
static STOP: AtomicBool = AtomicBool::new(false);

fn enabled(server: &Server) -> bool {
    server.prefs.server.get_bool("sonicAnalysis")
}

/// Half the cores: analysis is CPU-heavy and should leave room for streaming and the UI.
fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(1)
        .max(1)
}

/// Start analysing tracks that have no analysis yet (no-op unless enabled, or while running);
/// stop a running analysis when the pref has been turned off.
pub fn spawn_analysis(server: &Arc<Server>) {
    if !enabled(server) {
        STOP.store(true, Ordering::SeqCst);
        return;
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    STOP.store(false, Ordering::SeqCst);
    let s = server.clone();
    tokio::spawn(async move {
        let lib = s.library.clone();
        let ffmpeg = s.has_ffmpeg().then(crate::server::ffmpeg_path);
        let res = tokio::task::spawn_blocking(move || {
            lib.analyze_pending(threads(), ffmpeg.as_deref(), &STOP)
        })
        .await;
        RUNNING.store(false, Ordering::SeqCst);
        if let Err(e) = res {
            warn!("sonic analysis panicked: {}", e);
        }
    });
}
