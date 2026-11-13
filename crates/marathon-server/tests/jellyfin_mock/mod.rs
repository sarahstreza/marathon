//! A mock Jellyfin and a Marathon server pointed at it, shared by the
//! Jellyfin integration tests (`jellyfin.rs`, `jellyfin_key_secrecy.rs`).
//!
//! The mock serves the recorded responses in `fixtures/jellyfin/` (see its
//! README) under a `/jellyfin` path prefix, as behind a reverse proxy, and
//! insists on the API key.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use marathon_api::{ChannelSummary, Now};
use marathon_server::Server;
use marathon_server::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::common::{CHANNELS, TestEnv};

pub const KEY: &str = "jf-test-key-8c1d0f";

pub const SHOWS: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
pub const FILMS: &str = "f00df00df00df00df00df00df00df00d";

pub fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/jellyfin")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// What the mock knows and has seen.
struct MockState {
    /// Every item below each library, by library item ID.
    items: HashMap<String, Vec<Value>>,
    media: TempDir,
    /// Paths of the requests made, with their queries.
    requests: Vec<String>,
    /// The most items one `/Items` page returns, whatever `Limit` asks,
    /// to make the client page.
    page_cap: usize,
}

type Mock = Arc<Mutex<MockState>>;

/// A mock Jellyfin on a loopback port.
pub struct MockJellyfin {
    state: Mock,
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl MockJellyfin {
    pub async fn start() -> Self {
        let media = tempfile::tempdir().unwrap();
        let mut items = HashMap::new();
        for (library, file) in [(SHOWS, "items_shows.json"), (FILMS, "items_films.json")] {
            let list = fixture(file)["Items"].as_array().unwrap().clone();
            for item in &list {
                write_media(&media, item["Id"].as_str().unwrap());
            }
            items.insert(library.to_owned(), list);
        }
        let state = Arc::new(Mutex::new(MockState {
            items,
            media,
            requests: Vec::new(),
            page_cap: 2,
        }));
        let routes = Router::new()
            .route("/Library/VirtualFolders", get(virtual_folders))
            .route("/Items", get(list_items))
            .route("/Videos/{id}/stream", get(stream).head(stream))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                check_key,
            ))
            .with_state(state.clone());
        let app = Router::new().nest("/jellyfin", routes);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { state, addr, task }
    }

    pub fn url(&self) -> String {
        format!("http://{}/jellyfin", self.addr)
    }

    pub fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Where the mock keeps item `id`'s media file (delete it to make
    /// Jellyfin lose the item).
    pub fn media_file(&self, id: &str) -> PathBuf {
        self.state
            .lock()
            .unwrap()
            .media
            .path()
            .join(format!("{id}.mkv"))
    }

    /// Adds an episode to a library, as if Jellyfin had found a new file.
    pub fn add_episode(&self, id: &str, season: u32, episode: u32, name: &str) {
        let mut state = self.state.lock().unwrap();
        write_media(&state.media, id);
        let path = format!(
            "/media/tv/The Office (US)/Season {season}/The Office (US) - S{season:02}E{episode:02} - {name}.mkv"
        );
        state.items.get_mut(SHOWS).unwrap().push(json!({
            "Name": name, "Id": id, "Type": "Episode", "SeriesName": "The Office (US)",
            "ParentIndexNumber": season, "IndexNumber": episode, "RunTimeTicks": 12_000_000_000_i64,
            "Path": path, "LocationType": "FileSystem",
            "MediaSources": [{"Id": id, "Path": path, "Size": 900}],
        }));
    }
}

impl Drop for MockJellyfin {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The fake media file served for item `id`: 1000 recognisable bytes.
pub fn media_bytes(id: &str) -> Vec<u8> {
    id.bytes().cycle().take(1000).collect()
}

fn write_media(dir: &TempDir, id: &str) {
    std::fs::write(dir.path().join(format!("{id}.mkv")), media_bytes(id)).unwrap();
}

async fn check_key(
    State(state): State<Mock>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let uri = request.uri().to_string();
    state.lock().unwrap().requests.push(uri);
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !(auth.starts_with("MediaBrowser ") && auth.contains(&format!("Token=\"{KEY}\""))) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn virtual_folders() -> Json<Value> {
    Json(fixture("virtual_folders.json"))
}

async fn list_items(
    State(state): State<Mock>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    let state = state.lock().unwrap();
    let types: Vec<&str> = query["IncludeItemTypes"].split(',').collect();
    let excluded: Vec<&str> = query
        .get("ExcludeLocationTypes")
        .map(|v| v.split(',').collect())
        .unwrap_or_default();
    let matching: Vec<&Value> = state
        .items
        .get(&query["ParentId"])
        .map(|items| {
            items
                .iter()
                .filter(|i| types.contains(&i["Type"].as_str().unwrap()))
                .filter(|i| !excluded.contains(&i["LocationType"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    let start: usize = query["StartIndex"].parse().unwrap();
    let limit: usize = query["Limit"].parse().unwrap();
    let page: Vec<&Value> = matching
        .iter()
        .skip(start)
        .take(limit.min(state.page_cap))
        .copied()
        .collect();
    Json(json!({"Items": page, "TotalRecordCount": matching.len(), "StartIndex": start}))
}

async fn stream(
    State(state): State<Mock>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    // Marathon must ask for the original file, never a transcode.
    assert_eq!(query.get("static").map(String::as_str), Some("true"));
    let file = state.lock().unwrap().media.path().join(format!("{id}.mkv"));
    if !file.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match ServeFile::new(file).oneshot(request).await {
        Ok(response) => response.into_response(),
        Err(never) => match never {},
    }
}

/// Channels over the Jellyfin source `jf` (next to the filesystem source
/// `lib` from [`CHANNELS`]).
pub fn jellyfin_channels(url: &str, key: &str) -> String {
    format!(
        r#"{CHANNELS}
[[source]]
id = "jf"
kind = "jellyfin"
url = "{url}"
api_key = "{key}"

[[channel]]
id = "office"
number = 10
name = "The Office"
items = [{{ source = "jf", path = "Shows/The Office (US)" }}]
rule = {{ kind = "ordered" }}

[[channel]]
id = "films"
number = 11
name = "Films"
items = [{{ source = "jf", path = "Films", recursive = true }}]
rule = {{ kind = "ordered" }}
"#
    )
}

/// A Marathon server on a loopback port.
pub struct Running {
    addr: SocketAddr,
    pub http: reqwest::Client,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    pub async fn start(env: &TestEnv, jellyfin_url: &str, key: &str) -> Self {
        let toml = env.config_toml("", &jellyfin_channels(jellyfin_url, key));
        let server = Server::bind(Config::from_toml_str(&toml).unwrap())
            .await
            .unwrap();
        let addr = server.local_addr();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(server.serve(async {
            let _ = stopped.await;
        }));
        Self {
            addr,
            http: reqwest::Client::new(),
            stop: Some(stop),
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(self.url(path)).send().await.unwrap()
    }

    pub async fn json<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
        let response = self.get(path).await;
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.json().await.unwrap()
    }

    pub async fn channel(&self, id: &str) -> ChannelSummary {
        let channels: Vec<ChannelSummary> = self.json("/api/v1/channels").await;
        channels.into_iter().find(|c| c.id == id).unwrap()
    }

    pub async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

pub async fn advance(server: &Running, channel: &str, from: &str) -> Now {
    server
        .http
        .post(server.url(&format!("/api/v1/channels/{channel}/advance")))
        .json(&json!({"from_item_id": from, "reason": "skipped"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
