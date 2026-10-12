//! Announcements: play a short clip on a player (a doorbell, "dinner is ready" from a speech
//! synthesizer), then put back what it was doing — the queue as it was (shuffle order and
//! all), the track and position, playing or not, and each sync group member's power and
//! volume.
//!
//! Interrupt and resume: the music stops for the clip and continues from where it was.  An
//! announcement goes to the player's whole sync group, which shares one queue.  If someone
//! changes the queue while the clip plays, they win: nothing is restored over their choice.
//! A paused player comes back stopped at the track it was on (resuming a paused position
//! would mean playing it for a moment).
//!
//! The clip is written to `<data dir>/announce/` and played like any file, so every kind of
//! player can play it (transcoded when needed); the file is deleted when it is done.

use super::command::{run, Change, PlayerCommand};
use super::controller::Controller;
use super::playlist::{Entry, Playlist};
use super::Player;
use crate::server::Server;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Largest clip accepted.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;
/// How long a clip may take to start playing.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a clip may play at most (when its length is unknown, or it overruns).
const MAX_LENGTH: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(200);

/// Sync groups (by their first player) with an announcement under way.
static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

#[derive(Debug, PartialEq)]
pub enum AnnounceError {
    /// Not something to play (bad type, empty, too big).
    Invalid(String),
    /// Another announcement is playing in the group.
    Busy,
    Failed(String),
}

/// The file extension for an audio content type, or `None` for something else.
pub fn extension(content_type: &str) -> Option<&'static str> {
    let t = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match t.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" | "audio/vorbis" | "application/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/mp4" | "audio/aac" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aiff" | "audio/x-aiff" => "aiff",
        _ => return None,
    })
}

/// What to put back.
struct Saved {
    playlist: Playlist,
    /// Queue entry (in play order) that was playing, and how far in.
    index: usize,
    elapsed: f64,
    playing: bool,
    /// Each group member: power and volume.
    members: Vec<(Arc<Player>, bool, i32)>,
}

fn group_key(c: &Controller) -> String {
    c.all_players()
        .iter()
        .map(|p| p.id.to_string())
        .min()
        .unwrap_or_default()
}

fn claim(key: &str) -> bool {
    BUSY.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(key.to_string())
}

fn release(key: &str) {
    if let Some(s) = BUSY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        s.remove(key);
    }
}

/// Play `clip` (audio of type `content_type`) on `p`'s sync group at `volume` (0–100, or the
/// players' own), then restore.  Returns once the clip has been queued; the rest happens in
/// the background.
pub fn announce(
    server: &Arc<Server>,
    p: &Arc<Player>,
    clip: &[u8],
    content_type: &str,
    volume: Option<i32>,
) -> Result<(), AnnounceError> {
    let ext = extension(content_type).ok_or_else(|| {
        AnnounceError::Invalid(format!(
            "not an audio type: {content_type:?} (mp3, wav, flac, ogg, opus, m4a or aiff)"
        ))
    })?;
    if clip.is_empty() {
        return Err(AnnounceError::Invalid("the clip is empty".into()));
    }
    if clip.len() > MAX_BYTES {
        return Err(AnnounceError::Invalid(format!(
            "the clip is larger than {} MB",
            MAX_BYTES / (1024 * 1024)
        )));
    }
    if let Some(v) = volume {
        if !(0..=100).contains(&v) {
            return Err(AnnounceError::Invalid("volume is 0–100".into()));
        }
    }
    let cc = p.controller();
    let key = group_key(&cc.lock());
    if !claim(&key) {
        return Err(AnnounceError::Busy);
    }
    let dir = server.config.data_dir.join("announce");
    let path = dir.join(format!("{:016x}.{ext}", rand::random::<u64>()));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, clip)) {
        release(&key);
        return Err(AnnounceError::Failed(format!("cannot keep the clip: {e}")));
    }
    let url = rms_library::url::file_url_from_path(&path);

    let saved = {
        let c = cc.lock();
        Saved {
            playlist: c.playlist.clone(),
            index: c.playing_song_index(),
            elapsed: c.playing_song_elapsed(),
            playing: c.is_playing(false),
            members: c
                .all_players()
                .into_iter()
                .map(|m| {
                    let (power, vol) = (m.power(), m.volume());
                    (m, power, vol)
                })
                .collect(),
        }
    };
    info!(
        "{}: announcement ({} bytes), then back to entry {} at {:.1}s",
        p.id,
        clip.len(),
        saved.index,
        saved.elapsed
    );
    if let Some(v) = volume {
        for (m, _, _) in &saved.members {
            let _ = run(server, m, PlayerCommand::Volume(Change::To(v)));
        }
    }
    let queued = run(
        server,
        p,
        PlayerCommand::Enqueue {
            entries: vec![Entry::new(url.clone())],
            how: super::command::Enqueue::Play {
                start: None,
                fade: None,
            },
        },
    );
    if let Err(e) = queued {
        restore(server, p, &saved);
        finish(&key, &path);
        return Err(AnnounceError::Failed(format!(
            "cannot play the clip: {e:?}"
        )));
    }
    // play it once, whatever the group's repeat setting (the saved playlist keeps that)
    cc.lock().playlist.repeat = 0;

    let (server, p) = (server.clone(), p.clone());
    tokio::spawn(async move {
        watch(&server, &p, &url, &saved).await;
        finish(&key, &path);
    });
    Ok(())
}

fn finish(key: &str, path: &PathBuf) {
    release(key);
    let _ = std::fs::remove_file(path);
}

/// Wait for the clip to end, then restore — unless the queue was changed meanwhile.
async fn watch(server: &Arc<Server>, p: &Arc<Player>, url: &str, saved: &Saved) {
    let began = Instant::now();
    let mut started: Option<Instant> = None;
    loop {
        tokio::time::sleep(POLL).await;
        let (still_ours, stopped, playing, length) = {
            let cc = p.controller();
            let c = cc.lock();
            let ours = c.playlist.len() == 1 && c.playlist.entries[0].url == url;
            (
                ours,
                c.is_stopped(),
                c.is_playing(false),
                c.playing_song_duration(),
            )
        };
        if !still_ours {
            debug!(
                "{}: queue changed during the announcement; not restoring",
                p.id
            );
            return;
        }
        if playing && started.is_none() {
            started = Some(Instant::now());
        }
        let overran = match started {
            None => began.elapsed() > START_TIMEOUT,
            Some(s) => {
                let limit = length
                    .map(|d| Duration::from_secs_f64(d.max(0.0)) + Duration::from_secs(30))
                    .unwrap_or(MAX_LENGTH)
                    .min(MAX_LENGTH);
                s.elapsed() > limit
            }
        };
        if overran {
            warn!("{}: announcement did not finish in time; restoring", p.id);
            let _ = run(server, p, PlayerCommand::Stop);
            break;
        }
        if started.is_some() && stopped {
            break;
        }
    }
    restore(server, p, saved);
}

/// Put back the queue, the track and position, power and volume.
fn restore(server: &Arc<Server>, p: &Arc<Player>, saved: &Saved) {
    let cc = p.controller();
    {
        let mut c = cc.lock();
        c.queue.clear();
        c.playlist = saved.playlist.clone();
        super::command::playlist_changed(&mut c);
        if saved.playlist.is_empty() {
            // nothing was queued: leave it empty and stopped
        } else {
            let index = saved.index.min(saved.playlist.len() - 1);
            if saved.playing {
                let seek = (saved.elapsed > 1.0).then_some(saved.elapsed);
                c.play(Some(index), seek, None);
            } else {
                c.reset_song_queue(Some(index));
            }
        }
    }
    server
        .notifier
        .notify(Some(p.id), &["playlist", "jump", &saved.index.to_string()]);
    for (m, power, volume) in &saved.members {
        if m.volume() != *volume {
            let _ = run(server, m, PlayerCommand::Volume(Change::To(*volume)));
        }
        if !*power && m.power() {
            let _ = run(
                server,
                m,
                PlayerCommand::Power {
                    on: Some(false),
                    noplay: true,
                },
            );
        }
    }
    info!("{}: announcement done, restored", p.id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_types() {
        assert_eq!(extension("audio/mpeg"), Some("mp3"));
        assert_eq!(extension("Audio/WAV; codecs=1"), Some("wav"));
        assert_eq!(extension("audio/ogg"), Some("ogg"));
        assert_eq!(extension("image/png"), None);
        assert_eq!(extension(""), None);
    }

    #[test]
    fn one_announcement_per_group() {
        assert!(claim("test-group"));
        assert!(!claim("test-group"));
        release("test-group");
        assert!(claim("test-group"));
        release("test-group");
    }
}
