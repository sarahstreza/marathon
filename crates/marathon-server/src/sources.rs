//! Sources at run time: what each configured `[[source]]` needs to be
//! scanned and streamed from (a root directory, or a Jellyfin client).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{Config, SourceConfig, expand_home};
use crate::jellyfin::{JellyfinClient, JellyfinError};

/// One source, ready to use.
#[derive(Debug, Clone)]
pub enum Source {
    /// A directory; items are files below it.
    Filesystem { root: PathBuf },
    /// A Jellyfin server; items are its library entries.
    Jellyfin(Arc<JellyfinClient>),
}

/// Every configured source, by ID.
#[derive(Debug, Clone, Default)]
pub struct Sources(HashMap<String, Source>);

impl Sources {
    /// Sets up every source in `config`. Makes no network requests.
    pub fn from_config(config: &Config) -> Result<Self, (String, JellyfinError)> {
        let mut sources = HashMap::new();
        for source in &config.sources {
            let runtime = match source {
                SourceConfig::Filesystem { root, .. } => Source::Filesystem {
                    root: expand_home(root),
                },
                SourceConfig::Jellyfin {
                    id,
                    url,
                    api_key,
                    libraries,
                } => Source::Jellyfin(Arc::new(
                    JellyfinClient::new(id, url, api_key.expose(), libraries.clone())
                        .map_err(|err| (id.clone(), err))?,
                )),
            };
            sources.insert(source.id().to_owned(), runtime);
        }
        Ok(Self(sources))
    }

    pub fn get(&self, id: &str) -> Option<&Source> {
        self.0.get(id)
    }
}
