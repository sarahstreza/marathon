//! `marathon-desktop`: the Marathon desktop client.
//!
//! ```text
//! marathon-desktop [--server http://127.0.0.1:7878] [--token T] [--channel N] [--windowed]
//! marathon-desktop --embedded --config path/to/config.toml [--channel N] [--windowed]
//! ```
//!
//! With `--embedded` the app runs its own server in-process
//! ([`EmbeddedServer`], PLAN.md §9 "Embedded mode") on an OS-assigned
//! loopback port, and talks to it over HTTP exactly as it would to a
//! remote `marathond`. The server stops when the app quits.
//!
//! How the pieces fit (PLAN.md §9):
//!
//! ```text
//! platform (window, keys, mouse, gamepad) ──► input sources ──Action──► action bus
//! server ──WebSocket──► remote ─────────────►                             │ route()
//!                        ◄── fullscreen / quit ───────────────────────────┤
//!                                                                         ▼
//! platform ◄── frames ── renderer ◄── mpv ◄── MpvPlayer ◄───────── tuner thread
//!                           ▲                                             │
//!                           └──── overlay ◄── cues, progress, errors ─────┤
//! server ◄──WebSocket── remote ◄── events ────────────────────────────────┘
//! ```
//!
//! - [`platform`]: the main thread's event loop and window; the only code
//!   that knows about winit/glutin/gilrs.
//! - [`renderer`]: draws mpv's video and the [`overlay`] into any
//!   [`surface::Surface`].
//! - [`overlay`]: the channel banner, guide, and error toasts (egui).
//! - [`controller`]: routes actions and runs the tuner thread, which makes
//!   the (blocking) API calls, commands mpv through [`player`], and feeds
//!   the overlay.
//!
//! libmpv is thread-safe, so the renderer and the tuner thread share one
//! player. Logging is controlled with `RUST_LOG` (default `info`;
//! `RUST_LOG=info,mpv=debug` shows mpv's own log).

mod controller;
mod overlay;
mod platform;
mod player;
mod renderer;
mod surface;

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use anyhow::Context;
use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser};
use marathon_api::Action;
use marathon_client::api::{Api, HttpApi};
use marathon_client::bus::{ActionBus, BusClosed};
use marathon_client::client_id;
use marathon_client::input::InputSources;
use marathon_client::keyboard::KeyboardSource;
use marathon_client::mouse::MouseSource;
use marathon_client::remote::{RemoteConfig, RemoteSource};
use marathon_client::tuner::{SystemClock, Tuner, TunerConfig};
use marathon_mpv::Mpv;
use marathon_server::config::Config;
use marathon_server::embedded::EmbeddedServer;
use tracing_subscriber::EnvFilter;

use crate::controller::{Exit, Msg, Route, WindowCommand};
use crate::overlay::{Overlay, OverlayMsg};
use crate::platform::{Hooks, InputFeeds, Options, Platform};
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
    /// Run a Marathon server inside this app, from --config, instead of
    /// connecting to one. It listens on 127.0.0.1 only, on a port the OS
    /// picks, and stops when the app quits.
    #[arg(long, requires = "config")]
    embedded: bool,
    /// Server config file for --embedded (the same format marathond
    /// reads; server.bind is ignored).
    #[arg(long, short, value_name = "PATH", requires = "embedded")]
    config: Option<PathBuf>,
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

impl Args {
    /// Parses `argv`. Beyond what clap's attributes express, `--server` and
    /// `--token` on the command line conflict with `--embedded`. Set from
    /// `MARATHON_SERVER`/`MARATHON_TOKEN` they are just ignored, so a shell
    /// that exports them can still run an embedded server. (clap's own
    /// `conflicts_with` counts environment values as conflicting.)
    fn parse_from<I, T>(argv: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let mut command = Args::command();
        let matches = command.try_get_matches_from_mut(argv)?;
        let args = Args::from_arg_matches(&matches)?;
        if args.embedded {
            for flag in ["server", "token"] {
                if matches.value_source(flag) == Some(ValueSource::CommandLine) {
                    return Err(command.error(
                        ErrorKind::ArgumentConflict,
                        format!("--{flag} cannot be used with --embedded"),
                    ));
                }
            }
        }
        Ok(args)
    }
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
    let args = Args::parse_from(std::env::args_os()).unwrap_or_else(|err| err.exit());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        embedded = args.embedded,
        "marathon-desktop starting"
    );

    // Declared before everything that talks to it, so on an early return
    // it is dropped (and shut down) last.
    let (embedded, server_url, token) = match &args.config {
        Some(path) if args.embedded => {
            let config = Config::load(path)?;
            let token = config.server.token.clone();
            let server =
                EmbeddedServer::start(config).context("cannot start the embedded server")?;
            let url = server.base_url();
            tracing::info!(%url, "embedded server running");
            (Some(server), url, token)
        }
        _ => (None, args.server.clone(), args.token.clone()),
    };
    tracing::info!(server = %server_url, "using server");

    let id_path = match args.client_id_file {
        Some(path) => path,
        None => dirs::config_dir()
            .context("cannot find the user's config directory; pass --client-id-file")?
            .join("marathon")
            .join("client-id"),
    };
    let client_id = client_id::load_or_create(&id_path)
        .with_context(|| format!("cannot read or create {}", id_path.display()))?;

    let api = HttpApi::new(&server_url, token.as_deref(), &client_id)?;
    // Fail early, before opening a window, if the server is unusable.
    let channels = api
        .channels()
        .with_context(|| format!("cannot list channels from {server_url}"))?;
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
    let (mouse_source, mouse) = MouseSource::new();
    inputs.start(Box::new(mouse_source), &bus)?;
    #[cfg(feature = "input-gamepad")]
    inputs.start(Box::new(platform::gamepad::GamepadSource::new()), &bus)?;
    // Remote control through the server: relayed actions arrive on the bus
    // like any other input, and the tuner's events go back up for
    // observers (PLAN.md §9 "Remote adapters").
    let (remote, publisher) = RemoteSource::new(RemoteConfig::for_api(&api));
    if let Err(err) = inputs.start(Box::new(remote), &bus) {
        tracing::warn!(%err, "remote control unavailable; carrying on without it");
    }

    let (overlay, overlay_feed) = {
        let window = window.clone();
        Overlay::new(move || window.overlay_changed())
    };
    let (cues, errors) = (overlay_feed.clone(), overlay_feed.clone());
    let tuner = Tuner::new(
        api,
        MpvPlayer::new(mpv.clone()),
        SystemClock,
        TunerConfig {
            client_id: Some(client_id),
            ..TunerConfig::default()
        },
    )
    .with_ui_sink(move |cue| cues.send(OverlayMsg::Ui(cue)))
    .with_event_sink(move |event| {
        tracing::debug!(?event, "event");
        if let marathon_api::Event::Error { message } = &event {
            errors.send(OverlayMsg::Toast(message.clone()));
        }
        publisher.publish(event);
    });
    let wake = tx.clone();
    let start_channel = args.channel;
    let tuner_thread = std::thread::Builder::new()
        .name("tuner".into())
        .spawn(move || {
            let exit = controller::run(tuner, events, rx, wake, overlay_feed, start_channel);
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
    let result = platform.run(
        Options {
            fullscreen: !args.windowed,
        },
        mpv,
        overlay,
        InputFeeds { keys, mouse },
        app,
    );

    // By now the tuner has reported its final progress and mpv (with its
    // stream connection) is gone, so the server has nothing left to wait
    // for.
    if let Some(server) = embedded {
        match server.shutdown() {
            Ok(()) => tracing::info!("embedded server stopped"),
            Err(err) => {
                tracing::error!(err = %anyhow::Error::from(err), "embedded server did not stop cleanly")
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::Args;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::parse_from(std::iter::once("marathon-desktop").chain(args.iter().copied()))
    }

    #[test]
    fn embedded_needs_a_config_and_vice_versa() {
        let args = parse(&["--embedded", "--config", "m.toml"]).unwrap();
        assert!(args.embedded);
        assert_eq!(args.config.unwrap().to_str(), Some("m.toml"));
        assert!(parse(&["--embedded"]).is_err());
        assert!(parse(&["--config", "m.toml"]).is_err());
    }

    #[test]
    fn embedded_conflicts_with_a_remote_server() {
        for extra in [["--server", "http://h:7878"], ["--token", "t"]] {
            let mut argv = vec!["--embedded", "-c", "m.toml"];
            argv.extend(extra);
            let err = parse(&argv).err().unwrap();
            assert_eq!(err.kind(), ErrorKind::ArgumentConflict, "{err}");
        }
    }

    #[test]
    fn remote_is_the_default() {
        let args = parse(&[]).unwrap();
        assert!(!args.embedded);
        assert!(args.config.is_none());
    }
}
