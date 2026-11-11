//! `marathond`: the Marathon server daemon.
//!
//! ```text
//! marathond --config path/to/config.toml [--bind 0.0.0.0:7878]
//! marathond --print-openapi > openapi.json
//! ```
//!
//! Logging is controlled with `RUST_LOG` (default `info`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use marathon_server::Server;
use marathon_server::config::Config;
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;

/// Serves Marathon channels over HTTP.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Path to the TOML config file (see config/example.toml).
    #[arg(
        long,
        short,
        value_name = "PATH",
        required_unless_present = "print_openapi"
    )]
    config: Option<PathBuf>,
    /// Address to listen on, overriding `server.bind` in the config, e.g.
    /// `0.0.0.0:7878` to accept connections from other machines. A
    /// non-loopback address requires `server.token` to be set.
    #[arg(long, value_name = "ADDR:PORT")]
    bind: Option<SocketAddr>,
    /// Print the API's OpenAPI document as JSON and exit, without loading a
    /// config or starting a server. The Android client is generated from
    /// it (scripts/gen-android-api.sh).
    #[arg(long, exclusive = true)]
    print_openapi: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.print_openapi {
        let json = marathon_server::http::ApiDoc::openapi()
            .to_pretty_json()
            .context("failed to serialize the OpenAPI document")?;
        println!("{json}");
        return Ok(());
    }
    // Clap enforces this: `--config` is required unless `--print-openapi`
    // was given, and that returned above.
    let Some(config_path) = args.config else {
        anyhow::bail!("--config is required");
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), config = %config_path.display(), "marathond starting");

    let mut config = Config::load(&config_path)?;
    if let Some(bind) = args.bind {
        tracing::info!(%bind, config_bind = %config.server.bind, "--bind overrides server.bind");
        config.server.bind = bind;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_flag_parses_an_address() {
        let args =
            Args::try_parse_from(["marathond", "-c", "c.toml", "--bind", "0.0.0.0:7878"]).unwrap();
        assert_eq!(args.bind, Some("0.0.0.0:7878".parse().unwrap()));
        let args = Args::try_parse_from(["marathond", "-c", "c.toml"]).unwrap();
        assert_eq!(args.bind, None);
    }

    #[test]
    fn bind_flag_rejects_a_bare_ip() {
        assert!(Args::try_parse_from(["marathond", "-c", "c.toml", "--bind", "0.0.0.0"]).is_err());
    }
}
