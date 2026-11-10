//! Shared helpers for the server integration tests: a fake media library in
//! a temp directory, a config pointing at it, and request helpers that drive
//! the router in-process with `tower::ServiceExt::oneshot`.

// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use marathon_server::App;
use marathon_server::config::Config;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

/// Every video in the test library, relative to its root. Each file's
/// content is its own path repeated, so sizes differ and range requests
/// have something recognisable to return.
pub const VIDEOS: &[&str] = &[
    "TV/Test Show/Season 1/Test Show - S01E01 - Pilot.mkv",
    "TV/Test Show/Season 1/Test Show - S01E02 - The Second One.mkv",
    "TV/Test Show/Season 2/Test Show - S02E01 - New Beginnings.mkv",
    "TV/Test Show/Season 2/Test Show - S02E02 - Cliffhanger.mkv",
    "TV/Test Show/Extras/Blooper Reel.mkv",
    "Essays/Ep 2 - Pacing.mkv",
    "Essays/Ep 10 - Difficulty Curves.mp4",
    "Essays/Deep Dives/why_doors_matter.webm",
    "Misc/Café Society's Ünïcode Clip — Ørsted.mkv",
];

/// The sidecar subtitle in the test library.
pub const SUBTITLE: &str = "TV/Test Show/Season 1/Test Show - S01E01 - Pilot.en.srt";
pub const SUBTITLE_TEXT: &str = "1\n00:00:00,000 --> 00:00:02,000\nHello\n";

/// Channels over the test library:
///
/// - `show` (1): the four Test Show episodes, in order. The non-recursive
///   selector takes in the `Season N` folders but not `Extras`.
/// - `essays` (2): the three essays, shuffled.
/// - `seasons` (3): Test Show grouped by season, seasons shuffled.
/// - `misc` (4): the single unicode clip.
/// - `empty` (5): a folder with no videos.
pub const CHANNELS: &str = r#"
[[channel]]
id = "show"
number = 1
name = "Test Show"
items = [{ source = "lib", path = "TV/Test Show" }]
rule = { kind = "ordered" }

[[channel]]
id = "essays"
number = 2
name = "Essays"
items = [{ source = "lib", path = "Essays", recursive = true }]
rule = { kind = "shuffled" }

[[channel]]
id = "seasons"
number = 3
name = "Seasons"
items = [{ source = "lib", path = "TV/Test Show" }]
rule = { kind = "grouped", by = "season", groups = "shuffled", within = "ordered" }

[[channel]]
id = "misc"
number = 4
name = "Misc"
items = [{ source = "lib", path = "Misc" }]
rule = { kind = "ordered" }

[[channel]]
id = "empty"
number = 5
name = "Empty"
items = [{ source = "lib", path = "Nothing Here" }]
rule = { kind = "ordered" }
"#;

/// A temp directory holding a media library (`library/`) and a data
/// directory (`data/`). Deleted when dropped.
pub struct TestEnv {
    pub dir: TempDir,
}

impl TestEnv {
    /// Creates the library described by [`VIDEOS`] and [`SUBTITLE`].
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let env = Self { dir };
        for video in VIDEOS {
            env.write_video(video);
        }
        fs::write(env.library().join(SUBTITLE), SUBTITLE_TEXT).unwrap();
        fs::create_dir_all(env.library().join("Nothing Here")).unwrap();
        fs::write(env.library().join("Nothing Here/readme.txt"), "not a video").unwrap();
        env
    }

    pub fn library(&self) -> PathBuf {
        self.dir.path().join("library")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// Writes a fake video whose content is derived from its path.
    pub fn write_video(&self, rel: &str) {
        let path = self.library().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, video_bytes(rel)).unwrap();
    }

    /// Config TOML with the given `[server]` extras and channels.
    pub fn config_toml(&self, server_extra: &str, channels: &str) -> String {
        format!(
            "[server]\nbind = \"127.0.0.1:0\"\ndata_dir = {data:?}\n{server_extra}\n\
             [[source]]\nid = \"lib\"\nkind = \"filesystem\"\nroot = {root:?}\n{channels}",
            data = self.data_dir().display().to_string(),
            root = self.library().display().to_string(),
        )
    }

    pub fn config(&self) -> Config {
        Config::from_toml_str(&self.config_toml("", CHANNELS)).unwrap()
    }

    /// Opens the app (database in `data/`, initial scan done).
    pub async fn app(&self) -> App {
        App::open(self.config()).await.unwrap()
    }
}

/// The content of the fake video at `rel`.
pub fn video_bytes(rel: &str) -> Vec<u8> {
    rel.repeat(20).into_bytes()
}

/// A response, with the body collected.
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "{err}: status {}, body {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    /// The `code` of an error body.
    pub fn error_code(&self) -> String {
        self.json::<Value>()["code"].as_str().unwrap().to_owned()
    }
}

/// A request builder bound to an app.
pub struct Call<'a> {
    app: &'a App,
    request: axum::http::request::Builder,
    body: Body,
}

pub fn get<'a>(app: &'a App, uri: &str) -> Call<'a> {
    call(app, Method::GET, uri)
}

pub fn post<'a>(app: &'a App, uri: &str) -> Call<'a> {
    call(app, Method::POST, uri)
}

pub fn call<'a>(app: &'a App, method: Method, uri: &str) -> Call<'a> {
    Call {
        app,
        request: Request::builder().method(method).uri(uri),
        body: Body::empty(),
    }
}

impl Call<'_> {
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.request = self.request.header(name, value);
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.request = self
            .request
            .header(header::CONTENT_TYPE, "application/json");
        self.body = Body::from(body.to_string());
        self
    }

    pub async fn send(self) -> Reply {
        let request = self.request.body(self.body).unwrap();
        let response = marathon_server::http::router(self.app.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: body.to_vec(),
        }
    }
}

/// The item ID for a library path.
pub async fn item_id(app: &App, path: &str) -> String {
    let row: (String,) = sqlx::query_as("SELECT id FROM items WHERE path = ?")
        .bind(path)
        .fetch_one(app.db().pool())
        .await
        .unwrap();
    row.0
}

/// Calls `GET now` for a channel and returns the JSON.
pub async fn now(app: &App, channel: &str) -> Value {
    let reply = get(app, &format!("/api/v1/channels/{channel}/now"))
        .send()
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    reply.json()
}

/// Advances a channel from its current item and returns the new `now`.
pub async fn advance(app: &App, channel: &str) -> Value {
    let current = now(app, channel).await;
    let reply = post(app, &format!("/api/v1/channels/{channel}/advance"))
        .json(serde_json::json!({
            "from_item_id": current["item"]["id"],
            "reason": "ended",
        }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    reply.json()
}

/// The path of the item a `now` response is on.
pub fn path_of(now: &Value) -> &str {
    now["item"]["path"].as_str().unwrap()
}
