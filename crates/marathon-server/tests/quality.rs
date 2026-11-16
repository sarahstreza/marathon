//! Integration tests for quality profiles (PLAN.md M13): `GET /bandwidth`
//! and `POST /playback/profiles`. The cases that need transcoding to be on
//! use a software encoder and skip (passing, with a note on stderr) where
//! ffmpeg has none.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::StatusCode;
use common::{CHANNELS, TestEnv, get, post};
use marathon_server::App;
use marathon_server::config::Config;
use serde_json::{Value, json};

async fn app_with_transcode(env: &TestEnv, transcode: &str) -> App {
    let toml = env.config_toml_full("", "enabled = false", transcode, CHANNELS);
    App::open(Config::from_toml_str(&toml).unwrap())
        .await
        .unwrap()
}

/// What a desktop client would send.
fn capabilities(hls: bool) -> Value {
    json!({
        "video": [{"codec": "h264", "max_height": 2160, "ten_bit": true}],
        "audio_codecs": ["aac"],
        "hls": hls,
    })
}

fn ids(options: &Value) -> Vec<&str> {
    options
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn bandwidth_sends_exactly_the_bytes_asked_for_uncacheably() {
    let env = TestEnv::new();
    let app = env.app().await;
    for bytes in [1_u64, 1000, 200_000] {
        let reply = get(&app, &format!("/api/v1/bandwidth?bytes={bytes}"))
            .send()
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body.len() as u64, bytes);
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        assert_eq!(
            reply.header("content-length"),
            Some(bytes.to_string().as_str())
        );
        assert_eq!(
            reply.header("content-type"),
            Some("application/octet-stream")
        );
    }
}

#[tokio::test]
async fn bandwidth_refuses_missing_or_silly_sizes() {
    let env = TestEnv::new();
    let app = env.app().await;
    for uri in [
        "/api/v1/bandwidth",
        "/api/v1/bandwidth?bytes=0",
        "/api/v1/bandwidth?bytes=67108865",
        "/api/v1/bandwidth?bytes=lots",
        "/api/v1/bandwidth?bytes=-5",
    ] {
        let reply = get(&app, uri).send().await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(reply.error_code(), "bad_request", "{uri}");
    }
}

#[tokio::test]
async fn without_transcoding_only_the_original_is_offered() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert!(!app.transcode_enabled());
    let reply = post(&app, "/api/v1/playback/profiles")
        .json(json!({"capabilities": capabilities(true), "measured_kbps": 20_000}))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let options: Value = reply.json();
    assert_eq!(ids(&options), ["original"]);
    assert_eq!(options[0]["kind"], "original");
    assert_eq!(options[0]["recommended"], true);
}

#[tokio::test]
async fn with_transcoding_auto_and_the_ladder_follow_the_original() {
    let env = TestEnv::new();
    let app = app_with_transcode(&env, "hardware = \"software\"").await;
    if !app.transcode_enabled() {
        eprintln!("skipping: ffmpeg has no working software H.264 encoder");
        return;
    }
    let ask = |measured: Value, hls: bool| {
        let app = &app;
        async move {
            let reply = post(app, "/api/v1/playback/profiles")
                .json(json!({"capabilities": capabilities(hls), "measured_kbps": measured}))
                .send()
                .await;
            assert_eq!(reply.status, StatusCode::OK);
            reply.json::<Value>()
        }
    };

    let measured = ask(json!(6_000), true).await;
    assert_eq!(
        ids(&measured),
        [
            "original", "auto", "1080p", "720p", "720p-low", "480p", "360p"
        ]
    );
    let recommended: Vec<_> = measured
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["recommended"] == true)
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(recommended, ["auto"]);
    // 60% of 6 Mbit/s: 720p-low (2.1 Mbit/s with audio) fits, 720p (4.1) not.
    let fits = |id: &str| {
        measured
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == id)
            .unwrap()["fits_bandwidth"]
            .clone()
    };
    assert_eq!(fits("720p"), false);
    assert_eq!(fits("720p-low"), true);
    assert_eq!(measured[3]["video_kbps"], 4000);
    assert_eq!(measured[3]["max_height"], 720);

    // Not measured yet: original is the recommendation.
    let unmeasured = ask(Value::Null, true).await;
    let first = &unmeasured[0];
    assert_eq!(
        (first["id"].as_str(), first["recommended"].as_bool()),
        (Some("original"), Some(true))
    );

    // A client that cannot play HLS cannot play a transcode.
    assert_eq!(ids(&ask(json!(6_000), false).await), ["original"]);
}

#[tokio::test]
async fn configured_profiles_replace_the_built_in_ladder() {
    let env = TestEnv::new();
    let app = app_with_transcode(
        &env,
        "hardware = \"software\"\n[[transcode.profile]]\nid = \"tablet\"\nheight = 800\n\
         video_kbps = 1500",
    )
    .await;
    if !app.transcode_enabled() {
        eprintln!("skipping: ffmpeg has no working software H.264 encoder");
        return;
    }
    let reply = post(&app, "/api/v1/playback/profiles")
        .json(json!({"capabilities": capabilities(true)}))
        .send()
        .await;
    assert_eq!(ids(&reply.json::<Value>()), ["original", "auto", "tablet"]);
}

#[tokio::test]
async fn profiles_reject_a_malformed_request() {
    let env = TestEnv::new();
    let app = env.app().await;
    for body in [json!({}), json!({"capabilities": 5}), json!([])] {
        let reply = post(&app, "/api/v1/playback/profiles")
            .json(body)
            .send()
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
}
