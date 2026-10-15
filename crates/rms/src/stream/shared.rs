//! One source for a sync group (`Slim::Player::Source` feeding the sync slaves' chunk
//! queues): the source is opened and read once, and every player's `/stream.mp3` gets the
//! same bytes.  The source is read only as fast as the slowest player takes it.

use super::source::ByteStream;
use super::StreamSpec;
use bytes::Bytes;
use futures::stream::{self, StreamExt};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};
use tracing::debug;

/// How far (bytes) the source may get ahead of the slowest player.
const AHEAD: usize = 4 << 20;

/// How long to wait for every player of the group to connect before reading on without
/// the missing ones.
const ATTACH_GRACE: Duration = Duration::from_secs(10);

pub struct SharedSource {
    state: Mutex<State>,
    /// Bumped when a chunk arrives or the source ends (wakes the readers).
    data: watch::Sender<u64>,
    /// Bumped when a reader moves on, connects or leaves (wakes the producer).
    progress: watch::Sender<u64>,
    /// Players the stream was started on.
    expected: usize,
    deadline: Instant,
}

#[derive(Default)]
struct State {
    chunks: VecDeque<Bytes>,
    /// Index of `chunks[0]`.
    base: u64,
    /// Bytes in `chunks`.
    buffered: usize,
    /// Reader id → index of the next chunk it reads.
    readers: HashMap<u64, u64>,
    /// Readers that ever connected (ids are `0..attached`).
    attached: usize,
    started: bool,
    end: Option<Result<(), String>>,
}

impl State {
    fn next(&self) -> u64 {
        self.base + self.chunks.len() as u64
    }
}

impl std::fmt::Debug for SharedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedSource({} players)", self.expected)
    }
}

impl SharedSource {
    pub fn new(players: usize) -> Arc<SharedSource> {
        Arc::new(SharedSource {
            state: Mutex::new(State::default()),
            data: watch::channel(0).0,
            progress: watch::channel(0).0,
            expected: players,
            deadline: Instant::now() + ATTACH_GRACE,
        })
    }

    /// Still waiting for players that have not connected yet.
    fn waiting(&self, st: &State) -> bool {
        st.attached < self.expected && Instant::now() < self.deadline
    }

    /// Nobody is reading and nobody else will.
    fn abandoned(&self, st: &State) -> bool {
        st.readers.is_empty() && !self.waiting(st)
    }

    /// Drop the chunks every reader has passed (none while players may still connect:
    /// they start from the beginning).
    fn trim(&self, st: &mut State) {
        if self.waiting(st) {
            return;
        }
        let min = st.readers.values().copied().min().unwrap_or(st.next());
        while st.base < min {
            let Some(c) = st.chunks.pop_front() else {
                break;
            };
            st.buffered -= c.len();
            st.base += 1;
        }
    }

    /// The stream for one player, from the first byte.  The first call opens the source.
    pub fn subscribe(self: &Arc<Self>, spec: &StreamSpec, http: &reqwest::Client) -> ByteStream {
        let id = {
            let mut st = self.state.lock();
            let id = st.attached as u64;
            st.attached += 1;
            let base = st.base;
            st.readers.insert(id, base);
            if !st.started {
                st.started = true;
                let mut spec = spec.clone();
                spec.shared = None;
                crate::util::spawn(produce(self.clone(), spec, http.clone()));
            }
            id
        };
        debug!(
            "shared stream: player {} of {} connected",
            id + 1,
            self.expected
        );
        self.progress.send_modify(|v| *v += 1);

        struct Reader {
            shared: Arc<SharedSource>,
            id: u64,
            pos: u64,
            rx: watch::Receiver<u64>,
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                let mut st = self.shared.state.lock();
                st.readers.remove(&self.id);
                self.shared.trim(&mut st);
                drop(st);
                self.shared.progress.send_modify(|v| *v += 1);
            }
        }
        let reader = Reader {
            shared: self.clone(),
            id,
            pos: 0,
            rx: self.data.subscribe(),
        };
        stream::unfold(reader, |mut r| async move {
            loop {
                r.rx.borrow_and_update();
                let item = {
                    let shared = r.shared.clone();
                    let mut st = shared.state.lock();
                    if r.pos < st.base {
                        // only after the grace period, for a player that connected late
                        Some(Err(std::io::Error::other(
                            "too far behind the synced players",
                        )))
                    } else if r.pos < st.next() {
                        let chunk = st.chunks[(r.pos - st.base) as usize].clone();
                        r.pos += 1;
                        st.readers.insert(r.id, r.pos);
                        shared.trim(&mut st);
                        drop(st);
                        shared.progress.send_modify(|v| *v += 1);
                        Some(Ok(chunk))
                    } else {
                        match &st.end {
                            Some(Ok(())) => return None,
                            Some(Err(e)) => Some(Err(std::io::Error::other(e.clone()))),
                            None => None,
                        }
                    }
                };
                if let Some(item) = item {
                    return Some((item, r));
                }
                if r.rx.changed().await.is_err() {
                    return None;
                }
            }
        })
        .boxed()
    }

    fn finish(&self, end: Result<(), String>) {
        self.state.lock().end = Some(end);
        self.data.send_modify(|v| *v += 1);
    }
}

/// Read the source into the shared buffer while there is room and someone listening.
async fn produce(shared: Arc<SharedSource>, spec: StreamSpec, http: reqwest::Client) {
    let mut src = match super::source::open(&spec, &http).await {
        Ok(s) => super::spectrum::tap(s, &spec),
        Err(e) => return shared.finish(Err(e.to_string())),
    };
    let mut progress = shared.progress.subscribe();
    loop {
        // wait for room
        loop {
            progress.borrow_and_update();
            {
                let mut st = shared.state.lock();
                // (the grace period may have just ended)
                shared.trim(&mut st);
                if shared.abandoned(&st) {
                    debug!("shared stream: no players left");
                    return;
                }
                if st.buffered < AHEAD {
                    break;
                }
            }
            let deadline = shared.deadline;
            tokio::select! {
                r = progress.changed() => if r.is_err() { return },
                _ = tokio::time::sleep_until(deadline), if Instant::now() < deadline => {}
            }
        }
        match src.next().await {
            Some(Ok(chunk)) => {
                let mut st = shared.state.lock();
                st.buffered += chunk.len();
                st.chunks.push_back(chunk);
                drop(st);
                shared.data.send_modify(|v| *v += 1);
            }
            Some(Err(e)) => return shared.finish(Err(e.to_string())),
            None => return shared.finish(Ok(())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::SourceKind;

    /// A spec reading `path` (a plain file source).
    fn spec(path: &std::path::Path, shared: &Arc<SharedSource>) -> StreamSpec {
        StreamSpec {
            source: SourceKind::File {
                path: path.to_path_buf(),
                offset: 0,
                length: None,
            },
            format: "pcm".into(),
            song_id: 1,
            shared: Some(shared.clone()),
        }
    }

    async fn collect(mut s: ByteStream) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = s.next().await {
            out.extend_from_slice(&c.unwrap());
        }
        out
    }

    fn test_file(len: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.raw");
        let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        (dir, path, data)
    }

    #[tokio::test]
    async fn every_player_gets_the_whole_stream() {
        let (_dir, path, data) = test_file(10 * AHEAD + 123);
        let shared = SharedSource::new(2);
        let http = reqwest::Client::new();
        let sp = spec(&path, &shared);
        let a = shared.subscribe(&sp, &http);
        let b = shared.subscribe(&sp, &http);
        let (a, b) = tokio::join!(collect(a), collect(b));
        assert_eq!(a, data);
        assert_eq!(b, data);
        // nothing left behind
        assert_eq!(shared.state.lock().buffered, 0);
    }

    #[tokio::test]
    async fn the_slowest_player_sets_the_pace() {
        let (_dir, path, data) = test_file(4 * AHEAD);
        let shared = SharedSource::new(2);
        let http = reqwest::Client::new();
        let sp = spec(&path, &shared);
        let mut slow = shared.subscribe(&sp, &http);
        let fast = shared.subscribe(&sp, &http);
        let fast = crate::util::spawn(collect(fast));
        // the slow player takes one chunk; the fast one gets at most AHEAD past it
        let first = slow.next().await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!fast.is_finished());
        let buffered = shared.state.lock().buffered;
        assert!(
            buffered <= AHEAD + super::super::source::CHUNK,
            "{buffered}"
        );
        let mut rest = first.to_vec();
        rest.extend(collect(slow).await);
        assert_eq!(rest, data);
        assert_eq!(fast.await.unwrap(), data);
    }

    #[tokio::test]
    async fn a_player_leaving_does_not_stop_the_others() {
        let (_dir, path, data) = test_file(3 * AHEAD);
        let shared = SharedSource::new(2);
        let http = reqwest::Client::new();
        let sp = spec(&path, &shared);
        let mut leaving = shared.subscribe(&sp, &http);
        let staying = shared.subscribe(&sp, &http);
        leaving.next().await.unwrap().unwrap();
        drop(leaving);
        assert_eq!(collect(staying).await, data);
    }

    #[tokio::test(start_paused = true)]
    async fn waits_for_missing_players_then_goes_on() {
        let (_dir, path, data) = test_file(2 * AHEAD);
        let shared = SharedSource::new(2);
        let http = reqwest::Client::new();
        let sp = spec(&path, &shared);
        // the second player never connects: the first is held at AHEAD until the grace ends
        let only = crate::util::spawn(collect(shared.subscribe(&sp, &http)));
        tokio::time::sleep(ATTACH_GRACE / 2).await;
        assert!(!only.is_finished());
        assert!(shared.state.lock().buffered <= AHEAD + super::super::source::CHUNK);
        tokio::time::sleep(ATTACH_GRACE).await;
        assert_eq!(only.await.unwrap(), data);
    }

    #[tokio::test]
    async fn a_failing_source_fails_every_player() {
        let shared = SharedSource::new(2);
        let http = reqwest::Client::new();
        let sp = spec(std::path::Path::new("/nonexistent/audio"), &shared);
        let mut a = shared.subscribe(&sp, &http);
        let mut b = shared.subscribe(&sp, &http);
        assert!(a.next().await.unwrap().is_err());
        assert!(b.next().await.unwrap().is_err());
    }
}
