//! Shared helpers for the client-against-server tests: a small library in
//! a temp directory, a config over it, an embedded server, and a mock
//! player.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use marathon_client::api::HttpApi;
use marathon_client::player::{LoadRequest, Player, PlayerError};
use marathon_server::config::Config;
use marathon_server::embedded::EmbeddedServer;
use tempfile::TempDir;

pub const VIDEOS: &[&str] = &[
    "Show/Season 1/Show - S01E01 - One.mkv",
    "Show/Season 1/Show - S01E02 - Two.mkv",
    "Show/Season 1/Show - S01E03 - Three.mkv",
    "Show/Season 1/Show - S01E03 - Three.en.srt",
    "Other/Clip A.mkv",
    "Other/Clip B.mkv",
];

/// An embedded server over a temp library, stopped on drop.
pub struct TestServer {
    // Declared first so it stops before the directory is deleted.
    server: EmbeddedServer,
    _dir: TempDir,
}

impl TestServer {
    pub fn start(token: Option<&str>) -> Self {
        let dir = library();
        let config = Config::from_toml_str(&config_toml(dir.path(), token)).unwrap();
        let server = EmbeddedServer::start(config).unwrap();
        Self { server, _dir: dir }
    }

    pub fn url(&self) -> String {
        self.server.base_url()
    }

    pub fn api(&self, client_id: &str) -> HttpApi {
        HttpApi::new(&self.url(), None, client_id).unwrap()
    }
}

/// A temp directory holding the [`VIDEOS`] library in `lib/`; the
/// config from [`config_toml`] puts the database in `data/`.
pub fn library() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for rel in VIDEOS {
        let path = dir.path().join("lib").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel.as_bytes()).unwrap();
    }
    dir
}

/// A server config over [`library`]'s directory, binding an OS-assigned
/// loopback port. Channels: `show` (1) and `other` (2).
pub fn config_toml(dir: &Path, token: Option<&str>) -> String {
    let token = token
        .map(|t| format!("token = \"{t}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
[server]
bind = "127.0.0.1:0"
data_dir = "{data}"
{token}
[[source]]
id = "lib"
kind = "filesystem"
root = "{lib}"

[[channel]]
id = "show"
number = 1
name = "Show"
items = [{{ source = "lib", path = "Show" }}]
rule = {{ kind = "ordered" }}

[[channel]]
id = "other"
number = 2
name = "Other"
items = [{{ source = "lib", path = "Other" }}]
rule = {{ kind = "ordered" }}
"#,
        data = dir.join("data").display(),
        lib = dir.join("lib").display(),
    )
}

/// Records loads; position is set by the test.
#[derive(Clone, Default)]
pub struct Recorder {
    pub loads: Arc<Mutex<Vec<LoadRequest>>>,
    pub position: Arc<Mutex<Option<u64>>>,
}

impl Player for Recorder {
    fn load(&mut self, request: &LoadRequest) -> Result<(), PlayerError> {
        self.loads.lock().unwrap().push(request.clone());
        *self.position.lock().unwrap() = Some(request.start_ms);
        Ok(())
    }
    fn stop(&mut self) {}
    fn set_paused(&mut self, _: bool) {}
    fn is_paused(&self) -> bool {
        false
    }
    fn seek_relative(&mut self, _: i32) {}
    fn position_ms(&self) -> Option<u64> {
        *self.position.lock().unwrap()
    }
    fn duration_ms(&self) -> Option<u64> {
        Some(1_200_000)
    }
    fn toggle_subtitles(&mut self) {}
}
