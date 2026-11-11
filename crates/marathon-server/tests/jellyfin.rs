//! Jellyfin sources (M8) against a mock Jellyfin: the catalog from its API,
//! durations, proxied streams with ranges, rescans, an unreachable server,
//! and keeping the API key secret.
//!
//! The mock serves the recorded responses in `fixtures/jellyfin/` (see its
//! README) under a `/jellyfin` path prefix, as behind a reverse proxy, and
//! insists on the API key.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use common::{CHANNELS, TestEnv};
use marathon_api::{ChannelSummary, Item, Now, ScanReport};
use marathon_server::Server;
use marathon_server::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const KEY: &str = "jf-test-key-8c1d0f";

const SHOWS: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
const FILMS: &str = "f00df00df00df00df00df00df00df00d";

fn fixture(name: &str) -> Value {
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
struct MockJellyfin {
    state: Mock,
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl MockJellyfin {
    async fn start() -> Self {
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

    fn url(&self) -> String {
        format!("http://{}/jellyfin", self.addr)
    }

    fn requests(&self) -> Vec<String> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Adds an episode to a library, as if Jellyfin had found a new file.
    fn add_episode(&self, id: &str, season: u32, episode: u32, name: &str) {
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
fn media_bytes(id: &str) -> Vec<u8> {
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
fn jellyfin_channels(url: &str, key: &str) -> String {
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
struct Running {
    addr: SocketAddr,
    http: reqwest::Client,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Running {
    async fn start(env: &TestEnv, jellyfin_url: &str, key: &str) -> Self {
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

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.http.get(self.url(path)).send().await.unwrap()
    }

    async fn json<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
        let response = self.get(path).await;
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.json().await.unwrap()
    }

    async fn channel(&self, id: &str) -> ChannelSummary {
        let channels: Vec<ChannelSummary> = self.json("/api/v1/channels").await;
        channels.into_iter().find(|c| c.id == id).unwrap()
    }

    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

async fn advance(server: &Running, channel: &str, from: &str) -> Now {
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

#[tokio::test]
async fn jellyfin_libraries_become_items_with_metadata_and_durations() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;

    let office = server.channel("office").await;
    // The virtual (missing) episode is left out.
    assert_eq!(office.item_count, 3);
    let films = server.channel("films").await;
    assert_eq!(films.item_count, 2);

    // Ordered by season and episode from Jellyfin's metadata, with
    // durations known before anyone has played anything.
    let mut now: Now = server.json("/api/v1/channels/office/now").await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let item = &now.item;
        seen.push((
            item.season,
            item.episode,
            item.title.clone(),
            item.duration_ms,
        ));
        now = advance(&server, "office", &item.id.clone()).await;
    }
    assert_eq!(
        seen,
        vec![
            (Some(1), Some(1), "Pilot".to_owned(), Some(1_374_000)),
            (Some(3), Some(14), "The Injury".to_owned(), Some(1_320_000)),
            (Some(3), Some(15), "The Return".to_owned(), Some(1_308_000)),
        ]
    );
    // Paths mirror the library's folders, under the library's name.
    let item: Item = server.json(&format!("/api/v1/items/{}", now.item.id)).await;
    assert_eq!(item.source_id, "jf");
    assert_eq!(
        item.path,
        "Shows/The Office (US)/Season 1/The Office (US) - S01E01 - Pilot.mkv"
    );
    assert_eq!(item.show.as_deref(), Some("The Office (US)"));
    assert!(item.subtitles.is_empty());

    // Films come from both of the library's folders.
    let now: Now = server.json("/api/v1/channels/films/now").await;
    let mut paths = vec![now.item.path.clone()];
    let next = advance(&server, "films", &now.item.id).await;
    paths.push(next.item.path);
    paths.sort();
    assert_eq!(
        paths,
        [
            "Films/Alien (1979).mp4",
            "Films/Heat (1995)/Heat (1995).mkv"
        ]
    );

    // The client paged through results (the mock returns two at a time),
    // and asked only for videos.
    let requests = jellyfin.requests();
    let pages = requests
        .iter()
        .filter(|r| r.contains("/Items?") && r.contains(SHOWS))
        .count();
    assert_eq!(pages, 2, "{requests:#?}");
    assert!(requests.iter().all(|r| !r.contains(KEY)), "{requests:#?}");

    // Filesystem channels are unaffected.
    assert_eq!(server.channel("show").await.item_count, 4);
    server.stop().await;
}

#[tokio::test]
async fn streams_are_proxied_with_ranges() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    let now: Now = server.json("/api/v1/channels/office/now").await;
    let jellyfin_id = "0ffe0101aaaaaaaaaaaaaaaaaaaaaaaa"; // Pilot
    let expected = media_bytes(jellyfin_id);
    let stream = server.url(&now.stream_url);

    // The whole file.
    let response = server.http.get(&stream).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "1000");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(response.bytes().await.unwrap(), expected);

    // A range.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 100-199/1000");
    assert_eq!(response.headers()["content-length"], "100");
    assert_eq!(response.bytes().await.unwrap(), expected[100..200]);

    // An open-ended range, as players use when seeking.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=900-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 900-999/1000");
    assert_eq!(response.bytes().await.unwrap(), expected[900..]);

    // An unsatisfiable range.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=5000-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 416);
    assert_eq!(response.headers()["content-range"], "bytes */1000");

    // HEAD, which mpv uses to probe.
    let response = server.http.head(&stream).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "1000");

    // An item Jellyfin has lost is a 404, not a 502.
    std::fs::remove_file(
        jellyfin
            .state
            .lock()
            .unwrap()
            .media
            .path()
            .join(format!("{jellyfin_id}.mkv")),
    )
    .unwrap();
    let response = server.http.get(&stream).send().await.unwrap();
    assert_eq!(response.status(), 404);
    server.stop().await;
}

#[tokio::test]
async fn rescanning_picks_up_episodes_added_in_jellyfin() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    assert_eq!(server.channel("office").await.item_count, 3);

    jellyfin.add_episode("0ffe0317aaaaaaaaaaaaaaaaaaaaaaaa", 3, 17, "Lecture Circuit");
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report.added, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(server.channel("office").await.item_count, 4);

    // A client reporting a slightly different duration does not override
    // Jellyfin's...
    let now: Now = server.json("/api/v1/channels/office/now").await;
    let jellyfin_duration = now.item.duration_ms.unwrap();
    let response = server
        .http
        .post(server.url("/api/v1/channels/office/progress"))
        .json(&json!({
            "item_id": now.item.id,
            "position_ms": 60_000,
            "duration_ms": jellyfin_duration + 480,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let item: Item = server.json(&format!("/api/v1/items/{}", now.item.id)).await;
    assert_eq!(item.duration_ms, Some(jellyfin_duration));

    // ...so a rescan with nothing new changes nothing.
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((report.added, report.updated), (0, 0), "{report:?}");
    server.stop().await;
}

#[tokio::test]
async fn an_unreachable_jellyfin_is_a_clear_error_and_files_keep_working() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    // First with Jellyfin up, so the catalog has its items...
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    let now: Now = server.json("/api/v1/channels/office/now").await;
    server.stop().await;

    // ...then with it gone (nothing listens on its port any more).
    let url = jellyfin.url();
    drop(jellyfin);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let server = Running::start(&env, &url, KEY).await;

    // The server started; the scan reports the source and keeps its items.
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].contains("source jf"), "{report:?}");
    assert!(
        report.errors[0].contains("cannot reach Jellyfin"),
        "{report:?}"
    );
    assert_eq!(server.channel("office").await.item_count, 3);

    // Streaming fails fast with a 502 that says why, rather than hanging.
    let started = Instant::now();
    let response = server.get(&now.stream_url).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "upstream");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("cannot reach Jellyfin source jf"),
        "{body}"
    );

    // Filesystem channels play as usual.
    let show: Now = server.json("/api/v1/channels/show/now").await;
    assert_eq!(server.get(&show.stream_url).await.status(), 200);
    server.stop().await;
}

/// A log writer that keeps everything in memory.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The API key appears in no log line (at any level), no API response, and
/// not in the database, whether Jellyfin accepts it or not.
#[tokio::test(flavor = "current_thread")]
async fn the_api_key_never_appears_in_logs_or_responses() {
    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let jellyfin = MockJellyfin::start().await;
    let mut bodies = Vec::new();
    for key in [KEY, "wrong-key-5d2a91"] {
        let env = TestEnv::new();
        let server = Running::start(&env, &jellyfin.url(), key).await;
        let mut paths = vec![
            "/api/v1/channels".to_owned(),
            "/api/v1/channels/office/now".to_owned(),
            "/api/v1/history".to_owned(),
            "/api/v1/openapi.json".to_owned(),
        ];
        if key == KEY {
            let now: Now = server.json("/api/v1/channels/office/now").await;
            paths.push(format!("/api/v1/items/{}", now.item.id));
            paths.push(now.stream_url.clone());
        }
        for path in &paths {
            let response = server.get(path).await;
            bodies.push(String::from_utf8_lossy(&response.bytes().await.unwrap()).into_owned());
        }
        let rescan = server
            .http
            .post(server.url("/api/v1/library/rescan"))
            .send()
            .await
            .unwrap();
        let report = rescan.text().await.unwrap();
        if key != KEY {
            // A refused key is reported, without the key.
            assert!(report.contains("refused the API key"), "{report}");
        }
        bodies.push(report);
        let sources: Vec<(String,)> = sqlx::query_as("SELECT config_json FROM sources")
            .fetch_all(
                &marathon_server::db::Db::open(
                    &env.data_dir().join(marathon_server::DATABASE_FILE),
                )
                .await
                .unwrap()
                .pool()
                .clone(),
            )
            .await
            .unwrap();
        bodies.extend(sources.into_iter().map(|(json,)| json));
        server.stop().await;
    }

    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    // The capture works: requests to Jellyfin were logged in detail.
    assert!(logs.contains("Jellyfin"), "nothing captured?");
    for key in [KEY, "wrong-key-5d2a91"] {
        assert!(!logs.contains(key), "key in logs:\n{logs}");
        for body in &bodies {
            assert!(!body.contains(key), "key in a response: {body}");
        }
    }
}
