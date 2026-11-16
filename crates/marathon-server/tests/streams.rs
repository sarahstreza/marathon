//! Integration tests for how `now`, `advance` and `previous` choose between
//! the original file and a transcode (PLAN.md M13), from the client's stored
//! playback settings. The cases that transcode need real media and a software
//! H.264 encoder, so they make a tiny clip with ffmpeg and skip (passing,
//! with a note on stderr) where ffmpeg, ffprobe or an encoder is missing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::Command;

use axum::http::{Method, StatusCode};
use common::{CHANNELS, TestEnv, call, get, item_id};
use marathon_server::App;
use marathon_server::config::Config;
use serde_json::{Value, json};

/// The `misc` channel's only item: a real clip in these tests.
const CLIP: &str = "Misc/Café Society's Ünïcode Clip — Ørsted.mkv";

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

fn write_clip(env: &TestEnv) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=s=160x90:r=10:d=7")
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=7"])
        .args(["-c:v", "ffv1", "-c:a", "flac", "-shortest"])
        .arg(env.library().join(CLIP))
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make the test clip");
}

/// An app with transcoding on, or `None` (with a note) where it cannot be.
async fn transcoding_app(env: &TestEnv, extra: &str) -> Option<App> {
    if !have("ffmpeg") || !have("ffprobe") {
        return None;
    }
    let transcode = format!("hardware = \"software\"\nsegment_seconds = 2\n{extra}");
    let toml = env.config_toml_full("", "enabled = false", &transcode, CHANNELS);
    let app = App::open(Config::from_toml_str(&toml).unwrap())
        .await
        .unwrap();
    if !app.transcode_enabled() {
        eprintln!("skipping: ffmpeg has no working software H.264 encoder");
        return None;
    }
    Some(app)
}

/// What a client that plays HLS and the test clip's own codecs says.
fn can_play_the_clip(hls: bool) -> Value {
    json!({
        "video": [{"codec": "ffv1", "max_height": 2160, "ten_bit": true}],
        "audio_codecs": ["flac"],
        "hls": hls,
    })
}

/// What a client that plays only H.264 and AAC, so the test clip needs a
/// transcode, says.
fn needs_a_transcode() -> Value {
    json!({
        "video": [{"codec": "h264", "max_height": 2160}],
        "audio_codecs": ["aac"],
        "hls": true,
    })
}

async fn store(app: &App, client: &str, capabilities: Value, profile: &str, kbps: Value) {
    let reply = call(
        app,
        Method::PUT,
        &format!("/api/v1/clients/{client}/playback"),
    )
    .json(json!({
        "capabilities": capabilities,
        "profile": profile,
        "measured_kbps": kbps,
    }))
    .send()
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

/// `GET now` for `channel` as `client`, if it names one.
async fn now_as(app: &App, channel: &str, client: Option<&str>) -> Value {
    let mut request = get(app, &format!("/api/v1/channels/{channel}/now"));
    if let Some(client) = client {
        request = request.header("X-Marathon-Client", client);
    }
    let reply = request.send().await;
    assert_eq!(reply.status, StatusCode::OK);
    reply.json()
}

fn warnings(now: &Value) -> Vec<String> {
    now["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_client_with_no_settings_gets_the_original() {
    let env = TestEnv::new();
    let app = env.app().await;
    for client in [None, Some("never-heard-of-it")] {
        let now = now_as(&app, "show", client).await;
        let id = now["item"]["id"].as_str().unwrap();
        assert_eq!(now["stream_url"], format!("/api/v1/stream/{id}"));
        assert_eq!(now["stream"]["kind"], "direct");
        assert_eq!(now["stream"]["summary"], "Original");
        assert_eq!(now["stream"]["profile"], Value::Null);
        let next = now["next"]["item"]["id"].as_str().unwrap();
        assert_eq!(now["next"]["stream_url"], format!("/api/v1/stream/{next}"));
        assert_eq!(now["next"]["stream"]["kind"], "direct");
        assert!(warnings(&now).is_empty(), "{now}");
    }
}

#[tokio::test]
async fn a_named_quality_gets_a_transcode_and_says_so() {
    let env = TestEnv::new();
    write_clip(&env);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let id = item_id(&app, CLIP).await;
    store(&app, "tablet", needs_a_transcode(), "720p", Value::Null).await;

    let now = now_as(&app, "misc", Some("tablet")).await;
    assert_eq!(
        now["stream_url"],
        format!("/api/v1/transcode/{id}/720p/index.m3u8")
    );
    assert_eq!(now["stream"]["kind"], "transcode");
    assert_eq!(now["stream"]["profile"], "720p");
    let summary = now["stream"]["summary"].as_str().unwrap();
    assert!(summary.starts_with("Transcoding · "), "{summary}");
    assert!(warnings(&now).is_empty(), "{now}");
    // A one-item channel's next item is the same item, chosen the same way.
    assert_eq!(
        now["next"]["stream_url"],
        format!("/api/v1/transcode/{id}/720p/index.m3u8")
    );
    assert_eq!(now["next"]["stream"]["kind"], "transcode");

    // Another client of the same server still gets the original.
    let other = now_as(&app, "misc", Some("phone")).await;
    assert_eq!(other["stream"]["kind"], "direct");
    // And so does the same client once it asks for the original.
    store(&app, "tablet", needs_a_transcode(), "original", Value::Null).await;
    let back = now_as(&app, "misc", Some("tablet")).await;
    assert_eq!(back["stream"]["kind"], "direct");
}

#[tokio::test]
async fn the_playlist_the_stream_url_names_is_served() {
    let env = TestEnv::new();
    write_clip(&env);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    store(&app, "tablet", needs_a_transcode(), "480p", Value::Null).await;
    let now = now_as(&app, "misc", Some("tablet")).await;
    let playlist = get(&app, now["stream_url"].as_str().unwrap()).send().await;
    assert_eq!(playlist.status, StatusCode::OK);
    assert!(
        String::from_utf8(playlist.body)
            .unwrap()
            .contains("#EXT-X-ENDLIST")
    );
}

#[tokio::test]
async fn auto_plays_the_original_when_it_will_do_and_transcodes_when_not() {
    let env = TestEnv::new();
    write_clip(&env);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let id = item_id(&app, CLIP).await;
    let transcode = format!("/api/v1/transcode/{id}/");
    let original = format!("/api/v1/stream/{id}");

    // Can play the clip, and a fast link: no reason to transcode.
    store(
        &app,
        "fast",
        can_play_the_clip(true),
        "auto",
        json!(100_000),
    )
    .await;
    let now = now_as(&app, "misc", Some("fast")).await;
    assert_eq!(now["stream_url"], original);
    assert_eq!(now["stream"]["kind"], "direct");

    // Cannot play it: it is transcoded, whatever the link.
    store(&app, "picky", needs_a_transcode(), "auto", json!(100_000)).await;
    let now = now_as(&app, "misc", Some("picky")).await;
    assert!(now["stream_url"].as_str().unwrap().starts_with(&transcode));
    assert_eq!(now["stream"]["kind"], "transcode");

    // Can play it, but the link measured next to nothing: transcoded.
    store(&app, "slow", can_play_the_clip(true), "auto", json!(100)).await;
    let now = now_as(&app, "misc", Some("slow")).await;
    assert!(now["stream_url"].as_str().unwrap().starts_with(&transcode));
}

#[tokio::test]
async fn advance_and_previous_choose_streams_too() {
    let env = TestEnv::new();
    write_clip(&env);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    store(&app, "tablet", needs_a_transcode(), "720p", Value::Null).await;
    let tuned = now_as(&app, "misc", Some("tablet")).await;
    let body = json!({"from_item_id": tuned["item"]["id"], "reason": "ended"});
    let advanced = call(&app, Method::POST, "/api/v1/channels/misc/advance")
        .header("X-Marathon-Client", "tablet")
        .json(body.clone())
        .send()
        .await;
    assert_eq!(advanced.status, StatusCode::OK);
    assert_eq!(advanced.json::<Value>()["stream"]["kind"], "transcode");
    let previous = call(&app, Method::POST, "/api/v1/channels/misc/previous")
        .header("X-Marathon-Client", "tablet")
        .json(json!({"from_item_id": tuned["item"]["id"]}))
        .send()
        .await;
    assert_eq!(previous.status, StatusCode::OK);
    assert_eq!(previous.json::<Value>()["stream"]["kind"], "transcode");
}

#[tokio::test]
async fn when_a_transcode_cannot_be_given_the_original_is_and_a_warning_says_why() {
    let env = TestEnv::new();
    write_clip(&env);
    let Some(app) = transcoding_app(&env, "max_sessions = 1").await else {
        return;
    };
    let id = item_id(&app, CLIP).await;
    let original = format!("/api/v1/stream/{id}");

    // A client that cannot play HLS cannot play a transcode.
    let mut no_hls = needs_a_transcode();
    no_hls["hls"] = json!(false);
    store(&app, "old", no_hls, "720p", Value::Null).await;
    let now = now_as(&app, "misc", Some("old")).await;
    assert_eq!(now["stream_url"], original);
    assert!(warnings(&now)[0].contains("HLS"), "{now}");

    // An item that cannot be probed cannot have a quality chosen for it
    // (the `show` channel's files are not real media).
    store(&app, "tablet", needs_a_transcode(), "720p", Value::Null).await;
    let now = now_as(&app, "show", Some("tablet")).await;
    assert_eq!(now["stream"]["kind"], "direct");
    assert!(warnings(&now)[0].contains("cannot read"), "{now}");

    // With the one session taken, a second viewer is not given a URL that
    // would fail.
    let busy = now_as(&app, "misc", Some("tablet")).await;
    let playlist = get(&app, busy["stream_url"].as_str().unwrap()).send().await;
    let text = String::from_utf8(playlist.body).unwrap();
    let first = text.lines().find(|l| l.contains(".ts?s=")).unwrap();
    let segment = get(&app, &format!("/api/v1/transcode/{id}/720p/{first}"))
        .send()
        .await;
    assert_eq!(segment.status, StatusCode::OK);
    let full = now_as(&app, "misc", Some("tablet")).await;
    assert_eq!(full["stream_url"], original);
    assert_eq!(full["stream"]["kind"], "direct");
    assert!(warnings(&full)[0].contains("busy"), "{full}");
}

#[tokio::test]
async fn without_transcoding_a_named_quality_warns_and_auto_does_not() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert!(!app.transcode_enabled());
    store(&app, "tablet", needs_a_transcode(), "720p", Value::Null).await;
    let now = now_as(&app, "show", Some("tablet")).await;
    let id = now["item"]["id"].as_str().unwrap();
    assert_eq!(now["stream_url"], format!("/api/v1/stream/{id}"));
    assert_eq!(now["stream"]["kind"], "direct");
    assert!(warnings(&now)[0].contains("cannot transcode"), "{now}");

    store(&app, "tablet", needs_a_transcode(), "auto", json!(5_000)).await;
    let now = now_as(&app, "show", Some("tablet")).await;
    assert_eq!(now["stream"]["kind"], "direct");
    assert!(warnings(&now).is_empty(), "{now}");
}
