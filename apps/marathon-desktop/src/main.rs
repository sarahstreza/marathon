//! `marathon-desktop`: the Marathon desktop client.
//!
//! ```text
//! marathon-desktop [--server http://127.0.0.1:7878] [--token T] [--channel N] [--windowed]
//! ```
//!
//! How the pieces fit (PLAN.md §9):
//!
//! ```text
//! platform (window, keys) ──Key──► keyboard InputSource ──Action──► action bus
//!                                                                     │ route()
//!                        ◄── fullscreen / quit ───────────────────────┤
//!                                                                     ▼
//! platform ◄── frames ── renderer ◄── mpv ◄── MpvPlayer ◄── tuner thread
//! ```
//!
//! - [`platform`]: the main thread's event loop and window; the only code
//!   that knows about winit/glutin.
//! - [`renderer`]: draws mpv's video into any [`surface::Surface`].
//! - [`controller`]: routes actions and runs the tuner thread, which makes
//!   the (blocking) API calls and commands mpv through [`player`].
//!
//! libmpv is thread-safe, so the renderer and the tuner thread share one
//! player. There is no overlay yet (M5): channel changes, info, and the
//! guide are logged to the console. Logging is controlled with `RUST_LOG`
//! (default `info`; `RUST_LOG=info,mpv=debug` shows mpv's own log).

mod controller;
mod platform;
mod player;
mod renderer;
mod surface;

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use anyhow::Context;
use clap::Parser;
use marathon_api::Action;
use marathon_client::api::{Api, HttpApi};
use marathon_client::bus::{ActionBus, BusClosed};
use marathon_client::client_id;
use marathon_client::input::InputSources;
use marathon_client::keyboard::KeyboardSource;
use marathon_client::tuner::{SystemClock, Tuner, TunerConfig};
use marathon_mpv::Mpv;
use tracing_subscriber::EnvFilter;

use crate::controller::{Exit, Msg, Route, WindowCommand};
use crate::platform::{Hooks, Options, Platform};
use crate::player::MpvPlayer;

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

/// What the window calls back into: starting playback once it can be
/// shown, and shutting down in order.
struct App {
    tuner: Sender<Msg>,
    tuner_thread: Option<JoinHandle<Exit>>,
    inputs: InputSources,
}

impl Hooks for App {
    fn ready(&mut self) {
        let _ = self.tuner.send(Msg::Start);
    }

    fn shutdown(&mut self) {
        // No more input, then let the tuner report progress and stop.
        self.inputs.stop_all();
        let _ = self.tuner.send(Msg::Action(Action::Quit));
        if let Some(thread) = self.tuner_thread.take() {
            match thread.join() {
                Ok(exit) => tracing::debug!(?exit, "tuner thread stopped"),
                Err(_) => tracing::error!("tuner thread panicked"),
            }
        }
    }
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

    let platform = Platform::new().context("cannot start the event loop")?;
    let window = platform.handle();

    // The action bus: window actions to the platform, the rest to the
    // tuner thread.
    let (tx, rx) = mpsc::channel::<Msg>();
    let bus = {
        let (tx, window) = (tx.clone(), window.clone());
        ActionBus::new(move |sourced| {
            match controller::route(sourced.action) {
                Route::Window(WindowCommand::ToggleFullscreen) => window.toggle_fullscreen(),
                Route::Window(WindowCommand::Quit) => window.quit(),
                Route::Tuner(action) => tx.send(Msg::Action(action)).map_err(|_| BusClosed)?,
            }
            Ok(())
        })
    };
    let mut inputs = InputSources::new();
    let (keyboard, keys) = KeyboardSource::new();
    inputs.start(Box::new(keyboard), &bus)?;

    let tuner = Tuner::new(
        api,
        MpvPlayer::new(mpv.clone()),
        SystemClock,
        TunerConfig {
            client_id: Some(client_id),
            ..TunerConfig::default()
        },
    );
    let wake = tx.clone();
    let start_channel = args.channel;
    let tuner_thread = std::thread::Builder::new()
        .name("tuner".into())
        .spawn(move || {
            let exit = controller::run(tuner, events, rx, wake, start_channel);
            // Tell the window, in case it did not ask us to stop.
            window.quit();
            exit
        })
        .context("cannot start the tuner thread")?;

    let app = App {
        tuner: tx,
        tuner_thread: Some(tuner_thread),
        inputs,
    };
    platform.run(
        Options {
            fullscreen: !args.windowed,
        },
        mpv,
        keys,
        app,
    )
}
