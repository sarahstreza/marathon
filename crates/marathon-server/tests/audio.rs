//! Integration tests for audio-only mode (PLAN.md M12): probing durations
//! with ffprobe, and `GET /audio/{item_id}`. These need real media, so they make tiny clips with
//! ffmpeg, and skip (passing, with a note on stderr) where ffmpeg or
//! ffprobe is not installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::http::StatusCode;
use common::{CHANNELS, TestEnv, advance, now};
use marathon_server::App;
use marathon_server::config::Config;

/// The `misc` channel's only item: a real clip in these tests.
const CLIP: &str = "Misc/Café Society's Ünïcode Clip — Ørsted.mkv";

/// Whether `program -version` runs. Prints why a test is skipped if not.
fn have(program: &str) -> bool {
    let ok = Command::new(program)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipping: {program} is not installed");
    }
    ok
}

/// Replaces the fake video at `rel` with a real `seconds`-long clip: a
/// tiny lossless video and a sine tone, so any ffmpeg build can make it.
fn write_clip(env: &TestEnv, rel: &str, seconds: u32) {
    let out = env.library().join(rel);
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s=32x24:r=5:d={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-c:v", "ffv1", "-c:a", "flac", "-shortest"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {rel}");
}

/// A shell script that records each run in `<dir>/probes` and then runs
/// the real ffprobe, for counting probes.
#[cfg(unix)]
fn counting_ffprobe(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("ffprobe-counting");
    let log = dir.join("probes");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n[ \"$1\" = -version ] || echo run >> {log:?}\nexec ffprobe \"$@\"\n",
            log = log.display().to_string()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[cfg(unix)]
fn probe_count(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("probes"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

async fn app_with_audio(env: &TestEnv, audio: &str) -> App {
    let toml = env.config_toml_with_audio("", audio, CHANNELS);
    App::open(Config::from_toml_str(&toml).unwrap())
        .await
        .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn now_probes_a_missing_duration_once_and_stores_it() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let env = TestEnv::new();
    write_clip(&env, CLIP, 3);
    let ffprobe = counting_ffprobe(env.dir.path());
    let app = app_with_audio(
        &env,
        &format!("ffprobe = {:?}", ffprobe.display().to_string()),
    )
    .await;

    let first = now(&app, "misc").await;
    let duration = first["item"]["duration_ms"]
        .as_u64()
        .expect("probed duration");
    assert!((2_900..=3_100).contains(&duration), "{duration}");
    assert_eq!(probe_count(env.dir.path()), 1);

    // Stored: tuning in again (or restarting) needs no probe.
    let again = now(&app, "misc").await;
    assert_eq!(again["item"]["duration_ms"], duration);
    let item = common::get(
        &app,
        &format!("/api/v1/items/{}", first["item"]["id"].as_str().unwrap()),
    )
    .send()
    .await
    .json::<serde_json::Value>();
    assert_eq!(item["duration_ms"], duration);
    assert_eq!(probe_count(env.dir.path()), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn an_unprobeable_item_is_probed_only_once() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    // The `misc` clip stays the test library's fake bytes, which ffprobe
    // cannot read.
    let env = TestEnv::new();
    let ffprobe = counting_ffprobe(env.dir.path());
    let app = app_with_audio(
        &env,
        &format!("ffprobe = {:?}", ffprobe.display().to_string()),
    )
    .await;

    for _ in 0..3 {
        let reply = now(&app, "misc").await;
        assert_eq!(reply["item"]["duration_ms"], serde_json::Value::Null);
    }
    assert_eq!(probe_count(env.dir.path()), 1);
}

#[tokio::test]
async fn the_next_item_is_probed_in_the_background() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let env = TestEnv::new();
    let first = "TV/Test Show/Season 1/Test Show - S01E01 - Pilot.mkv";
    let second = "TV/Test Show/Season 1/Test Show - S01E02 - The Second One.mkv";
    write_clip(&env, first, 2);
    write_clip(&env, second, 4);
    let app = app_with_audio(&env, "").await;

    let tuned = now(&app, "show").await;
    assert_eq!(tuned["next"]["item"]["path"], second);
    // The background probe usually finishes well within this.
    let mut stored = serde_json::Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let id = tuned["next"]["item"]["id"].as_str().unwrap();
        stored = common::get(&app, &format!("/api/v1/items/{id}"))
            .send()
            .await
            .json::<serde_json::Value>()["duration_ms"]
            .clone();
        if !stored.is_null() {
            break;
        }
    }
    let duration = stored.as_u64().expect("next item probed");
    assert!((3_900..=4_100).contains(&duration), "{duration}");
    assert_eq!(advance(&app, "show").await["item"]["duration_ms"], duration);
}

#[tokio::test]
async fn nothing_is_probed_with_audio_off() {
    if !have("ffmpeg") {
        return;
    }
    let env = TestEnv::new();
    write_clip(&env, CLIP, 3);
    let app = env.app().await;
    assert!(!app.audio_enabled());
    assert_eq!(
        now(&app, "misc").await["item"]["duration_ms"],
        serde_json::Value::Null
    );
}

// --- GET /audio/{item_id} ----------------------------------------------------

/// What ffprobe says about an audio stream's bytes: codec, channels, and
/// duration in ms.
fn probe_bytes(dir: &Path, bytes: &[u8]) -> (String, u32, u64) {
    let file = dir.join("probe-me.ogg");
    std::fs::write(&file, bytes).unwrap();
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0"])
        .args([
            "-show_entries",
            "stream=codec_name,channels:format=duration",
        ])
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} in {text}"))
            .to_owned()
    };
    let seconds: f64 = field("duration").parse().unwrap();
    (
        field("codec_name"),
        field("channels").parse().unwrap(),
        (seconds * 1000.0).round() as u64,
    )
}

/// The `misc` clip's item ID.
async fn clip_id(app: &App) -> String {
    common::item_id(app, CLIP).await
}

#[tokio::test]
async fn audio_streams_stereo_opus_in_ogg_from_any_point() {
    if !have("ffmpeg") || !have("ffprobe") {
        return;
    }
    let env = TestEnv::new();
    write_clip(&env, CLIP, 3);
    let app = app_with_audio(&env, "").await;
    assert!(app.audio_enabled());
    let id = clip_id(&app).await;

    let reply = common::get(&app, &format!("/api/v1/audio/{id}"))
        .send()
        .await;
    assert_eq!(
        reply.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.header("content-type"), Some("audio/ogg"));
    assert!(reply.body.starts_with(b"OggS"));
    let (codec, channels, duration) = probe_bytes(env.dir.path(), &reply.body);
    assert_eq!((codec.as_str(), channels), ("opus", 2));
    assert!((2_800..=3_200).contains(&duration), "{duration}");

    let reply = common::get(&app, &format!("/api/v1/audio/{id}?start_ms=2000"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let (_, _, duration) = probe_bytes(env.dir.path(), &reply.body);
    assert!((800..=1_200).contains(&duration), "{duration}");
}

#[tokio::test]
async fn audio_errors() {
    if !have("ffmpeg") {
        return;
    }
    let env = TestEnv::new();
    let app = app_with_audio(&env, "").await;

    let reply = common::get(&app, "/api/v1/audio/no-such-item").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let id = clip_id(&app).await;
    let reply = common::get(&app, &format!("/api/v1/audio/{id}?start_ms=soon"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);

    // The test library's fake bytes are not media: ffmpeg makes nothing.
    let reply = common::get(&app, &format!("/api/v1/audio/{id}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.error_code(), "upstream");

    std::fs::remove_file(env.library().join(CLIP)).unwrap();
    let reply = common::get(&app, &format!("/api/v1/audio/{id}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let off = env.app().await;
    let reply = common::get(&off, &format!("/api/v1/audio/{id}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.error_code(), "unavailable");
}

#[tokio::test]
async fn an_item_without_audio_is_a_bad_gateway() {
    if !have("ffmpeg") {
        return;
    }
    let env = TestEnv::new();
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=32x24:r=5:d=1",
        ])
        .args(["-c:v", "ffv1"])
        .arg(env.library().join(CLIP))
        .status()
        .unwrap();
    assert!(status.success());
    let app = app_with_audio(&env, "").await;
    let id = clip_id(&app).await;
    let reply = common::get(&app, &format!("/api/v1/audio/{id}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert!(
        reply.json::<serde_json::Value>()["message"]
            .as_str()
            .unwrap()
            .contains("ffmpeg")
    );
}

/// The processes whose command line mentions `needle` (Linux's /proc).
/// Zombies have empty command lines, so a killed but unreaped ffmpeg does
/// not count.
#[cfg(target_os = "linux")]
fn processes_mentioning(needle: &str) -> usize {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .filter(|cmdline| String::from_utf8_lossy(cmdline).contains(needle))
        .count()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn streams_are_limited_and_dropping_one_kills_its_ffmpeg() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    if !have("ffmpeg") {
        return;
    }
    let env = TestEnv::new();
    // Long enough that ffmpeg is still running when the response is held.
    write_clip(&env, CLIP, 600);
    let app = app_with_audio(&env, "max_streams = 1").await;
    let id = clip_id(&app).await;
    let open = || {
        let request = Request::get(format!("/api/v1/audio/{id}"))
            .body(Body::empty())
            .unwrap();
        marathon_server::http::router(app.clone()).oneshot(request)
    };

    // Held, unread: ffmpeg blocks on the full pipe and keeps running.
    let held = open().await.unwrap();
    assert_eq!(held.status(), StatusCode::OK);
    let library = env.library().display().to_string();
    assert_eq!(processes_mentioning(&library), 1);

    let busy = open().await.unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);

    drop(held);
    let mut gone = false;
    for _ in 0..100 {
        if processes_mentioning(&library) == 0 {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(gone, "ffmpeg still running after its response was dropped");
    assert_eq!(open().await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn now_offers_audio_urls_exactly_when_audio_is_on() {
    if !have("ffmpeg") {
        return;
    }
    let env = TestEnv::new();
    let on = app_with_audio(&env, "").await;
    let tuned = now(&on, "show").await;
    let id = tuned["item"]["id"].as_str().unwrap();
    let next = tuned["next"]["item"]["id"].as_str().unwrap();
    assert_eq!(tuned["audio_url"], format!("/api/v1/audio/{id}"));
    assert_eq!(tuned["next"]["audio_url"], format!("/api/v1/audio/{next}"));
    assert_eq!(
        advance(&on, "show").await["audio_url"],
        format!("/api/v1/audio/{next}")
    );
    drop(on);

    let off = env.app().await;
    let tuned = now(&off, "show").await;
    assert_eq!(tuned["audio_url"], serde_json::Value::Null);
    assert_eq!(tuned["next"]["audio_url"], serde_json::Value::Null);
}
