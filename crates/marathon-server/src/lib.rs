//! Marathon server library: config, catalog, sources, database, HTTP routes,
//! and media streaming. Used by `marathond` and by the desktop client's
//! embedded mode.
//!
//! The usual entry point is [`Server`]:
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
mod error;
pub mod http;
pub mod parse;
mod playback;
pub mod scan;

use std::future::Future;
use std::net::SocketAddr;

pub use app::{App, DATABASE_FILE, StartError};
pub use error::{ApiError, Error};

use config::Config;

/// A server that is ready to serve: config validated, database open and
/// migrated, library scanned, and listening socket bound.
pub struct Server {
    app: App,
    listener: tokio::net::TcpListener,
    local_addr: SocketAddr,
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
        })
    }

    /// The address the server is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    /// Serves requests until `shutdown` completes, then finishes in-flight
    /// requests and closes the database.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        tracing::info!(addr = %self.local_addr, "serving");
        let result = axum::serve(self.listener, http::router(self.app.clone()))
            .with_graceful_shutdown(shutdown)
            .await;
        self.app.db().close().await;
        tracing::info!("server stopped");
        result
    }
}
