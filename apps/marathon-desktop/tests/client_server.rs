//! The client library against a real server: `HttpApi` talking to an
//! `EmbeddedServer` (what `--embedded` runs) on its loopback port, and the
//! `Tuner` driving a mock player through it. This checks that client and
//! server agree on the wire format and semantics, which the tuner's unit
//! tests (against an in-memory fake) cannot.
//!
//! It lives in the desktop crate because that is the only crate allowed to
//! depend on both (PLAN.md §4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use marathon_api::{Action, AdvanceReason, AdvanceRequest, ErrorCode, EventKind, ProgressReport};
use marathon_client::api::{Api, ApiError, HttpApi};
use marathon_client::player::{LoadRequest, Player, PlayerError, PlayerEvent};
use marathon_client::tuner::{SystemClock, Tuner, TunerConfig};
use marathon_server::config::Config;
use marathon_server::embedded::EmbeddedServer;
use tempfile::TempDir;

const VIDEOS: &[&str] = &[
    "Show/Season 1/Show - S01E01 - One.mkv",
    "Show/Season 1/Show - S01E02 - Two.mkv",
    "Show/Season 1/Show - S01E03 - Three.mkv",
    "Show/Season 1/Show - S01E03 - Three.en.srt",
    "Other/Clip A.mkv",
    "Other/Clip B.mkv",
];

/// An embedded server over a temp library, stopped on drop.
struct TestServer {
    // Declared first so it stops before the directory is deleted.
    server: EmbeddedServer,
    _dir: TempDir,
}

impl TestServer {
    fn start(token: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for rel in VIDEOS {
            let path = dir.path().join("lib").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rel.as_bytes()).unwrap();
        }
        let config = Config::from_toml_str(&config_toml(dir.path(), token)).unwrap();
        let server = EmbeddedServer::start(config).unwrap();
        Self { server, _dir: dir }
    }

    fn url(&self) -> String {
        self.server.base_url()
    }

    fn api(&self, client_id: &str) -> HttpApi {
        HttpApi::new(&self.url(), None, client_id).unwrap()
    }
}

fn config_toml(dir: &Path, token: Option<&str>) -> String {
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

#[test]
fn http_api_round_trips_every_call() {
    let server = TestServer::start(None);
    let api = server.api("client-a");

    let channels = api.channels().unwrap();
    let numbers: Vec<u32> = channels.iter().map(|c| c.number).collect();
    assert_eq!(numbers, [1, 2]);
    assert_eq!(channels[0].item_count, 3);

    let now = api.now("show").unwrap();
    assert_eq!(now.item.title, "One");
    assert_eq!(
        now.stream_url,
        format!("{}/api/v1/stream/{}", server.url(), now.item.id)
    );
    let next = now.next.clone().unwrap();
    assert_eq!(next.item.title, "Two");
    assert!(next.stream_url.starts_with(&server.url()));

    let report = ProgressReport {
        item_id: now.item.id.clone(),
        position_ms: 42_000,
        duration_ms: Some(1_200_000),
        paused: false,
    };
    api.progress("show", &report).unwrap();
    let now = api.now("show").unwrap();
    assert_eq!(now.position_ms, 42_000);
    assert_eq!(now.item.duration_ms, Some(1_200_000));

    let advance = AdvanceRequest {
        from_item_id: now.item.id.clone(),
        reason: AdvanceReason::Skipped,
    };
    let after = api.advance("show", &advance).unwrap();
    assert_eq!(after.item.title, "Two");
    // Idempotent: the same request again does not move the channel.
    assert_eq!(
        api.advance("show", &advance).unwrap().item.id,
        after.item.id
    );

    // Progress for the old item is stale.
    let err = api.progress("show", &report).unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::StaleItem), "{err}");

    let back = api
        .previous(
            "show",
            &marathon_api::PreviousRequest {
                from_item_id: after.item.id.clone(),
            },
        )
        .unwrap();
    assert_eq!(back.item.title, "One");

    let history = api.history(10).unwrap();
    assert_eq!(history[0].kind, EventKind::Previous);
    assert!(
        history
            .iter()
            .all(|e| e.client_id.as_deref() == Some("client-a"))
    );

    let err = api.now("nope").unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound), "{err}");
}

#[test]
fn subtitle_urls_are_absolute() {
    let server = TestServer::start(None);
    let api = server.api("c");
    let mut now = api.now("show").unwrap();
    for _ in 0..2 {
        now = api
            .advance(
                "show",
                &AdvanceRequest {
                    from_item_id: now.item.id.clone(),
                    reason: AdvanceReason::Skipped,
                },
            )
            .unwrap();
    }
    assert_eq!(now.item.title, "Three");
    let sub = &now.item.subtitles[0];
    assert_eq!(
        sub.url,
        format!("{}/api/v1/subtitles/{}/0", server.url(), now.item.id)
    );
}

#[test]
fn token_is_sent_when_configured() {
    let server = TestServer::start(Some("sesame"));
    let err = server.api("c").channels().unwrap_err();
    assert!(
        matches!(
            err,
            ApiError::Server {
                status: 401,
                code: Some(ErrorCode::Unauthorized),
                ..
            }
        ),
        "{err}"
    );
    let api = HttpApi::new(&server.url(), Some("sesame"), "c").unwrap();
    assert_eq!(api.channels().unwrap().len(), 2);
}

/// Records loads; position is set by the test.
#[derive(Clone, Default)]
struct Recorder {
    loads: Arc<Mutex<Vec<LoadRequest>>>,
    position: Arc<Mutex<Option<u64>>>,
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

#[test]
fn tuner_against_a_real_server() {
    let server = TestServer::start(None);
    let player = Recorder::default();
    let config = TunerConfig {
        client_id: Some("tv".into()),
        ..TunerConfig::default()
    };
    let mut tuner = Tuner::new(
        server.api("tv"),
        player.clone(),
        SystemClock,
        config.clone(),
    );
    let last_load = || player.loads.lock().unwrap().last().cloned().unwrap();

    tuner.start(None).unwrap();
    assert_eq!(tuner.now().unwrap().channel_number, 1);
    assert_eq!(last_load().start_ms, 0);

    // Watch 5 minutes, flip to channel 2, flip back: resumes 5s early.
    *player.position.lock().unwrap() = Some(300_000);
    tuner.handle_action(Action::ChannelUp).unwrap();
    assert_eq!(tuner.now().unwrap().channel_number, 2);
    tuner.handle_action(Action::ChannelDown).unwrap();
    assert_eq!(last_load().start_ms, 295_000);

    // The episode ends: the next one plays from the start.
    tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(tuner.now().unwrap().item.title, "Two");
    assert!(last_load().url.ends_with(&tuner.now().unwrap().item.id));

    // Quit on channel 2, then a fresh tuner for the same client starts
    // there, at the reported position.
    tuner
        .handle_action(Action::GoToChannel { number: 2 })
        .unwrap();
    *player.position.lock().unwrap() = Some(60_000);
    tuner.handle_action(Action::Quit).unwrap();
    let mut again = Tuner::new(server.api("tv"), player.clone(), SystemClock, config);
    again.start(None).unwrap();
    assert_eq!(again.now().unwrap().channel_number, 2);
    assert_eq!(last_load().start_ms, 55_000);
}
