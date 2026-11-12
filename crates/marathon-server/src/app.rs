//! [`App`]: the server's shared state, and getting it ready to serve.

use std::path::PathBuf;
use std::sync::Arc;

use marathon_api::ScanReport;
use tokio::sync::Mutex;

use crate::catalog;
use crate::config::{Config, ConfigError, Entry, SourceConfig, expand_home};
use crate::db::{ChannelRow, Db, OpenError, SourceDef};
use crate::jellyfin::JellyfinError;
use crate::relay::Relay;
use crate::sources::{Source, Sources};

/// File name of the SQLite database inside `data_dir`.
pub const DATABASE_FILE: &str = "marathon.db";

/// Errors that stop the server from starting.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot create data directory {path}")]
    DataDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Database(#[from] OpenError),
    #[error("failed to load config into the database")]
    Sync(#[source] crate::Error),
    #[error("initial library scan failed")]
    Scan(#[source] crate::Error),
    #[error("cannot set up source {id}")]
    Source {
        id: String,
        #[source]
        source: JellyfinError,
    },
    #[error("cannot listen on {addr}")]
    Bind {
        addr: std::net::SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// The server's shared state. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct App {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    pub(crate) db: Db,
    pub(crate) config: Config,
    /// Serialises every read-modify-write of channel state (`now`,
    /// `progress`, `advance`, `previous`), which is what makes `advance`
    /// idempotent under concurrent requests. All of it is quick database
    /// work, so one lock for the whole server is enough.
    pub(crate) playback: Mutex<()>,
    /// Ensures only one rescan runs at a time.
    pub(crate) scanning: Mutex<()>,
    /// Connected clients' control channels and the event stream.
    pub(crate) relay: Relay,
    /// Each source's root directory or Jellyfin client.
    pub(crate) sources: Sources,
}

impl App {
    /// Validates `config`, opens (or creates) the database in its
    /// `data_dir`, mirrors config into it, and scans the library.
    pub async fn open(config: Config) -> Result<Self, StartError> {
        config.validate()?;
        let data_dir = expand_home(&config.server.data_dir);
        std::fs::create_dir_all(&data_dir).map_err(|source| StartError::DataDir {
            path: data_dir.clone(),
            source,
        })?;
        let db = Db::open(&data_dir.join(DATABASE_FILE)).await?;
        Self::with_db(config, db).await
    }

    /// Like [`App::open`], but with an already-open database (for example
    /// [`Db::open_in_memory`] in tests). Ignores `config.server.data_dir`.
    pub async fn with_db(config: Config, db: Db) -> Result<Self, StartError> {
        config.validate()?;
        let sources = Sources::from_config(&config)
            .map_err(|(id, source)| StartError::Source { id, source })?;
        sync_definitions(&db, &config)
            .await
            .map_err(StartError::Sync)?;
        let app = App {
            inner: Arc::new(Inner {
                db,
                config,
                playback: Mutex::new(()),
                scanning: Mutex::new(()),
                relay: Relay::new(),
                sources,
            }),
        };
        let report = app.rescan().await.map_err(StartError::Scan)?;
        for error in &report.errors {
            tracing::warn!(%error, "source not scanned at startup");
        }
        Ok(app)
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// The client relay: control channels and the event stream.
    pub fn relay(&self) -> &Relay {
        &self.inner.relay
    }

    /// Rescans every source and rebuilds channel membership. Concurrent
    /// calls run one after another.
    pub async fn rescan(&self) -> Result<ScanReport, crate::Error> {
        let _guard = self.inner.scanning.lock().await;
        catalog::rescan(&self.inner.db, &self.inner.config, &self.inner.sources).await
    }

    /// A configured source, ready to scan or stream from.
    pub(crate) fn source(&self, source_id: &str) -> Option<&Source> {
        self.inner.sources.get(source_id)
    }

    /// The root directory of a configured filesystem source, with `~`
    /// expanded. `None` for other kinds of source.
    pub(crate) fn source_root(&self, source_id: &str) -> Option<PathBuf> {
        match self.source(source_id)? {
            Source::Filesystem { root } => Some(root.clone()),
            Source::Jellyfin(_) => None,
        }
    }
}

/// Mirrors config's sources and channels into the database. Channel state is
/// untouched, so editing a channel's rule keeps its position.
async fn sync_definitions(db: &Db, config: &Config) -> Result<(), crate::Error> {
    let sources = config
        .sources
        .iter()
        .map(|source| {
            // Never the Jellyfin API key: the database is not a place
            // for credentials.
            let config_json = match source {
                SourceConfig::Filesystem { root, .. } => {
                    serde_json::json!({ "root": root.to_string_lossy() })
                }
                SourceConfig::Jellyfin { url, libraries, .. } => {
                    serde_json::json!({ "url": url, "libraries": libraries })
                }
            };
            SourceDef {
                id: source.id().to_owned(),
                kind: source.kind().to_owned(),
                config_json: config_json.to_string(),
            }
        })
        .collect::<Vec<_>>();
    let channels = config
        .channels
        .iter()
        .map(|channel| {
            // A record of what the channel was configured with; playback
            // reads definitions from config, not from here.
            let selectors = channel
                .items
                .iter()
                .map(|entry| match entry {
                    Entry::Items(s) => serde_json::json!({
                        "source": s.source,
                        "path": s.relative_path(),
                        "recursive": s.recursive,
                        "files": s.relative_files(),
                        "exclude": s.relative_exclude(),
                        "weight": s.weight,
                    }),
                    Entry::Channel(c) => serde_json::json!({
                        "channel": c.channel,
                        "weight": c.weight,
                    }),
                })
                .collect::<Vec<_>>();
            Ok(ChannelRow {
                id: channel.id.clone(),
                // Unnumbered (hidden) channels are stored, and listed, as 0.
                number: i64::from(channel.number.unwrap_or(0)),
                name: channel.name.clone(),
                rule_json: serde_json::to_string(&channel.rule)?,
                selectors_json: serde_json::to_string(&selectors)?,
            })
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;
    db.sync_definitions(&sources, &channels).await?;
    Ok(())
}
