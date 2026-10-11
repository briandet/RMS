//! Player commands of the native API, run by the typed command core
//! ([`crate::player::command`]) like the LMS commands that do the same.  Listeners of the
//! LMS interfaces are notified with that LMS command, so they see what they always saw.

use super::model::{Command, CommandResult, EnqueueMode, Item};
use crate::control::dispatch::execute_tokens;
use crate::control::request::{Request, Source, Status};
use crate::player::command::{self as core, Change, Enqueue, PlayerCommand, Position};
use crate::player::Player;
use crate::server::Server;
use std::sync::Arc;

#[derive(Debug)]
pub enum CommandError {
    /// The request can't be carried out as given (400).
    Invalid(String),
    /// The server could not carry it out (422).
    Failed(String),
}

impl From<core::CommandError> for CommandError {
    fn from(e: core::CommandError) -> Self {
        match e {
            core::CommandError::Invalid(m) => CommandError::Invalid(m),
        }
    }
}

pub async fn run(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: Command,
) -> Result<CommandResult, CommandError> {
    if !p.is_connected() && !matches!(cmd, Command::Rename { .. }) {
        return Err(CommandError::Failed("player is not connected".into()));
    }
    let jump = |to| PlayerCommand::Jump {
        to,
        fade: None,
        noplay: false,
        seek: None,
    };
    let typed = match cmd {
        Command::Play => PlayerCommand::Play { fade: None },
        Command::Pause => PlayerCommand::Pause,
        Command::TogglePause => PlayerCommand::TogglePause { fade: None },
        Command::Stop => PlayerCommand::Stop,
        Command::Next => jump(Position::By(1)),
        Command::Previous => jump(Position::By(-1)),
        Command::Seek { position } => {
            if !position.is_finite() || position < 0.0 {
                return Err(CommandError::Invalid("position must be >= 0".into()));
            }
            PlayerCommand::Seek(Change::To(position))
        }
        Command::Volume { value } => PlayerCommand::Volume(Change::To(value.clamp(0, 100))),
        Command::Mute { muted } => PlayerCommand::Mute(muted),
        Command::Power { on } => PlayerCommand::Power {
            on: Some(on),
            noplay: false,
        },
        Command::Shuffle { mode } => PlayerCommand::Shuffle(Some(mode)),
        Command::Repeat { mode } => PlayerCommand::Repeat(Some(mode)),
        Command::Jump { index } => {
            check_index(p, index)?;
            jump(Position::Index(index))
        }
        Command::Move { from, to } => PlayerCommand::Move { from, to },
        Command::Remove { index } => PlayerCommand::Remove { index },
        Command::Clear => PlayerCommand::Clear,
        Command::Rename { name } => PlayerCommand::Rename(name.trim().to_string()),
        Command::Sync { with } => {
            let Some(with) = server.resolve_client(&with) else {
                return Err(CommandError::Invalid(format!("unknown player {}", with)));
            };
            PlayerCommand::Sync {
                with,
                no_restart: false,
            }
        }
        Command::Unsync => PlayerCommand::Unsync,
        Command::Enqueue { item, mode, start } => {
            let how = match mode {
                EnqueueMode::Play => Enqueue::Play { start, fade: None },
                EnqueueMode::Add => Enqueue::Add,
                EnqueueMode::Next => Enqueue::Next,
            };
            let entries = match item {
                // a Spotify menu item plays as its menu says
                Item::Spotify { id } => return spotify(server, p, &id, mode).await,
                other => core::entries_for(server, &core_item(other)).await,
            };
            PlayerCommand::Enqueue { entries, how }
        }
        Command::Mix { seed } => {
            use rms_library::mix::Seed;
            let seed = match seed {
                Item::Track { id } => Seed::Track(id),
                Item::Album { id } => Seed::Album(id),
                Item::Artist { id } => Seed::Artist(id),
                Item::Genre { id } => Seed::Genre(id),
                Item::Year { year } => Seed::Year(year),
                _ => {
                    return Err(CommandError::Invalid(
                        "mixes start from a track, album, artist, genre or year".into(),
                    ))
                }
            };
            let ids = crate::plugins::dstm::mix_tracks(server, p, seed).await;
            let entries = core::entries_for(server, &core::Item::Tracks(ids)).await;
            let how = Enqueue::Play {
                start: None,
                fade: None,
            };
            let out = execute(server, p, PlayerCommand::Enqueue { entries, how })?;
            return Ok(CommandResult {
                count: Some(out.count.unwrap_or(0) as u64),
            });
        }
    };
    let out = execute(server, p, typed)?;
    Ok(CommandResult {
        count: out.count.map(|n| n as u64),
    })
}

/// Run a typed command and notify LMS listeners of it.
fn execute(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: PlayerCommand,
) -> Result<core::Outcome, CommandError> {
    let words = cmd.lms_words();
    let out = core::run(server, p, cmd)?;
    if out.changed {
        server.notifier.notify_owned(Some(p.id), words);
    }
    Ok(out)
}

fn core_item(item: Item) -> core::Item {
    match item {
        Item::Track { id } => core::Item::Track(id),
        Item::Album { id } => core::Item::Album(id),
        Item::Artist { id } => core::Item::Artist(id),
        Item::Genre { id } => core::Item::Genre(id),
        Item::Year { year } => core::Item::Year(year),
        Item::Playlist { id } => core::Item::Playlist(id),
        Item::Folder { id } => core::Item::Folder(id),
        Item::Url { url } => core::Item::Url(url),
        Item::Favorite { id } => core::Item::Favorite(id),
        Item::Spotify { id } => core::Item::Url(id),
    }
}

/// A Spotify menu item, queued as the Spotify menus queue it.
async fn spotify(
    server: &Arc<Server>,
    p: &Arc<Player>,
    id: &str,
    mode: EnqueueMode,
) -> Result<CommandResult, CommandError> {
    let verb = match mode {
        EnqueueMode::Play => "play",
        EnqueueMode::Add => "add",
        EnqueueMode::Next => "insert",
    };
    let words = ["spotify", "playlist", verb, &format!("item_id:{id}")]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let r = exec_for(server, Some(p), words).await?;
    Ok(CommandResult { count: count(&r) })
}

fn count(r: &Request) -> Option<u64> {
    let v = r.results.get("count")?;
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn check_index(p: &Arc<Player>, index: usize) -> Result<(), CommandError> {
    let len = p.controller().lock().playlist.len();
    if index >= len {
        return Err(CommandError::Invalid(format!(
            "index {} is out of range (queue has {} tracks)",
            index, len
        )));
    }
    Ok(())
}

/// Run an LMS command for a player, or for the server without one (service menus and
/// settings, which live in the LMS command set).
pub async fn exec_for(
    server: &Arc<Server>,
    p: Option<&Arc<Player>>,
    words: Vec<String>,
) -> Result<Request, CommandError> {
    let summary = words.first().cloned().unwrap_or_default();
    let r = execute_tokens(server, p.map(|p| p.id), words, Source::Web).await;
    match r.status {
        Status::Done | Status::Dispatchable => Ok(r),
        Status::NeedsClient => Err(CommandError::Failed("player is not connected".into())),
        s => Err(CommandError::Failed(format!(
            "{} failed ({:?})",
            summary, s
        ))),
    }
}
