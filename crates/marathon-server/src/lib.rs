//! Marathon server library: config, catalog, sources, database, HTTP routes,
//! and media streaming. Used by `marathond` and by the desktop client's
//! embedded mode.
//!
//! The usual entry point is [`Server`]. To run a server inside another
//! (possibly non-async) program, use [`embedded::EmbeddedServer`].
//!
//! A standalone server looks like this:
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = marathon_server::config::Config::load("marathon.toml")?;
//! let server = marathon_server::Server::bind(config).await?;
//! println!("listening on {}", server.local_addr());
//! server.serve(async { let _ = tokio::signal::ctrl_c().await; }).await?;
//! # Ok(())
//! # }
//! ```

mod app;
pub mod catalog;
pub mod config;
pub mod db;
pub mod embedded;
mod error;
pub mod http;
pub mod parse;
mod playback;
pub mod scan;

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::time::Duration;

pub use app::{App, DATABASE_FILE, StartError};
pub use error::{ApiError, Error};

use config::Config;

/// How long [`Server::serve`] waits, by default, for open connections to
/// finish after shutdown is requested. See [`Server::shutdown_grace`].
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// A server that is ready to serve: config validated, database open and
/// migrated, library scanned, and listening socket bound.
pub struct Server {
    app: App,
    listener: tokio::net::TcpListener,
    local_addr: SocketAddr,
    shutdown_grace: Duration,
}

impl Server {
    /// Validates `config`, binds `config.server.bind`, opens the database,
    /// and scans the library.
    ///
    /// Fails if the bind address is not loopback and no token is set. Bind
    /// to port 0 to have the OS pick a free port; [`Server::local_addr`]
    /// reports which.
    pub async fn bind(config: Config) -> Result<Self, StartError> {
        config.validate()?;
        let addr = config.server.bind;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| StartError::Bind { addr, source })?;
        let local_addr = listener
            .local_addr()
            .map_err(|source| StartError::Bind { addr, source })?;
        let app = App::open(config).await?;
        Ok(Self {
            app,
            listener,
            local_addr,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        })
    }

    /// Sets how long [`Server::serve`] lets open connections finish once
    /// shutdown is requested (default [`DEFAULT_SHUTDOWN_GRACE`]). A
    /// player streaming a video holds its connection open for as long as
    /// it plays, so without a limit shutdown could wait forever.
    pub fn shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// The address the server is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// Serves requests until `shutdown` completes. Then it stops accepting
    /// connections, gives in-flight requests up to the shutdown grace
    /// period (see [`Server::shutdown_grace`]) to finish, and closes the
    /// database.
    ///
    /// Connections still open after the grace period are abandoned rather
    /// than awaited: their tasks end when the Tokio runtime shuts down.
    /// None of them hold a database connection for longer than a query, so
    /// the database still closes cleanly.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        tracing::info!(addr = %self.local_addr, "serving");
        let (requested, on_request) = tokio::sync::oneshot::channel::<()>();
        let serve = axum::serve(self.listener, http::router(self.app.clone()))
            .with_graceful_shutdown(async move {
                shutdown.await;
                tracing::info!("shutting down");
                let _ = requested.send(());
            })
            .into_future();
        let grace = self.shutdown_grace;
        let deadline = async move {
            // An error means `serve` finished and dropped the sender, so
            // there is nothing to time out.
            if on_request.await.is_err() {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(grace).await;
        };
        let result = tokio::select! {
            result = serve => result,
            () = deadline => {
                tracing::warn!(
                    ?grace,
                    "connections still open after the shutdown grace period; closing them"
                );
                Ok(())
            }
        };
        self.app.db().close().await;
        tracing::info!("server stopped");
        result
    }
}
