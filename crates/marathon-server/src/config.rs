//! Server configuration, loaded from TOML (see `config/example.toml`).
//!
//! Config is authoritative for source and channel definitions; the database is
//! authoritative for playback state.

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

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
    /// The config parsed but is not usable.
    #[error("invalid config: {0}")]
    Invalid(String),
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

    /// Checks the rules that the TOML schema cannot express:
    ///
    /// - a token is set (and non-empty) unless `bind` is a loopback address;
    /// - source IDs, channel IDs, and channel numbers are unique;
    /// - every channel item selector names a configured source and a relative
    ///   path that stays inside it (no `..`).
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));

        let has_token = self.server.token.as_deref().is_some_and(|t| !t.is_empty());
        if !has_token && !self.server.bind.ip().is_loopback() {
            return invalid(format!(
                "server.bind is {}, which is not a loopback address, so server.token \
                 must be set; anyone who can reach the server could otherwise control it",
                self.server.bind
            ));
        }
        if self.server.token.as_deref() == Some("") {
            return invalid("server.token is empty; remove it or set a value".to_owned());
        }

        let mut source_ids = HashSet::new();
        for source in &self.sources {
            if source.id().is_empty() {
                return invalid("a source has an empty id".to_owned());
            }
            if !source_ids.insert(source.id()) {
                return invalid(format!("duplicate source id {:?}", source.id()));
            }
        }

        let mut channel_ids = HashSet::new();
        let mut numbers = HashSet::new();
        for channel in &self.channels {
            if channel.id.is_empty() {
                return invalid(format!("channel {} has an empty id", channel.number));
            }
            if !channel_ids.insert(&channel.id) {
                return invalid(format!("duplicate channel id {:?}", channel.id));
            }
            if !numbers.insert(channel.number) {
                return invalid(format!(
                    "channel number {} is used by more than one channel",
                    channel.number
                ));
            }
            for selector in &channel.items {
                if !source_ids.contains(selector.source.as_str()) {
                    return invalid(format!(
                        "channel {:?} uses unknown source {:?}",
                        channel.id, selector.source
                    ));
                }
                if selector.relative_path().is_none() {
                    return invalid(format!(
                        "channel {:?} has item path {:?}, which must be relative to the \
                         source root and must not contain `..`",
                        channel.id,
                        selector.path.display()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Expands a leading `~` in `path` to the user's home directory (from
/// `$HOME`). Paths without a leading `~`, and all paths when `$HOME` is
/// unset, are returned unchanged.
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_owned();
    };
    match std::env::var_os("HOME") {
        Some(home) => Path::new(&home).join(rest),
        None => path.to_owned(),
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
///
/// A selector whose `path` is a file selects that file. One whose `path` is
/// a directory selects:
///
/// - with `recursive = true`, every video below it, at any depth;
/// - otherwise, the videos directly inside it **and** those directly inside
///   its `Season N` subfolders, so `path = "TV/The Office (US)"` selects a
///   whole show laid out as `Season 1/`, `Season 2/`, and so on. Other
///   subfolders (extras, featurettes) are not included.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSelector {
    /// ID of a configured source.
    pub source: String,
    /// Path relative to the source root; a file or a directory. Empty or
    /// `"."` means the whole source.
    pub path: PathBuf,
    /// Whether to include items in all subdirectories of `path`.
    #[serde(default)]
    pub recursive: bool,
}

impl ItemSelector {
    /// `path` as a `/`-separated string relative to the source root, the
    /// form item paths are stored in; empty for the root. `None` if the path
    /// is absolute, contains `..`, or is not valid UTF-8.
    pub fn relative_path(&self) -> Option<String> {
        let mut parts = Vec::new();
        for component in self.path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str()?),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        Some(parts.join("/"))
    }

    /// Whether this selector selects the item at `item_path` (`/`-separated,
    /// relative to the root of the selector's source). See the type docs for
    /// the rules.
    pub fn matches(&self, item_path: &str) -> bool {
        let Some(base) = self.relative_path() else {
            return false;
        };
        let rest = if base.is_empty() {
            item_path
        } else if item_path == base {
            return true;
        } else {
            match item_path
                .strip_prefix(base.as_str())
                .and_then(|r| r.strip_prefix('/'))
            {
                Some(rest) => rest,
                None => return false,
            }
        };
        if self.recursive {
            return true;
        }
        match rest.split_once('/') {
            None => true,
            Some((dir, file)) => !file.contains('/') && crate::parse::is_season_folder(dir),
        }
    }
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

    fn with_channels(extra: &str) -> String {
        format!(
            "{MINIMAL}\n[[source]]\nid = \"nas\"\nkind = \"filesystem\"\nroot = \"/m\"\n{extra}"
        )
    }

    fn validate(toml: &str) -> Result<(), String> {
        Config::from_toml_str(toml)
            .unwrap()
            .validate()
            .map_err(|e| e.to_string())
    }

    #[test]
    fn loopback_without_token_is_valid() {
        validate(MINIMAL).unwrap();
        validate(&MINIMAL.replace("127.0.0.1", "[::1]")).unwrap();
    }

    #[test]
    fn non_loopback_bind_requires_token() {
        let open = MINIMAL.replace("127.0.0.1", "0.0.0.0");
        let err = validate(&open).unwrap_err();
        assert!(err.contains("server.token must be set"), "{err}");
        validate(&format!("{open}\ntoken = \"secret\"")).unwrap();
        assert!(validate(&format!("{open}\ntoken = \"\"")).is_err());
    }

    #[test]
    fn rejects_duplicates_and_unknown_sources() {
        let ch = |id: &str, n: u32, source: &str| {
            format!(
                "[[channel]]\nid = \"{id}\"\nnumber = {n}\nname = \"x\"\n\
                 items = [{{ source = \"{source}\", path = \"a\" }}]\nrule = {{ kind = \"ordered\" }}\n"
            )
        };
        validate(&with_channels(&(ch("a", 1, "nas") + &ch("b", 2, "nas")))).unwrap();
        let dup_id = validate(&with_channels(&(ch("a", 1, "nas") + &ch("a", 2, "nas"))));
        assert!(dup_id.unwrap_err().contains("duplicate channel id"));
        let dup_num = validate(&with_channels(&(ch("a", 1, "nas") + &ch("b", 1, "nas"))));
        assert!(dup_num.unwrap_err().contains("channel number 1"));
        let unknown = validate(&with_channels(&ch("a", 1, "tape")));
        assert!(unknown.unwrap_err().contains("unknown source"));
        let dup_source =
            with_channels("[[source]]\nid = \"nas\"\nkind = \"filesystem\"\nroot = \"/n\"");
        assert!(
            validate(&dup_source)
                .unwrap_err()
                .contains("duplicate source")
        );
    }

    #[test]
    fn rejects_escaping_selector_paths() {
        for path in ["../etc", "/etc", "TV/../../x"] {
            let toml = with_channels(&format!(
                "[[channel]]\nid = \"a\"\nnumber = 1\nname = \"x\"\n\
                 items = [{{ source = \"nas\", path = \"{path}\" }}]\nrule = {{ kind = \"ordered\" }}"
            ));
            assert!(validate(&toml).is_err(), "{path} should be rejected");
        }
    }

    fn selector(path: &str, recursive: bool) -> ItemSelector {
        ItemSelector {
            source: "nas".to_owned(),
            path: path.into(),
            recursive,
        }
    }

    #[test]
    fn non_recursive_selector_includes_season_folders() {
        let s = selector("TV/Show", false);
        assert!(s.matches("TV/Show/pilot.mkv"));
        assert!(s.matches("TV/Show/Season 1/e1.mkv"));
        assert!(!s.matches("TV/Show/Extras/blooper.mkv"));
        assert!(!s.matches("TV/Show/Season 1/Extras/blooper.mkv"));
        assert!(!s.matches("TV/Show 2/e1.mkv"));
        assert!(!s.matches("TV/e1.mkv"));
    }

    #[test]
    fn recursive_selector_includes_everything_below() {
        let s = selector("./YouTube/Game Design/", true);
        assert_eq!(s.relative_path().as_deref(), Some("YouTube/Game Design"));
        assert!(s.matches("YouTube/Game Design/a/b/c.mkv"));
        assert!(!s.matches("YouTube/Game Designers/c.mkv"));
    }

    #[test]
    fn selector_can_name_a_file_or_the_root() {
        assert!(selector("Films/Heat.mkv", false).matches("Films/Heat.mkv"));
        assert!(selector("", false).matches("top.mkv"));
        assert!(!selector("", false).matches("TV/top.mkv"));
        assert!(selector(".", true).matches("TV/top.mkv"));
    }

    #[test]
    fn expand_home_only_touches_leading_tilde() {
        assert_eq!(expand_home(Path::new("/srv/x")), PathBuf::from("/srv/x"));
        assert_eq!(expand_home(Path::new("a/~/b")), PathBuf::from("a/~/b"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                expand_home(Path::new("~/.local/share/marathon")),
                Path::new(&home).join(".local/share/marathon")
            );
        }
    }

    #[test]
    fn load_reports_missing_file() {
        let err = Config::load("/nonexistent/marathon.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }));
    }
}
