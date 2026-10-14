//! Rusty Music Server: a Rust implementation of the Lyrion Music Server (formerly Logitech
//! Media Server / SqueezeCenter) protocols for Squeezebox-compatible players.

mod api;
mod bridge;
mod config;
mod control;
mod discovery;
mod display;
mod notify;
mod player;
mod plugins;
mod prefs;
mod server;
mod stream;
mod strings;
mod subsonic;
mod tagedit;
mod ui;
mod upload;
mod util;
mod watch;
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&config.log)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    info!(
        "Rusty Music Server {} (LMS protocol {})",
        env!("CARGO_PKG_VERSION"),
        VERSION
    );

    let server = server::Server::new(config.clone())?;
    if !config.scan_only {
        if let Some(sp) = crate::plugins::spotify::get() {
            sp.attach(&server);
        }
        crate::plugins::spotify::connect::start(&server);
    }
    let dirs = server.prefs.server.get_string_list("mediadirs");
    if dirs.is_empty() {
        info!("no music folder configured; set one with --music <dir> or in the web UI settings");
    } else {
        info!("music folders: {}", dirs.join(", "));
    }

    if config.scan_only || config.rescan || config.wipe {
        let mode = if config.wipe {
            rms_library::ScanMode::Wipe
        } else {
            rms_library::ScanMode::Full
        };
        if config.scan_only {
            let lib = server.library.clone();
            let summary = crate::util::spawn_blocking(move || lib.scan(mode)).await??;
            info!("scan finished: {:?}", summary);
            return Ok(());
        }
        control::commands::start_scan(&server, mode);
    } else if server.library.totals().songs == 0 && !dirs.is_empty() {
        info!("library is empty, starting initial scan");
        control::commands::start_scan(&server, rms_library::ScanMode::Full);
    }
    // EffNet's models, when that analysis is on (then it starts the analysis itself)
    crate::plugins::effnet::apply(&server);
    // tracks left unanalysed last time (a scan starts the analysis when it finishes)
    if !server.scanning.load(std::sync::atomic::Ordering::SeqCst) {
        crate::plugins::sonic::spawn_analysis(&server);
    }

    spawn("slimproto", player::connection::listen(server.clone()));
    if !config.no_discovery {
        spawn("discovery", discovery::run(server.clone()));
    }
    spawn("cli", control::cli::listen(server.clone()));
    spawn("http", web::serve(server.clone()));
    crate::util::spawn(control::subscribe::pump(server.clone()));
    crate::util::spawn(api::events::run(server.clone()));
    crate::util::spawn(web::cometd::run(server.clone()));
    crate::util::spawn(plugins::alarm::run(server.clone()));
    crate::util::spawn(plugins::dstm::run(server.clone()));
    crate::util::spawn(plugins::musicbrainz::run(server.clone()));
    crate::util::spawn(plugins::scrobble::run(server.clone()));
    crate::util::spawn(display::run(server.clone()));
    crate::util::spawn(control::jive::run(server.clone()));
    watch::start(&server);
    bridge::devices::start(&server);

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
    crate::util::spawn(async move {
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
