//! Server configuration, loaded from TOML (see `config/example.toml`).
//!
//! Config is authoritative for source and channel definitions; the database is
//! authoritative for playback state.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use marathon_rules::Rule;
use serde::Deserialize;

/// Errors from loading a config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("failed to read config file {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The config file is not valid TOML or does not match the schema.
    #[error("failed to parse config {origin}")]
    Parse {
        /// The file path, or a description such as `<string>`.
        origin: String,
        #[source]
        source: toml::de::Error,
    },
}

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(rename = "source", default)]
    pub sources: Vec<SourceConfig>,
    #[serde(rename = "channel", default)]
    pub channels: Vec<ChannelConfig>,
}

impl Config {
    /// Parses a config from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(|source| ConfigError::Parse {
            origin: "<string>".to_owned(),
            source,
        })
    }

    /// Reads and parses a config file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            origin: path.display().to_string(),
            source,
        })
    }
}

/// The `[server]` table.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the HTTP server binds to.
    pub bind: SocketAddr,
    /// Directory holding the SQLite database. A leading `~` is expanded when
    /// the server starts, not here.
    pub data_dir: PathBuf,
    /// Bearer token required on every request. Must be set when `bind` is not
    /// a loopback address.
    #[serde(default)]
    pub token: Option<String>,
}

// Hand-written so the token never ends up in logs.
impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind", &self.bind)
            .field("data_dir", &self.data_dir)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A `[[source]]` entry: somewhere items come from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SourceConfig {
    /// A local directory (including mounted SMB/NFS shares).
    Filesystem { id: String, root: PathBuf },
}

impl SourceConfig {
    /// The source's config-assigned ID, referenced by channel item selectors.
    pub fn id(&self) -> &str {
        match self {
            SourceConfig::Filesystem { id, .. } => id,
        }
    }
}

/// A `[[channel]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub id: String,
    pub number: u32,
    pub name: String,
    pub items: Vec<ItemSelector>,
    pub rule: Rule,
}

/// Selects items for a channel: a path within a source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSelector {
    /// ID of a configured source.
    pub source: String,
    /// Path relative to the source root; a file or a directory.
    pub path: PathBuf,
    /// Whether to include items in subdirectories of `path`.
    #[serde(default)]
    pub recursive: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        bind = "127.0.0.1:7878"
        data_dir = "/tmp/marathon"
    "#;

    #[test]
    fn minimal_config_has_no_sources_or_channels() {
        let config = Config::from_toml_str(MINIMAL).unwrap();
        assert!(config.sources.is_empty());
        assert!(config.channels.is_empty());
        assert_eq!(config.server.token, None);
    }

    #[test]
    fn debug_output_redacts_token() {
        let config = Config::from_toml_str(&format!("{MINIMAL}\ntoken = \"hunter2\"")).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = Config::from_toml_str(&format!("{MINIMAL}\nbnid = \"x\"")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_unknown_source_kind() {
        let toml = format!("{MINIMAL}\n[[source]]\nid = \"x\"\nkind = \"ftp\"\nroot = \"/\"");
        assert!(Config::from_toml_str(&toml).is_err());
    }

    #[test]
    fn load_reports_missing_file() {
        let err = Config::load("/nonexistent/marathon.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }));
    }
}
