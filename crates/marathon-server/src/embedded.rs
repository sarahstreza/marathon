//! Running the server inside another program (PLAN.md §2 decision 2, §9
//! "Embedded mode").
//!
//! [`EmbeddedServer`] starts a [`Server`] on its own thread and Tokio
//! runtime, so the host does not need to be async: the desktop client's
//! main thread belongs to the window's event loop. The host then talks to
//! it over HTTP exactly as it would to a standalone `marathond`, at
//! [`EmbeddedServer::base_url`]. There is one client code path.
//!
//! The embedded server always listens on `127.0.0.1` at a port the OS
//! picks, whatever `server.bind` says in the config. It exists for one
//! local client, and must not show up on the network just because the
//! config was written for a standalone server. libmpv still needs a real
//! TCP port to fetch streams from, which is why this is loopback TCP and
//! not a Unix socket. A configured token is still enforced. Other users
//! on the same machine can reach loopback ports, so the client needs the
//! token too.
//!
//! ```no_run
//! # fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = marathon_server::config::Config::load("marathon.toml")?;
//! let server = marathon_server::embedded::EmbeddedServer::start(config)?;
//! println!("API at {}/api/v1", server.base_url());
//! // ... use it ...
//! server.shutdown()?;
//! # Ok(())
//! # }
//! ```

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::config::Config;
use crate::{Server, StartError};

/// The address an embedded server binds: loopback only, port chosen by the
/// OS.
pub const EMBEDDED_BIND: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// How long, after the server has stopped, to wait for any remaining tasks
/// (connections abandoned after the shutdown grace period) to be cancelled
/// before the runtime is torn down regardless.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Errors from starting or stopping an [`EmbeddedServer`].
#[derive(Debug, thiserror::Error)]
pub enum EmbeddedError {
    /// The server's thread or Tokio runtime could not be created.
    #[error("cannot start the embedded server's runtime")]
    Runtime(#[source] std::io::Error),
    /// The server itself failed to start (bad config, database, scan).
    #[error(transparent)]
    Start(#[from] StartError),
    /// The server stopped with an I/O error while serving.
    #[error("the embedded server failed while serving")]
    Serve(#[source] std::io::Error),
    /// The server's thread panicked.
    #[error("the embedded server's thread panicked")]
    Panicked,
}

/// A Marathon server running in this process on a background thread.
///
/// Stop it with [`EmbeddedServer::shutdown`] to see whether it stopped
/// cleanly; dropping it also shuts it down (and blocks until it has),
/// logging any error.
#[derive(Debug)]
pub struct EmbeddedServer {
    local_addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<Result<(), EmbeddedError>>>,
}

impl EmbeddedServer {
    /// Starts a server from `config`, bound to [`EMBEDDED_BIND`] in place
    /// of `config.server.bind`, and blocks until it is serving: the
    /// database is open and the initial library scan has finished.
    pub fn start(mut config: Config) -> Result<Self, EmbeddedError> {
        if config.server.bind != EMBEDDED_BIND {
            tracing::info!(
                configured = %config.server.bind,
                "embedded server ignores server.bind and listens on 127.0.0.1 only"
            );
        }
        config.server.bind = EMBEDDED_BIND;

        let (started_tx, started_rx) = mpsc::channel::<Result<SocketAddr, StartError>>();
        let (stop, stopped) = oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("embedded-server".into())
            .spawn(move || run(config, started_tx, stopped))
            .map_err(EmbeddedError::Runtime)?;

        match started_rx.recv() {
            Ok(Ok(local_addr)) => Ok(Self {
                local_addr,
                stop: Some(stop),
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                // The thread has nothing left to do; reap it.
                let _ = thread.join();
                Err(err.into())
            }
            // The thread ended without reporting: it failed to build the
            // runtime (and says why) or panicked.
            Err(mpsc::RecvError) => match thread.join() {
                Ok(Err(err)) => Err(err),
                Ok(Ok(())) | Err(_) => Err(EmbeddedError::Panicked),
            },
        }
    }

    /// The address the server listens on (always `127.0.0.1`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The base URL for API clients, e.g. `http://127.0.0.1:43127`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.local_addr)
    }

    /// Stops the server and waits until it has: open connections get the
    /// server's shutdown grace period, the database is closed, the port is
    /// released, and the thread has exited.
    pub fn shutdown(mut self) -> Result<(), EmbeddedError> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> Result<(), EmbeddedError> {
        if let Some(stop) = self.stop.take() {
            // Fails only if the server already stopped by itself.
            let _ = stop.send(());
        }
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| EmbeddedError::Panicked)?,
            None => Ok(()),
        }
    }
}

impl Drop for EmbeddedServer {
    fn drop(&mut self) {
        if let Err(err) = self.stop_and_join() {
            tracing::error!(%err, "embedded server did not stop cleanly");
        }
    }
}

/// The server thread: build a runtime, start, report, serve until told to
/// stop, then tear the runtime down.
fn run(
    config: Config,
    started: mpsc::Sender<Result<SocketAddr, StartError>>,
    stopped: oneshot::Receiver<()>,
) -> Result<(), EmbeddedError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("embedded-server-worker")
        .build()
        .map_err(EmbeddedError::Runtime)?;
    let result = runtime.block_on(async move {
        let server = match Server::bind(config).await {
            Ok(server) => server,
            Err(err) => {
                let _ = started.send(Err(err));
                return Ok(());
            }
        };
        if started.send(Ok(server.local_addr())).is_err() {
            // `start` is gone (it cannot be, as it waits for this); there
            // is nobody to serve.
            return Ok(());
        }
        server
            .serve(async {
                // An error means the EmbeddedServer was dropped without
                // sending, which also means stop.
                let _ = stopped.await;
            })
            .await
            .map_err(EmbeddedError::Serve)
    });
    // Cancels connection tasks abandoned after the grace period.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}
