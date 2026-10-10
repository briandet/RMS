//! Rusty Music Server: a Rust implementation of the Lyrion Music Server (formerly Logitech
//! Media Server / SqueezeCenter) protocols for Squeezebox-compatible players.

mod config;
mod control;
mod discovery;
mod notify;
mod player;
mod plugins;
mod prefs;
mod server;
mod stream;
mod strings;
mod util;
mod web;

use clap::Parser;
use std::sync::Arc;
use tracing::{error, info};

/// Version reported to players and controllers.  Clients gate features on the LMS version,
/// so we report the Lyrion release whose protocol we implement.
pub const VERSION: &str = "9.2.0";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = config::Config::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&config.log).unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();
    info!("Rusty Music Server {} (LMS protocol {})", env!("CARGO_PKG_VERSION"), VERSION);

    let server = server::Server::new(config.clone())?;
    let dirs = server.prefs.server.get_string_list("mediadirs");
    if dirs.is_empty() {
        info!("no music folder configured; set one with --music <dir> or in the web UI settings");
    } else {
        info!("music folders: {}", dirs.join(", "));
    }

    if config.scan_only || config.rescan || config.wipe {
        let mode = if config.wipe { rms_library::ScanMode::Wipe } else { rms_library::ScanMode::Full };
        if config.scan_only {
            let lib = server.library.clone();
            let summary = tokio::task::spawn_blocking(move || lib.scan(mode)).await??;
            info!("scan finished: {:?}", summary);
            return Ok(());
        }
        control::commands::start_scan(&server, mode);
    } else if server.library.totals().songs == 0 && !dirs.is_empty() {
        info!("library is empty, starting initial scan");
        control::commands::start_scan(&server, rms_library::ScanMode::Full);
    }

    spawn("slimproto", player::connection::listen(server.clone()));
    spawn("discovery", discovery::run(server.clone()));
    spawn("cli", control::cli::listen(server.clone()));
    spawn("http", web::serve(server.clone()));
    tokio::spawn(control::subscribe::pump(server.clone()));
    tokio::spawn(web::cometd::run(server.clone()));
    tokio::spawn(plugins::alarm::run(server.clone()));

    let shutdown = server.shutdown.clone();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("interrupted"),
        _ = shutdown.cancelled() => info!("shutdown requested"),
    }
    shutdown_players(&server);
    Ok(())
}

fn spawn<F>(name: &'static str, fut: F)
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            error!("{} failed: {:#}", name, e);
            std::process::exit(1);
        }
    });
}

fn shutdown_players(server: &Arc<server::Server>) {
    for p in server.players() {
        let c = p.controller();
        c.lock().save_playlist();
    }
}
