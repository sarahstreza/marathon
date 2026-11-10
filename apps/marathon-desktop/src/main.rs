//! `marathon-desktop`: the Marathon desktop client.
//!
//! ```text
//! marathon-desktop [--server http://127.0.0.1:7878] [--token T] [--channel N] [--windowed]
//! ```
//!
//! Two threads:
//!
//! - the main thread runs the winit event loop, owns the GL context, and
//!   renders mpv's video ([`window`]);
//! - the tuner thread runs the [`marathon_client::tuner::Tuner`], making
//!   the (blocking) API calls and commanding mpv ([`controller`]).
//!
//! libmpv is thread-safe, so both hold handles to the same player. Keys
//! map to actions (see `marathon_client::keyboard` for the bindings).
//! There is no overlay yet (PLAN.md M5): channel changes and info are
//! logged to the console. Logging is controlled with `RUST_LOG` (default
//! `info`; `RUST_LOG=info,mpv=debug` shows mpv's own log).

mod controller;
mod input;
mod player;
mod window;

use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Context;
use clap::Parser;
use marathon_client::api::{Api, HttpApi};
use marathon_client::client_id;
use marathon_client::tuner::{SystemClock, Tuner, TunerConfig};
use marathon_mpv::Mpv;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

use crate::controller::Msg;
use crate::player::MpvPlayer;
use crate::window::{App, UserEvent};

/// Watches Marathon channels.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Base URL of the Marathon server.
    #[arg(long, env = "MARATHON_SERVER", default_value = "http://127.0.0.1:7878")]
    server: String,
    /// Bearer token, if the server requires one.
    #[arg(long, env = "MARATHON_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Channel number to start on. Defaults to the channel this client
    /// watched last.
    #[arg(long, value_name = "N")]
    channel: Option<u32>,
    /// Start in a window rather than fullscreen.
    #[arg(long)]
    windowed: bool,
    /// Where this client's ID is stored. Defaults to `marathon/client-id`
    /// in the user's config directory.
    #[arg(long, value_name = "PATH")]
    client_id_file: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        server = %args.server,
        "marathon-desktop starting"
    );

    let id_path = match args.client_id_file {
        Some(path) => path,
        None => dirs::config_dir()
            .context("cannot find the user's config directory; pass --client-id-file")?
            .join("marathon")
            .join("client-id"),
    };
    let client_id = client_id::load_or_create(&id_path)
        .with_context(|| format!("cannot read or create {}", id_path.display()))?;

    let api = HttpApi::new(&args.server, args.token.as_deref(), &client_id)?;
    // Fail early, before opening a window, if the server is unusable.
    let channels = api
        .channels()
        .with_context(|| format!("cannot list channels from {}", args.server))?;
    tracing::info!(count = channels.len(), "connected");

    let (mpv, events) = Mpv::new(player::mpv_options()).context("cannot start mpv")?;
    player::set_http_headers(&mpv, api.media_headers())?;
    mpv.request_log_messages("info")?;
    let (major, minor) = marathon_mpv::client_api_version();
    tracing::debug!(major, minor, "libmpv client API");

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("cannot start the event loop")?;
    let proxy = event_loop.create_proxy();

    let tuner = Tuner::new(
        api,
        MpvPlayer::new(mpv.clone()),
        SystemClock,
        TunerConfig {
            client_id: Some(client_id),
            ..TunerConfig::default()
        },
    );
    let (tx, rx) = mpsc::channel::<Msg>();
    let wake = tx.clone();
    let stopped = proxy.clone();
    let start_channel = args.channel;
    let tuner_thread = std::thread::Builder::new()
        .name("tuner".into())
        .spawn(move || {
            let exit = controller::run(tuner, events, rx, wake, start_channel);
            // Tell the window, in case it did not ask us to stop.
            let _ = stopped.send_event(UserEvent::TunerStopped);
            exit
        })
        .context("cannot start the tuner thread")?;

    let mut app = App::new(mpv, proxy, tx, tuner_thread, !args.windowed);
    event_loop.run_app(&mut app)?;
    match app.error.take() {
        Some(err) => Err(err),
        None => Ok(()),
    }
}
