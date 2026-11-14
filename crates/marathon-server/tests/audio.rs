//! Integration tests for audio-only mode (PLAN.md M12): probing durations
//! with ffprobe. These need real media, so they make tiny clips with
//! ffmpeg, and skip (passing, with a note on stderr) where ffmpeg or
//! ffprobe is not installed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

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
