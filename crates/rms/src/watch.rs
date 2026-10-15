//! Watching the music folders (`autorescan`): when files are added, changed, renamed or
//! removed, the folders they are in are scanned again, so the library follows the disk
//! without full rescans.
//!
//! Events are collected until the folders have been quiet for [`QUIET`] (a copy in
//! progress keeps writing), at most [`MAX_WAIT`], then each affected folder is scanned
//! (`ScanMode::Dir`, which also notices what was removed below it).  A change in the
//! playlist folder rescans the playlists.  While another scan runs, changes wait for it.

use crate::control::commands::{claim_scan, run_scans};
use crate::server::Server;
use notify::event::{AccessKind, AccessMode, EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher};
use parking_lot::Mutex;
use rms_library::ScanMode;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// How long the folders must be quiet before they are scanned.
const QUIET: Duration = Duration::from_secs(2);
/// The longest a change waits for quiet.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// More folders than this are scanned as their music folder as a whole.
const MAX_DIRS: usize = 50;

struct Running {
    _watcher: notify::RecommendedWatcher,
    task: tokio::task::AbortHandle,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Start watching as the prefs say (`autorescan`, `mediadirs`, `playlistdir`), stopping
/// what was watched before.  Called at startup and when one of those prefs changes.
pub fn start(server: &Arc<Server>) {
    let mut running = RUNNING.lock();
    if let Some(r) = running.take() {
        r.task.abort();
    }
    if !server.prefs.server.get_bool("autorescan") {
        return;
    }
    let roots: Vec<PathBuf> = server
        .prefs
        .server
        .get_string_list("mediadirs")
        .into_iter()
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .collect();
    let playlists = Some(PathBuf::from(server.prefs.server.get_str("playlistdir")))
        .filter(|d| !d.as_os_str().is_empty() && d.is_dir());
    if roots.is_empty() && playlists.is_none() {
        return;
    }
    let (tx, rx) = mpsc::unbounded_channel::<PathBuf>();
    let watcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(ev) if relevant(&ev.kind) => {
                for p in ev.paths {
                    let _ = tx.send(p);
                }
            }
            Ok(_) => {}
            Err(e) => warn!("watching the music folders: {e}"),
        });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            warn!("cannot watch the music folders: {e}");
            return;
        }
    };
    for dir in roots.iter().chain(playlists.iter()) {
        match watcher.watch(dir, RecursiveMode::Recursive) {
            Ok(()) => info!("watching {} for changes", dir.display()),
            Err(e) => warn!(
                "cannot watch {} for changes: {e} (for a big library, raise \
                 fs.inotify.max_user_watches)",
                dir.display()
            ),
        }
    }
    let task = crate::util::spawn(run(server.clone(), rx, roots, playlists));
    *running = Some(Running {
        _watcher: watcher,
        task: task.abort_handle(),
    });
}

fn relevant(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        // a file written and closed: the copy is complete
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        _ => false,
    }
}

/// Hidden files and the temporary files of copies and downloads.
fn ignored(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.')
        || name.ends_with('~')
        || [".part", ".tmp", ".crdownload", ".partial"]
            .iter()
            .any(|ext| name.ends_with(ext))
}

async fn run(
    server: Arc<Server>,
    mut rx: mpsc::UnboundedReceiver<PathBuf>,
    roots: Vec<PathBuf>,
    playlists: Option<PathBuf>,
) {
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    // (first change, last change) of what is pending
    let mut times: Option<(Instant, Instant)> = None;
    loop {
        let due = times.map(|(first, last)| (last + QUIET).min(first + MAX_WAIT));
        tokio::select! {
            _ = server.shutdown.cancelled() => return,
            p = rx.recv() => {
                let Some(p) = p else { return };
                if ignored(&p) {
                    continue;
                }
                debug!("changed: {}", p.display());
                pending.insert(p);
                let now = Instant::now();
                times = Some((times.map_or(now, |(f, _)| f), now));
            }
            _ = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                if !claim_scan(&server) {
                    // another scan is running: look again in a moment
                    let now = Instant::now();
                    times = Some((now, now));
                    continue;
                }
                let modes = plan(std::mem::take(&mut pending), &roots, playlists.as_deref());
                times = None;
                if modes.is_empty() {
                    server.scanning.store(false, std::sync::atomic::Ordering::SeqCst);
                    continue;
                }
                info!("music folders changed: scanning {:?}", modes);
                run_scans(&server, modes).await;
            }
        }
    }
}

/// The scans that cover the changed paths: each path's folder (its parent, which also
/// sees it removed), without folders below another one, or the playlists.
fn plan(changed: BTreeSet<PathBuf>, roots: &[PathBuf], playlists: Option<&Path>) -> Vec<ScanMode> {
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut scan_playlists = false;
    for p in changed {
        let Some(root) = roots.iter().find(|r| p.starts_with(r)) else {
            if playlists.is_some_and(|d| p.starts_with(d)) {
                scan_playlists = true;
            }
            continue;
        };
        if p == *root {
            // the music folder itself went away (unmounted?): leave the library alone
            continue;
        }
        let mut dir = p.parent().unwrap_or(root).to_path_buf();
        while !dir.is_dir() && dir != *root {
            match dir.parent() {
                Some(up) => dir = up.to_path_buf(),
                None => break,
            }
        }
        if dir.is_dir() {
            dirs.insert(dir);
        }
    }
    // a folder below another is scanned with it
    let mut kept: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !kept.iter().any(|k| d.starts_with(k)) {
            kept.push(d);
        }
    }
    if kept.len() > MAX_DIRS {
        kept = roots
            .iter()
            .filter(|r| kept.iter().any(|k| k.starts_with(r)))
            .cloned()
            .collect();
    }
    let mut modes: Vec<ScanMode> = kept.into_iter().map(ScanMode::Dir).collect();
    if scan_playlists {
        modes.push(ScanMode::Playlists);
    }
    modes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(modes: &[ScanMode]) -> Vec<String> {
        modes
            .iter()
            .map(|m| match m {
                ScanMode::Dir(d) => d.display().to_string(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn changes_become_folder_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("music");
        let lists = tmp.path().join("playlists");
        for d in ["A/One", "A/Two", "B"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::create_dir_all(&lists).unwrap();
        let r = |s: &str| root.join(s);
        let roots = vec![root.clone()];
        let changed: BTreeSet<PathBuf> = [
            r("A/One/01.flac"),
            r("A/One/02.flac"),
            r("A/Two/cover.jpg"),
            r("B/Gone Album/01.mp3"), // removed with its folder: scan B
            r("A"),                   // a change of A itself: its parent, the root
            lists.join("Mix.m3u"),
            PathBuf::from("/elsewhere/x.flac"),
        ]
        .into_iter()
        .collect();
        let modes = plan(changed, &roots, Some(&lists));
        assert_eq!(
            dirs(&modes),
            vec![root.display().to_string(), "Playlists".into()]
        );

        let changed: BTreeSet<PathBuf> =
            [r("A/One/01.flac"), r("B/02.flac"), r("A/Two/x/y/03.flac")]
                .into_iter()
                .collect();
        assert_eq!(
            dirs(&plan(changed, &roots, None)),
            [r("A/One"), r("A/Two"), r("B")]
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
        );
        // the music folder itself removed: nothing
        assert!(plan([root.clone()].into_iter().collect(), &roots, None).is_empty());
    }

    #[test]
    fn temporary_files_are_ignored() {
        assert!(ignored(Path::new("/m/A/.01.flac.XYZ123")));
        assert!(ignored(Path::new("/m/A/01.flac.part")));
        assert!(ignored(Path::new("/m/A/.DS_Store")));
        assert!(!ignored(Path::new("/m/A/01.flac")));
    }
}
