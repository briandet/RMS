//! The TV visualiser: MilkDrop presets drawn by projectM (the `pmviz` helper, see
//! `tools/projectm-viz`) to the music, as one live video stream with the music in it, for
//! the Chromecasts of a sync group.
//!
//! A [`Session`] belongs to a sync group: one `pmviz` draws and encodes on the GPU, the
//! group's tracks are decoded and fed to it one after another (gaplessly), and every
//! Chromecast of the group plays the same stream (`/viz/<id>.mp4`), fragmented MP4 that
//! [`mp4`] splits so a TV can join at any fragment.  Where each track starts in the
//! stream is kept ([`Session::start_of`]), so a Chromecast's position in the stream tells
//! which track it plays and how far into it.
//!
//! The stream is read as fast as the slowest TV takes it: a paused TV holds the stream (and
//! `pmviz`, which carries on from there when it reads again).

pub mod mp4;

use crate::server::Server;
use bytes::Bytes;
use mp4::{Fragment, Piece, Splitter};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// The sample rate and channels `pmviz` takes on stdin (32-bit float).
const RATE: u32 = 44100;
const FRAME_BYTES: u64 = 8;
/// Fragments (a second each) kept for readers behind the newest; the stream waits for the
/// slowest reader beyond that.
const KEEP: usize = 8;
/// A reader starts this many fragments before the newest (so the TV that started a session
/// gets it from the beginning, and one joining later isn't far ahead of the others).
const JOIN_BACK: u64 = 3;
/// A session nobody has read for this long is stopped.
const IDLE: Duration = Duration::from_secs(30);
/// A session's first track may still be joined by the group's other TVs for this long.
const JOIN_WINDOW: Duration = Duration::from_secs(20);
/// How long after a track is all decoded the next one may take to come before the stream
/// ends (the end of the queue).
const NEXT_WITHIN: Duration = Duration::from_secs(4);
/// Decoded audio buffered ahead of `pmviz` (bytes of 32-bit stereo: about 3 seconds).
const PCM_CHUNK: usize = 64 * 1024;
const PCM_CHUNKS: usize = 16;

/// How the visualiser runs, from the server's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub command: PathBuf,
    pub presets: PathBuf,
    pub textures: Option<PathBuf>,
    /// `1920x1080`
    pub size: String,
    pub fps: u32,
    /// Size projectM draws at (scaled up to `size`); empty: `size`.
    pub render_size: String,
    pub preset_seconds: u32,
    /// `h264_vaapi`, `libx264`, …
    pub encoder: String,
    /// Encoder quality (QP / CRF: higher is a smaller stream); 0: pmviz's default.
    pub quality: u32,
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// The first of `candidates` that exists.
fn first_existing(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.exists())
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
}

/// The visualiser's settings, or why it can't run here.
pub fn config(server: &Server) -> Result<Config, String> {
    let prefs = &server.prefs.server;
    let set = |k: &str| {
        Some(prefs.get_str(k))
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
    };
    let command = match set("visualizerCommand") {
        Some(p) => Some(p),
        None => first_existing(
            exe_dir()
                .map(|d| d.join("pmviz"))
                .into_iter()
                .chain([PathBuf::from("/usr/local/lib/rms/pmviz")]),
        )
        .or_else(|| on_path("pmviz")),
    }
    .filter(|p| p.is_file())
    .ok_or("the pmviz helper isn't installed (see tools/projectm-viz)")?;
    let share = |sub: &str| {
        first_existing(
            [PathBuf::from("/usr/local/share/rms/projectm")]
                .into_iter()
                .chain(exe_dir().map(|d| d.join("../share/rms/projectm")))
                .map(|d| d.join(sub)),
        )
    };
    let presets = set("visualizerPresets")
        .or_else(|| share("presets"))
        .filter(|p| p.exists())
        .ok_or("no MilkDrop presets found (visualizerPresets)")?;
    let textures = set("visualizerTextures").or_else(|| share("textures"));
    let size = Some(prefs.get_str("visualizerSize"))
        .filter(|s| valid_size(s))
        .unwrap_or_else(|| "1920x1080".into());
    let render_size = Some(prefs.get_str("visualizerRenderSize"))
        .filter(|s| valid_size(s))
        .unwrap_or_default();
    let fps = match prefs.get_i64("visualizerFps") {
        f @ 1..=120 => f as u32,
        _ => 60,
    };
    let preset_seconds = match prefs.get_i64("visualizerPresetSeconds") {
        s @ 5..=3600 => s as u32,
        _ => 30,
    };
    let encoder = Some(prefs.get_str("visualizerEncoder"))
        .filter(|e| !e.is_empty() && e != "auto")
        .unwrap_or_else(|| {
            if Path::new("/dev/dri/renderD128").exists() {
                "h264_vaapi".into()
            } else {
                "libx264".into()
            }
        });
    let quality = match prefs.get_i64("visualizerQuality") {
        q @ 1..=51 => q as u32,
        _ => 0,
    };
    Ok(Config {
        quality,
        command,
        presets,
        textures,
        size,
        fps,
        render_size,
        preset_seconds,
        encoder,
    })
}

/// `WxH`, both even and sensible.
pub fn valid_size(s: &str) -> bool {
    let Some((w, h)) = s.split_once('x') else {
        return false;
    };
    matches!((w.parse::<u32>(), h.parse::<u32>()), (Ok(w), Ok(h))
        if (160..=7680).contains(&w) && (90..=4320).contains(&h) && w % 4 == 0 && h % 2 == 0)
}

impl Config {
    fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "-i",
            "-",
            "-o",
            "-",
            "--gpu-yuv",
            "-e",
            &self.encoder,
            "-s",
            &self.size,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.extend(["-r".into(), self.fps.to_string()]);
        a.extend(["-d".into(), self.preset_seconds.to_string()]);
        a.extend(["-p".into(), self.presets.display().to_string()]);
        if let Some(t) = &self.textures {
            a.extend(["-t".into(), t.display().to_string()]);
        }
        if !self.render_size.is_empty() {
            a.extend(["--render-size".into(), self.render_size.clone()]);
        }
        if self.quality > 0 {
            a.extend(["-q".into(), self.quality.to_string()]);
        }
        a
    }
}

// ------------------------------------------------------------------------------- sessions

/// One visualiser stream, for a sync group.
pub struct Session {
    pub id: String,
    /// The sync group (its master's id).
    key: String,
    /// The track it started with (the group's other TVs join a session of the same track).
    first_song: u64,
    created: Instant,
    feed: mpsc::Sender<Track>,
    /// Song id → where (seconds) in the stream its audio starts.
    timeline: Mutex<Vec<(u64, f64)>>,
    out: Mutex<Out>,
    /// Bumped when a fragment arrives or the stream ends (wakes readers).
    data: watch::Sender<u64>,
    /// Bumped when a reader moves on, comes or goes (wakes the producer).
    progress: watch::Sender<u64>,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Out {
    init: Option<Bytes>,
    scales: HashMap<u32, u32>,
    frags: VecDeque<Fragment>,
    /// Index of `frags[0]`.
    base: u64,
    ended: bool,
    /// Reader id → index of the next fragment it reads.
    readers: HashMap<u64, u64>,
    next_reader: u64,
    /// Reader token (the player) → where (seconds) in the stream its stream starts.
    joined: HashMap<String, f64>,
    last_read: Option<Instant>,
}

impl Out {
    fn next(&self) -> u64 {
        self.base + self.frags.len() as u64
    }
}

/// A track to feed: its stream (requested when the track came, as a player would), and
/// what to call once it is all decoded.
struct Track {
    song_id: u64,
    source: tokio::sync::oneshot::Receiver<reqwest::Result<reqwest::Response>>,
    fetched: Box<dyn FnOnce() + Send>,
}

static SESSIONS: LazyLock<Mutex<HashMap<String, Arc<Session>>>> = LazyLock::new(Default::default);

/// A session by id.
pub fn get(id: &str) -> Option<Arc<Session>> {
    SESSIONS.lock().get(id).cloned()
}

/// The sync group `key`'s session for `song_id`: the one another of its TVs just started
/// with this track (`false`: joined), or a new one (`true`: this TV feeds it), which ends
/// the group's previous session.
pub fn start_or_join(
    server: &Arc<Server>,
    key: &str,
    song_id: u64,
) -> Result<(Arc<Session>, bool), String> {
    let mut all = SESSIONS.lock();
    all.retain(|_, s| !s.cancel.is_cancelled());
    if let Some(s) = all
        .values()
        .find(|s| s.key == key && s.first_song == song_id && s.created.elapsed() < JOIN_WINDOW)
    {
        return Ok((s.clone(), false));
    }
    for s in all.values().filter(|s| s.key == key) {
        s.stop();
    }
    let cfg = config(server)?;
    let s = spawn(&cfg, key, song_id)?;
    all.insert(s.id.clone(), s.clone());
    Ok((s, true))
}

fn spawn(cfg: &Config, key: &str, song_id: u64) -> Result<Arc<Session>, String> {
    let id = format!("{:016x}", rand::random::<u64>());
    let mut child = tokio::process::Command::new(&cfg.command)
        .args(cfg.args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", cfg.command.display()))?;
    info!(
        "visualiser {id}: {} {}",
        cfg.command.display(),
        cfg.args().join(" ")
    );
    let stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (feed, feed_rx) = mpsc::channel(8);
    let s = Arc::new(Session {
        id: id.clone(),
        key: key.to_string(),
        first_song: song_id,
        created: Instant::now(),
        feed,
        timeline: Mutex::new(Vec::new()),
        out: Mutex::new(Out::default()),
        data: watch::channel(0).0,
        progress: watch::channel(0).0,
        cancel: CancellationToken::new(),
    });
    let (pcm_tx, pcm_rx) = mpsc::channel(PCM_CHUNKS);
    crate::util::spawn(log(id.clone(), stderr));
    crate::util::spawn(decode(s.clone(), feed_rx, pcm_tx));
    crate::util::spawn(write(s.clone(), pcm_rx, stdin));
    crate::util::spawn(read(s.clone(), stdout));
    let w = s.clone();
    crate::util::spawn(async move {
        // stopped, idle, or pmviz gone
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = w.cancel.cancelled() => break,
                r = child.wait() => {
                    debug!("visualiser {}: pmviz exited: {r:?}", w.id);
                    break;
                }
                _ = tick.tick() => {
                    let idle = {
                        let o = w.out.lock();
                        o.readers.is_empty()
                            && o.last_read.unwrap_or(w.created).elapsed() > IDLE
                    };
                    if idle {
                        info!("visualiser {}: nobody watching, stopping", w.id);
                        break;
                    }
                }
            }
        }
        let _ = child.kill().await;
        w.cancel.cancel();
        w.out.lock().ended = true;
        w.data.send_modify(|v| *v += 1);
        SESSIONS.lock().remove(&w.id);
    });
    Ok(s)
}

/// pmviz's log, at debug level (its summary at the end at info).
async fn log(id: String, stderr: tokio::process::ChildStderr) {
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.contains("failed") || line.contains("cannot") || line.contains("error") {
            warn!("visualiser {id}: {line}");
        } else {
            debug!("visualiser {id}: {line}");
        }
    }
}

enum Pcm {
    Start(u64),
    Data(Bytes),
}

/// Decode the tracks fed, one after another, into 32-bit float stereo for pmviz.
async fn decode(s: Arc<Session>, mut feed: mpsc::Receiver<Track>, pcm: mpsc::Sender<Pcm>) {
    let mut first = true;
    loop {
        let track = if first {
            first = false;
            tokio::select! {
                _ = s.cancel.cancelled() => return,
                t = feed.recv() => t,
            }
        } else {
            tokio::select! {
                _ = s.cancel.cancelled() => return,
                t = feed.recv() => t,
                // the end of the queue: let the stream end
                _ = tokio::time::sleep(NEXT_WITHIN) => None,
            }
        };
        let Some(track) = track else {
            debug!("visualiser {}: no more tracks", s.id);
            return; // dropping `pcm` closes pmviz's input: the stream ends
        };
        if pcm.send(Pcm::Start(track.song_id)).await.is_err() {
            return;
        }
        let mut resp = match track.source.await {
            Ok(Ok(r)) if r.status().is_success() => r,
            Ok(Ok(r)) => {
                warn!(
                    "visualiser {}: song {}: {}",
                    s.id,
                    track.song_id,
                    r.status()
                );
                (track.fetched)();
                continue;
            }
            other => {
                warn!("visualiser {}: song {}: {other:?}", s.id, track.song_id);
                (track.fetched)();
                continue;
            }
        };
        let child = tokio::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i", "pipe:0"])
            .args(["-f", "f32le", "-ac", "2", "-ar", &RATE.to_string(), "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                warn!("visualiser {}: cannot run ffmpeg: {e}", s.id);
                (track.fetched)();
                continue;
            }
        };
        // the track's stream into ffmpeg
        let mut input = child.stdin.take().expect("piped");
        let copy = crate::util::spawn(async move {
            while let Ok(Some(chunk)) = resp.chunk().await {
                if input.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        let mut out = child.stdout.take().expect("piped");
        loop {
            let mut buf = vec![0u8; PCM_CHUNK];
            let n = tokio::select! {
                _ = s.cancel.cancelled() => return,
                n = out.read(&mut buf) => n.unwrap_or(0),
            };
            if n == 0 {
                break;
            }
            buf.truncate(n);
            if pcm.send(Pcm::Data(buf.into())).await.is_err() {
                return;
            }
        }
        copy.abort();
        let _ = child.wait().await;
        (track.fetched)();
    }
}

/// Feed the decoded audio to pmviz, noting where each track starts.
async fn write(
    s: Arc<Session>,
    mut pcm: mpsc::Receiver<Pcm>,
    mut stdin: tokio::process::ChildStdin,
) {
    let mut fed: u64 = 0; // bytes
    let mut odd = Vec::new(); // a partial sample frame left over
    while let Some(p) = tokio::select! {
        _ = s.cancel.cancelled() => None,
        p = pcm.recv() => p,
    } {
        match p {
            Pcm::Start(song) => {
                // tracks start on a whole sample frame
                if !odd.is_empty() {
                    odd.resize(FRAME_BYTES as usize, 0);
                    if stdin.write_all(&odd).await.is_err() {
                        break;
                    }
                    fed += FRAME_BYTES;
                    odd.clear();
                }
                let at = (fed / FRAME_BYTES) as f64 / RATE as f64;
                s.timeline.lock().push((song, at));
                debug!("visualiser {}: song {song} starts at {at:.3} s", s.id);
            }
            Pcm::Data(b) => {
                let mut b = b.to_vec();
                if !odd.is_empty() {
                    odd.extend_from_slice(&b);
                    b = std::mem::take(&mut odd);
                }
                let whole = b.len() - b.len() % FRAME_BYTES as usize;
                odd = b.split_off(whole);
                if stdin.write_all(&b).await.is_err() {
                    break;
                }
                fed += whole as u64;
            }
        }
    }
    // closing pmviz's input ends the stream
}

/// Split pmviz's output into fragments for the readers, as fast as the slowest takes them.
async fn read(s: Arc<Session>, mut stdout: tokio::process::ChildStdout) {
    let mut split = Splitter::default();
    let mut buf = vec![0u8; 256 * 1024];
    let mut progress = s.progress.subscribe();
    loop {
        let n = tokio::select! {
            _ = s.cancel.cancelled() => 0,
            n = stdout.read(&mut buf) => n.unwrap_or(0),
        };
        if n == 0 {
            break;
        }
        for piece in split.push(&buf[..n]) {
            match piece {
                Piece::Init(init, scales) => {
                    let mut o = s.out.lock();
                    o.init = Some(init);
                    o.scales = scales;
                }
                Piece::Fragment(f) => {
                    // the slowest reader is too far behind (a paused TV): wait for it
                    loop {
                        let behind = {
                            let o = s.out.lock();
                            o.readers
                                .values()
                                .min()
                                .is_some_and(|slowest| o.next() - slowest >= KEEP as u64)
                        };
                        if !behind || s.cancel.is_cancelled() {
                            break;
                        }
                        tokio::select! {
                            _ = s.cancel.cancelled() => break,
                            _ = progress.changed() => {}
                        }
                    }
                    let mut o = s.out.lock();
                    o.frags.push_back(f);
                    while o.frags.len() > KEEP {
                        o.frags.pop_front();
                        o.base += 1;
                    }
                }
            }
        }
        s.data.send_modify(|v| *v += 1);
    }
    s.out.lock().ended = true;
    s.data.send_modify(|v| *v += 1);
}

impl Session {
    /// Feed `song_id` (played by `url`, absolute) after what is fed already; `fetched` is
    /// called once it is all decoded (the next track can come).  Its stream is requested
    /// now, as a player requests a stream when it is told to.
    pub fn feed(
        &self,
        http: &reqwest::Client,
        song_id: u64,
        url: String,
        fetched: impl FnOnce() + Send + 'static,
    ) {
        let (tx, source) = tokio::sync::oneshot::channel();
        let req = http.get(&url);
        crate::util::spawn(async move {
            let _ = tx.send(req.send().await);
        });
        let t = Track {
            song_id,
            source,
            fetched: Box::new(fetched),
        };
        if let Err(e) = self.feed.try_send(t) {
            warn!("visualiser {}: cannot queue a track: {e}", self.id);
        }
    }

    /// Where (seconds) in the stream `song_id` starts, once its audio has been fed.
    pub fn start_of(&self, song_id: u64) -> Option<f64> {
        self.timeline
            .lock()
            .iter()
            .rev()
            .find(|(s, _)| *s == song_id)
            .map(|(_, at)| *at)
    }

    /// Where (seconds) in the stream the stream read with `token` starts.
    pub fn joined_at(&self, token: &str) -> Option<f64> {
        self.out.lock().joined.get(token).copied()
    }

    pub fn stop(&self) {
        if !self.cancel.is_cancelled() {
            info!("visualiser {}: stopping", self.id);
        }
        self.cancel.cancel();
    }

    /// The stream for one reader (`token` names it, to tell where its stream starts).
    pub fn reader(
        self: &Arc<Self>,
        token: &str,
    ) -> impl futures::Stream<Item = std::io::Result<Bytes>> + Send + 'static {
        let id = {
            let mut o = self.out.lock();
            o.next_reader += 1;
            let id = o.next_reader;
            let start = o.next().saturating_sub(JOIN_BACK).max(o.base);
            o.readers.insert(id, start);
            o.last_read = Some(Instant::now());
            id
        };
        self.progress.send_modify(|v| *v += 1);
        let r = Reader {
            s: self.clone(),
            id,
            token: token.to_string(),
            sent_init: false,
            first: None,
            data: self.data.subscribe(),
        };
        futures::stream::unfold(r, |mut r| async move {
            let chunk = r.next().await?;
            Some((Ok(chunk), r))
        })
    }
}

struct Reader {
    s: Arc<Session>,
    id: u64,
    token: String,
    sent_init: bool,
    /// The reader's first fragment (its decode times are taken off all it gets).
    first: Option<Fragment>,
    data: watch::Receiver<u64>,
}

impl Reader {
    async fn next(&mut self) -> Option<Bytes> {
        loop {
            {
                let mut o = self.s.out.lock();
                if !self.sent_init {
                    if let Some(init) = o.init.clone() {
                        self.sent_init = true;
                        return Some(init);
                    }
                } else {
                    let want = *o.readers.get(&self.id)?;
                    // behind what is kept (only if the producer didn't wait): skip ahead
                    let want = want.max(o.base);
                    if want < o.next() {
                        let f = o.frags[(want - o.base) as usize].clone();
                        if self.first.is_none() {
                            let at = f.start(&o.scales);
                            o.joined.insert(self.token.clone(), at);
                            debug!(
                                "visualiser {}: {} joins at {at:.3} s",
                                self.s.id, self.token
                            );
                            self.first = Some(f.clone());
                        }
                        o.readers.insert(self.id, want + 1);
                        o.last_read = Some(Instant::now());
                        let first = self.first.as_ref().expect("set above");
                        let chunk = f.rebased(first);
                        drop(o);
                        self.s.progress.send_modify(|v| *v += 1);
                        return Some(chunk);
                    }
                }
                if o.ended {
                    return None;
                }
            }
            if self.data.changed().await.is_err() {
                return None;
            }
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let mut o = self.s.out.lock();
        o.readers.remove(&self.id);
        o.last_read = Some(Instant::now());
        drop(o);
        self.s.progress.send_modify(|v| *v += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert!(valid_size("1920x1080"));
        assert!(valid_size("1280x720"));
        assert!(!valid_size("1921x1080"));
        assert!(!valid_size("1920"));
        assert!(!valid_size("big"));
        assert!(!valid_size("10x10"));
    }
}
