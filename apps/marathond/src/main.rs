//! `marathond`: the Marathon server daemon.
//!
//! ```text
//! marathond --config path/to/config.toml
//! ```
//!
//! Logging is controlled with `RUST_LOG` (default `info`).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use marathon_server::Server;
use marathon_server::config::Config;
use tracing_subscriber::EnvFilter;

/// Serves Marathon channels over HTTP.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the TOML config file (see config/example.toml).
    #[arg(long, short, value_name = "PATH")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "marathond starting");

    let config = Config::load(&args.config)?;
    let server = Server::bind(config)
        .await
        .context("failed to start the server")?;
    println!("marathond listening on http://{}", server.local_addr());
    server.serve(shutdown_signal()).await?;
    Ok(())
}

/// Completes on Ctrl-C, or on SIGTERM on Unix.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
