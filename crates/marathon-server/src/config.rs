//! Server configuration, loaded from TOML (see `config/example.toml`).
//!
//! Config is authoritative for source and channel definitions; the database is
//! authoritative for playback state.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};

use marathon_rules::Rule;
use serde::Deserialize;

/// How deep channels may nest (PLAN.md M11): the most parent → child steps
/// from any channel down to a channel with no children.
pub const MAX_NESTING_DEPTH: usize = 8;

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
    /// - every channel that is not hidden has a number;
    /// - every channel item selector names a configured source and a relative
    ///   path that stays inside it (no `..`);
    /// - every child channel entry names a configured channel, at most once
    ///   per parent, and channels nest without cycles and no deeper than
    ///   [`MAX_NESTING_DEPTH`] (PLAN.md M11).
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
            if let SourceConfig::Jellyfin {
                id, url, api_key, ..
            } = source
            {
                validate_jellyfin_url(url).map_err(|reason| {
                    ConfigError::Invalid(format!("source {id:?}: url {url:?} {reason}"))
                })?;
                if api_key.expose().trim().is_empty() {
                    return invalid(format!("source {id:?}: api_key is empty"));
                }
            }
        }

        let mut channel_ids = HashSet::new();
        let mut numbers = HashSet::new();
        for channel in &self.channels {
            if channel.id.is_empty() {
                return invalid(format!(
                    "channel {:?} has an empty id",
                    channel
                        .number
                        .map_or(channel.name.clone(), |n| n.to_string())
                ));
            }
            if !channel_ids.insert(&channel.id) {
                return invalid(format!("duplicate channel id {:?}", channel.id));
            }
            match channel.number {
                Some(number) if !numbers.insert(number) => {
                    return invalid(format!(
                        "channel number {number} is used by more than one channel"
                    ));
                }
                Some(_) => {}
                None if !channel.hidden => {
                    return invalid(format!(
                        "channel {:?} has no number; give it one, or set hidden = true \
                         if it is only meant to be part of other channels",
                        channel.id
                    ));
                }
                None => {}
            }
            for selector in channel.selectors() {
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
        self.validate_nesting()
    }

    /// Checks child channel references: each names a known channel, at
    /// most once per parent, with no cycles, and no deeper than
    /// [`MAX_NESTING_DEPTH`]. Errors name the channels involved.
    fn validate_nesting(&self) -> Result<(), ConfigError> {
        let by_id: HashMap<&str, &ChannelConfig> =
            self.channels.iter().map(|c| (c.id.as_str(), c)).collect();
        for channel in &self.channels {
            let mut seen = HashSet::new();
            for child in channel.children() {
                if !by_id.contains_key(child.channel.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "channel {:?} contains channel {:?}, which is not configured",
                        channel.id, child.channel
                    )));
                }
                if !seen.insert(child.channel.as_str()) {
                    return Err(ConfigError::Invalid(format!(
                        "channel {:?} lists channel {:?} more than once",
                        channel.id, child.channel
                    )));
                }
            }
        }

        // Depth-first search from every channel, keeping the current chain
        // so a cycle or an over-deep chain can be reported in full. `depth`
        // memoises the longest chain below each finished channel, keeping
        // this linear in the size of the graph.
        fn visit<'a>(
            id: &'a str,
            by_id: &HashMap<&'a str, &'a ChannelConfig>,
            chain: &mut Vec<&'a str>,
            depth: &mut HashMap<&'a str, usize>,
        ) -> Result<usize, ConfigError> {
            if let Some(d) = depth.get(id) {
                return Ok(*d);
            }
            if let Some(start) = chain.iter().position(|c| *c == id) {
                let mut cycle: Vec<&str> = chain[start..].to_vec();
                cycle.push(id);
                return Err(ConfigError::Invalid(if cycle.len() == 2 {
                    format!("channel {id:?} contains itself")
                } else {
                    format!(
                        "channels contain each other in a cycle: {}",
                        describe_chain(&cycle)
                    )
                }));
            }
            chain.push(id);
            let mut deepest = 0;
            if let Some(channel) = by_id.get(id) {
                for child in channel.children() {
                    let below = visit(child.channel.as_str(), by_id, chain, depth)?;
                    deepest = deepest.max(below + 1);
                }
            }
            if deepest > MAX_NESTING_DEPTH {
                // Report the chain from here down the deepest path.
                let mut path = vec![id];
                let mut at = id;
                while let Some(next) = by_id.get(at).and_then(|c| {
                    c.children()
                        .map(|child| child.channel.as_str())
                        .max_by_key(|child| depth.get(child).copied().unwrap_or(0))
                }) {
                    path.push(next);
                    at = next;
                }
                return Err(ConfigError::Invalid(format!(
                    "channels nest {deepest} levels deep, more than the limit of \
                     {MAX_NESTING_DEPTH}: {}",
                    describe_chain(&path)
                )));
            }
            chain.pop();
            depth.insert(id, deepest);
            Ok(deepest)
        }

        let mut depth = HashMap::new();
        for channel in &self.channels {
            visit(channel.id.as_str(), &by_id, &mut Vec::new(), &mut depth)?;
        }
        Ok(())
    }

    /// The configured channel with ID `id`.
    pub fn channel(&self, id: &str) -> Option<&ChannelConfig> {
        self.channels.iter().find(|c| c.id == id)
    }
}

/// `"a" → "b" → "c"`.
fn describe_chain(ids: &[&str]) -> String {
    ids.iter()
        .map(|id| format!("{id:?}"))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Checks a Jellyfin server URL: plain `http(s)://host[:port][/path]`, with
/// no credentials in it (those belong in `api_key`, which is kept out of
/// logs; a URL is not).
fn validate_jellyfin_url(url: &str) -> Result<(), &'static str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or("must start with http:// or https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err("has no host");
    }
    if authority.contains('@') {
        return Err("must not contain credentials; put the API key in api_key");
    }
    if url.contains(['?', '#']) {
        return Err("must not have a query or fragment");
    }
    Ok(())
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
    /// A Jellyfin server (PLAN.md M8). Its libraries appear as top-level
    /// folders named after the library (`TV Shows/The Office (US)/...`),
    /// and its streams are proxied through Marathon.
    Jellyfin {
        id: String,
        /// The Jellyfin server's base URL, e.g. `http://jellyfin.lan:8096`.
        url: String,
        /// An API key, from Jellyfin's Dashboard → API Keys. Never logged
        /// or returned by the API.
        api_key: Secret,
        /// Only scan these libraries (by name). Default: all of them.
        #[serde(default)]
        libraries: Option<Vec<String>>,
    },
}

impl SourceConfig {
    /// The source's config-assigned ID, referenced by channel item selectors.
    pub fn id(&self) -> &str {
        match self {
            SourceConfig::Filesystem { id, .. } | SourceConfig::Jellyfin { id, .. } => id,
        }
    }

    /// The source's `kind` as written in config.
    pub fn kind(&self) -> &'static str {
        match self {
            SourceConfig::Filesystem { .. } => "filesystem",
            SourceConfig::Jellyfin { .. } => "jellyfin",
        }
    }
}

/// A credential from config. Its `Debug` output is `<redacted>`, so it
/// cannot leak into logs by accident; use [`Secret::expose`] where the
/// value is really needed.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A `[[channel]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub id: String,
    /// The channel number. Only hidden channels may go without.
    #[serde(default)]
    pub number: Option<u32>,
    pub name: String,
    /// What the channel plays: item selectors and child channels.
    pub items: Vec<Entry>,
    pub rule: Rule,
    /// Whether this channel shares its child channels' playback state or
    /// keeps its own (PLAN.md M11). Irrelevant without child channels.
    #[serde(default)]
    pub state: StateMode,
    /// Hidden channels are left out of the guide and channel up/down and
    /// can go without a number; they can still be tuned by ID, and be
    /// child channels of others.
    #[serde(default)]
    pub hidden: bool,
}

impl ChannelConfig {
    /// The channel's item selectors.
    pub fn selectors(&self) -> impl Iterator<Item = &ItemSelector> {
        self.items.iter().filter_map(|entry| match entry {
            Entry::Items(selector) => Some(selector),
            Entry::Channel(_) => None,
        })
    }

    /// The channel's child channel entries.
    pub fn children(&self) -> impl Iterator<Item = &ChildEntry> {
        self.items.iter().filter_map(|entry| match entry {
            Entry::Channel(child) => Some(child),
            Entry::Items(_) => None,
        })
    }
}

/// How a channel with child channels treats their playback state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateMode {
    /// The parent plays each child from the child's own state, and moves it
    /// along: watching through the parent or tuning the child directly is
    /// the same playback position.
    #[default]
    Shared,
    /// The parent keeps its own position in each child, in its own state,
    /// and never reads or writes the child's.
    Independent,
}

/// One entry of a channel's `items`: either an item selector or a child
/// channel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawEntry")]
pub enum Entry {
    /// `{ source = "nas", path = "TV/Show", recursive = false, weight = 1 }`
    Items(ItemSelector),
    /// `{ channel = "other-channel", weight = 1 }`
    Channel(ChildEntry),
}

/// A child channel entry: the parent's rule picks it like an item, and it
/// plays whatever the child channel is on (PLAN.md M11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildEntry {
    /// ID of the child channel.
    pub channel: String,
    /// Weight for a `weighted` rule with `by = "manual"`; default 1.
    pub weight: Option<u64>,
}

/// Every key an entry may have, so that a mistake gets an error naming
/// what is wrong rather than serde's "did not match any variant".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    source: Option<String>,
    path: Option<PathBuf>,
    recursive: Option<bool>,
    channel: Option<String>,
    weight: Option<u64>,
}

impl TryFrom<RawEntry> for Entry {
    type Error = String;

    fn try_from(raw: RawEntry) -> Result<Self, String> {
        match raw {
            RawEntry {
                channel: Some(channel),
                source: None,
                path: None,
                recursive: None,
                weight,
            } => Ok(Entry::Channel(ChildEntry { channel, weight })),
            RawEntry {
                channel: Some(channel),
                ..
            } => Err(format!(
                "the entry for channel {channel:?} must not also have source, path, or \
                 recursive; list the channel and the items as separate entries"
            )),
            RawEntry {
                source: Some(source),
                path: Some(path),
                recursive,
                weight,
                channel: None,
            } => Ok(Entry::Items(ItemSelector {
                source,
                path,
                recursive: recursive.unwrap_or(false),
                weight,
            })),
            _ => Err(
                "an entry needs either `channel = \"<channel id>\"`, or both \
                      `source` and `path`"
                    .to_owned(),
            ),
        }
    }
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
    /// Weight of each item selected, for a `weighted` rule with
    /// `by = "manual"`; default 1. (To weigh a whole folder as one entry,
    /// make it a hidden channel and use that as a child channel.)
    #[serde(default)]
    pub weight: Option<u64>,
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

    fn jellyfin(url: &str, key: &str) -> String {
        format!(
            "{MINIMAL}\n[[source]]\nid = \"jf\"\nkind = \"jellyfin\"\nurl = \"{url}\"\napi_key = \"{key}\""
        )
    }

    #[test]
    fn jellyfin_sources_parse_and_validate() {
        let config = Config::from_toml_str(&jellyfin("http://jf.lan:8096", "abc123")).unwrap();
        config.validate().unwrap();
        let SourceConfig::Jellyfin {
            url,
            api_key,
            libraries,
            ..
        } = &config.sources[0]
        else {
            panic!("not jellyfin: {:?}", config.sources[0]);
        };
        assert_eq!(url, "http://jf.lan:8096");
        assert_eq!(api_key.expose(), "abc123");
        assert_eq!(libraries, &None);
        assert_eq!(config.sources[0].kind(), "jellyfin");

        for (url, key, why) in [
            ("jf.lan:8096", "k", "http"),
            ("ftp://jf.lan", "k", "http"),
            ("http://", "k", "no host"),
            ("http://me:pw@jf.lan", "k", "credentials"),
            ("http://jf.lan/?api_key=k", "k", "query"),
            ("http://jf.lan", " ", "api_key is empty"),
        ] {
            let err = Config::from_toml_str(&jellyfin(url, key))
                .unwrap()
                .validate()
                .unwrap_err()
                .to_string();
            assert!(err.contains(why), "{url}: {err}");
        }
    }

    #[test]
    fn debug_output_redacts_the_jellyfin_key() {
        let config = Config::from_toml_str(&jellyfin("http://jf.lan", "s3cret-key")).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("s3cret-key"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        // Validation errors name the source, never the key.
        let err = Config::from_toml_str(&jellyfin("http://u:p@jf.lan", "s3cret-key"))
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(!err.to_string().contains("s3cret-key"));
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

    /// A channel line for `with_channels`: `items` is the TOML inside
    /// `items = [...]`.
    fn channel(id: &str, number: Option<u32>, items: &str, extra: &str) -> String {
        let number = number.map_or(String::new(), |n| format!("number = {n}\n"));
        format!(
            "[[channel]]\nid = \"{id}\"\n{number}name = \"{id}\"\nitems = [{items}]\n\
             rule = {{ kind = \"ordered\" }}\n{extra}\n"
        )
    }

    fn parent(id: &str, number: u32, children: &[&str]) -> String {
        let items = children
            .iter()
            .map(|c| format!("{{ channel = \"{c}\" }}"))
            .collect::<Vec<_>>()
            .join(", ");
        channel(id, Some(number), &items, "")
    }

    const LEAF_ITEMS: &str = r#"{ source = "nas", path = "a" }"#;

    #[test]
    fn parses_nested_channels() {
        let toml = with_channels(
            r#"
            [[channel]]
            id = "essayists"
            number = 10
            name = "Video Essayists"
            items = [
              { channel = "contrapoints", weight = 3 },
              { source = "nas", path = "YouTube/Misc Essays", recursive = true },
            ]
            rule = { kind = "weighted", by = "manual" }
            state = "independent"

            [[channel]]
            id = "contrapoints"
            name = "Contrapoints"
            hidden = true
            items = [{ source = "nas", path = "YouTube/Contrapoints" }]
            rule = { kind = "ordered" }
            "#,
        );
        let config = Config::from_toml_str(&toml).unwrap();
        config.validate().unwrap();
        let essayists = &config.channels[0];
        assert_eq!(essayists.state, StateMode::Independent);
        assert_eq!(
            essayists.children().collect::<Vec<_>>(),
            [&ChildEntry {
                channel: "contrapoints".into(),
                weight: Some(3)
            }]
        );
        assert_eq!(essayists.selectors().count(), 1);
        let contrapoints = &config.channels[1];
        assert!(contrapoints.hidden);
        assert_eq!(contrapoints.number, None);
        assert_eq!(contrapoints.state, StateMode::Shared);
    }

    #[test]
    fn rejects_malformed_entries() {
        for (items, why) in [
            (r#"{ channel = "b", source = "nas" }"#, "must not also have"),
            (r#"{ source = "nas" }"#, "needs either"),
            (r#"{ path = "a" }"#, "needs either"),
            (r#"{ channel = "b", wieght = 2 }"#, "unknown field"),
        ] {
            let toml = with_channels(&channel("a", Some(1), items, ""));
            let err = Config::from_toml_str(&toml).unwrap_err();
            let ConfigError::Parse { source, .. } = &err else {
                panic!("{items}: {err:?}");
            };
            assert!(source.to_string().contains(why), "{items}: {source}");
        }
    }

    #[test]
    fn visible_channels_need_numbers() {
        let err = validate(&with_channels(&channel("a", None, LEAF_ITEMS, ""))).unwrap_err();
        assert!(err.contains("\"a\" has no number"), "{err}");
        validate(&with_channels(&channel(
            "a",
            None,
            LEAF_ITEMS,
            "hidden = true",
        )))
        .unwrap();
        // Hidden channels may have numbers too, which must still be unique.
        let both = channel("a", Some(1), LEAF_ITEMS, "hidden = true")
            + &channel("b", Some(1), LEAF_ITEMS, "");
        assert!(
            validate(&with_channels(&both))
                .unwrap_err()
                .contains("number 1")
        );
    }

    #[test]
    fn rejects_a_channel_that_contains_itself() {
        let err = validate(&with_channels(&parent("a", 1, &["a"]))).unwrap_err();
        assert!(err.contains("channel \"a\" contains itself"), "{err}");
    }

    #[test]
    fn rejects_an_indirect_cycle_naming_every_channel() {
        let toml = parent("a", 1, &["b"]) + &parent("b", 2, &["c"]) + &parent("c", 3, &["a"]);
        let err = validate(&with_channels(&toml)).unwrap_err();
        assert!(err.contains("\"a\" → \"b\" → \"c\" → \"a\""), "{err}");
    }

    #[test]
    fn rejects_unknown_and_repeated_children() {
        let err = validate(&with_channels(&parent("a", 1, &["ghost"]))).unwrap_err();
        assert!(
            err.contains("\"a\" contains channel \"ghost\", which is not configured"),
            "{err}"
        );
        let toml = parent("a", 1, &["b", "b"]) + &channel("b", Some(2), LEAF_ITEMS, "");
        let err = validate(&with_channels(&toml)).unwrap_err();
        assert!(err.contains("lists channel \"b\" more than once"), "{err}");
    }

    /// `levels` parent → child steps: c0 → c1 → … → c{levels}.
    fn chain(levels: usize) -> String {
        let mut toml = String::new();
        for i in 0..levels {
            toml += &parent(&format!("c{i}"), i as u32 + 1, &[&format!("c{}", i + 1)]);
        }
        toml + &channel(&format!("c{levels}"), Some(100), LEAF_ITEMS, "")
    }

    #[test]
    fn nesting_depth_is_limited() {
        validate(&with_channels(&chain(MAX_NESTING_DEPTH))).unwrap();
        let err = validate(&with_channels(&chain(MAX_NESTING_DEPTH + 1))).unwrap_err();
        assert!(
            err.contains("9 levels deep, more than the limit of 8"),
            "{err}"
        );
        assert!(
            err.contains("\"c0\" → \"c1\" → \"c2\" → \"c3\" → \"c4\" → \"c5\" → \"c6\" → \"c7\" → \"c8\" → \"c9\""),
            "{err}"
        );
    }

    #[test]
    fn accepts_a_diamond() {
        // Two parents sharing a child, and a grandparent over both.
        let toml = parent("top", 1, &["left", "right"])
            + &parent("left", 2, &["shared"])
            + &parent("right", 3, &["shared"])
            + &channel("shared", None, LEAF_ITEMS, "hidden = true");
        validate(&with_channels(&toml)).unwrap();
    }

    fn selector(path: &str, recursive: bool) -> ItemSelector {
        ItemSelector {
            source: "nas".to_owned(),
            path: path.into(),
            recursive,
            weight: None,
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
