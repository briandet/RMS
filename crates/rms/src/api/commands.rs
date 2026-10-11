//! Player commands.  Each one runs through the regular command engine, so players, Jive
//! clients and subscribers see exactly what they would for the equivalent LMS command.

use super::model::{Command, CommandResult, EnqueueMode, Item, PlayMode};
use crate::control::dispatch::execute_tokens;
use crate::control::request::{Request, Source, Status};
use crate::player::Player;
use crate::server::Server;
use std::sync::Arc;

#[derive(Debug)]
pub enum CommandError {
    /// The request can't be carried out as given (400).
    Invalid(String),
    /// The command engine refused it (422).
    Failed(String),
}

pub async fn run(
    server: &Arc<Server>,
    p: &Arc<Player>,
    cmd: Command,
) -> Result<CommandResult, CommandError> {
    let mut result = CommandResult::default();
    match cmd {
        Command::Play => {
            power_on(server, p).await?;
            let paused = p.controller().lock().mode() == "pause";
            exec(server, p, if paused { &["pause", "0"] } else { &["play"] }).await?;
        }
        Command::Pause => drop(exec(server, p, &["pause", "1"]).await?),
        Command::TogglePause => {
            let mode = super::state::player(server, p).mode;
            if mode == PlayMode::Play {
                exec(server, p, &["pause", "1"]).await?;
            } else {
                Box::pin(run(server, p, Command::Play)).await?;
            }
        }
        Command::Stop => drop(exec(server, p, &["stop"]).await?),
        Command::Next => drop(exec(server, p, &["playlist", "index", "+1"]).await?),
        Command::Previous => drop(exec(server, p, &["playlist", "index", "-1"]).await?),
        Command::Seek { position } => {
            if !position.is_finite() || position < 0.0 {
                return Err(CommandError::Invalid("position must be >= 0".into()));
            }
            exec(server, p, &["time", &position.to_string()]).await?;
        }
        Command::Volume { value } => {
            let v = value.clamp(0, 100).to_string();
            exec(server, p, &["mixer", "volume", &v]).await?;
        }
        Command::Mute { muted } => {
            let arg = match muted {
                None => "toggle",
                Some(true) => "1",
                Some(false) => "0",
            };
            exec(server, p, &["mixer", "muting", arg]).await?;
        }
        Command::Power { on } => {
            exec(server, p, &["power", if on { "1" } else { "0" }]).await?;
        }
        Command::Shuffle { mode } => {
            let m = mode.to_lms().to_string();
            exec(server, p, &["playlist", "shuffle", &m]).await?;
        }
        Command::Repeat { mode } => {
            let m = mode.to_lms().to_string();
            exec(server, p, &["playlist", "repeat", &m]).await?;
        }
        Command::Jump { index } => {
            check_index(p, index)?;
            exec(server, p, &["playlist", "index", &index.to_string()]).await?;
        }
        Command::Move { from, to } => {
            check_index(p, from)?;
            check_index(p, to)?;
            let (f, t) = (from.to_string(), to.to_string());
            exec(server, p, &["playlist", "move", &f, &t]).await?;
        }
        Command::Remove { index } => {
            check_index(p, index)?;
            exec(server, p, &["playlist", "delete", &index.to_string()]).await?;
        }
        Command::Clear => drop(exec(server, p, &["playlist", "clear"]).await?),
        Command::Enqueue { item, mode, start } => {
            if mode == EnqueueMode::Play {
                power_on(server, p).await?;
            }
            let r = match &item {
                Item::Favorite { id } | Item::Spotify { id } => {
                    let service = if matches!(item, Item::Favorite { .. }) {
                        "favorites"
                    } else {
                        "spotify"
                    };
                    let verb = match mode {
                        EnqueueMode::Play => "play",
                        EnqueueMode::Add => "add",
                        EnqueueMode::Next => "insert",
                    };
                    let args = vec![
                        service.to_string(),
                        "playlist".into(),
                        verb.into(),
                        format!("item_id:{}", id),
                    ];
                    exec_owned(server, p, args).await?
                }
                Item::Url { url } => {
                    let verb = match mode {
                        EnqueueMode::Play => "play",
                        EnqueueMode::Add => "add",
                        EnqueueMode::Next => "insert",
                    };
                    exec(server, p, &["playlist", verb, url]).await?
                }
                _ => {
                    let verb = match mode {
                        EnqueueMode::Play => "cmd:load",
                        EnqueueMode::Add => "cmd:add",
                        EnqueueMode::Next => "cmd:insert",
                    };
                    let mut args = vec!["playlistcontrol".to_string(), verb.into(), param(&item)];
                    if let (EnqueueMode::Play, Some(i)) = (mode, start) {
                        args.push(format!("play_index:{}", i));
                    }
                    exec_owned(server, p, args).await?
                }
            };
            result.count = count(&r);
        }
        Command::Mix { seed } => {
            if matches!(
                seed,
                Item::Playlist { .. }
                    | Item::Folder { .. }
                    | Item::Url { .. }
                    | Item::Favorite { .. }
                    | Item::Spotify { .. }
            ) {
                return Err(CommandError::Invalid(
                    "mixes start from a track, album, artist, genre or year".into(),
                ));
            }
            power_on(server, p).await?;
            let args = vec!["createmix".into(), "play".into(), param(&seed)];
            let r = exec_owned(server, p, args).await?;
            result.count = Some(count(&r).unwrap_or(0));
        }
        Command::Sync { with } => {
            let Some(other) = server.resolve_client(&with) else {
                return Err(CommandError::Invalid(format!("unknown player {}", with)));
            };
            if other.id == p.id {
                return Err(CommandError::Invalid(
                    "a player can't sync with itself".into(),
                ));
            }
            exec(server, p, &["sync", &other.id.to_string()]).await?;
        }
        Command::Unsync => drop(exec(server, p, &["sync", "-"]).await?),
        Command::Rename { name } => {
            let name = name.trim();
            if name.is_empty() {
                return Err(CommandError::Invalid("name must not be empty".into()));
            }
            exec(server, p, &["name", name]).await?;
        }
    }
    Ok(result)
}

/// `playlistcontrol` / `createmix` parameter for an item.
fn param(item: &Item) -> String {
    match item {
        Item::Track { id } => format!("track_id:{}", id),
        Item::Album { id } => format!("album_id:{}", id),
        Item::Artist { id } => format!("artist_id:{}", id),
        Item::Genre { id } => format!("genre_id:{}", id),
        Item::Year { year } => format!("year:{}", year),
        Item::Playlist { id } => format!("playlist_id:{}", id),
        Item::Folder { id } => format!("folder_id:{}", id),
        Item::Url { url } => format!("url:{}", url),
        Item::Favorite { id } | Item::Spotify { id } => format!("item_id:{}", id),
    }
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

/// Turn the player on before something new plays, without resuming what it played before.
async fn power_on(server: &Arc<Server>, p: &Arc<Player>) -> Result<(), CommandError> {
    if !p.power() {
        exec(server, p, &["power", "1", "1"]).await?;
    }
    Ok(())
}

async fn exec(
    server: &Arc<Server>,
    p: &Arc<Player>,
    words: &[&str],
) -> Result<Request, CommandError> {
    exec_owned(server, p, words.iter().map(|s| s.to_string()).collect()).await
}

async fn exec_owned(
    server: &Arc<Server>,
    p: &Arc<Player>,
    words: Vec<String>,
) -> Result<Request, CommandError> {
    exec_for(server, Some(p), words).await
}

/// Run an LMS command for a player, or for the server without one.
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
