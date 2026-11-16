//! Integration tests for live transcoding (PLAN.md M13): the HLS playlist
//! and segments under `/transcode`. They need real media and a software
//! H.264 encoder, so they make a tiny clip with ffmpeg and skip (passing,
//! with a note on stderr) where ffmpeg or ffprobe is missing or has none.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::Command;

use axum::http::StatusCode;
use common::{CHANNELS, TestEnv, get, item_id};
use marathon_server::App;
use marathon_server::config::Config;

/// The `misc` channel's only item: a real clip in these tests.
const CLIP: &str = "Misc/Café Society's Ünïcode Clip — Ørsted.mkv";

/// An item that stays the test library's fake bytes, which ffprobe cannot
/// read.
const FAKE: &str = "Essays/Ep 2 - Pacing.mkv";

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

/// Replaces the fake video at `CLIP` with a real `seconds`-long clip.
fn write_clip(env: &TestEnv, seconds: u32) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=s=160x90:r=10:d={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-c:v", "ffv1", "-c:a", "flac", "-shortest"])
        .arg(env.library().join(CLIP))
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make the test clip");
}

/// An app with transcoding on, using 2 s segments so a short clip has several,
/// or `None` (with a note) if this machine cannot transcode.
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

fn playlist_uri(item: &str, profile: &str) -> String {
    format!("/api/v1/transcode/{item}/{profile}/index.m3u8")
}

/// The segment URIs of a playlist, resolved against `base`'s directory.
fn segment_uris(playlist: &str, item: &str, profile: &str) -> Vec<String> {
    playlist
        .lines()
        .filter(|l| l.contains(".ts?s="))
        .map(|l| format!("/api/v1/transcode/{item}/{profile}/{l}"))
        .collect()
}

#[tokio::test]
async fn the_playlist_lists_every_segment_of_the_item() {
    let env = TestEnv::new();
    write_clip(&env, 7);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let item = item_id(&app, CLIP).await;
    let reply = get(&app, &playlist_uri(&item, "720p")).send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.header("content-type"),
        Some("application/vnd.apple.mpegurl")
    );
    assert_eq!(reply.header("cache-control"), Some("no-store"));

    let text = String::from_utf8(reply.body).unwrap();
    assert!(text.starts_with("#EXTM3U\n"), "{text}");
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD"), "{text}");
    assert!(text.trim_end().ends_with("#EXT-X-ENDLIST"), "{text}");
    // 7 s in 2 s segments: three whole and a 1 s tail.
    let uris = segment_uris(&text, &item, "720p");
    assert_eq!(uris.len(), 4, "{text}");
    let durations: Vec<f64> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#EXTINF:"))
        .map(|l| l.trim_end_matches(',').parse().unwrap())
        .collect();
    assert_eq!(&durations[..3], [2.0, 2.0, 2.0]);
    assert!((0.9..=1.1).contains(&durations[3]), "{durations:?}");
    // One session across the playlist.
    let session = uris[0].split("?s=").nth(1).unwrap();
    assert!(uris.iter().all(|u| u.ends_with(session)));
}

#[tokio::test]
async fn segments_are_mpeg_ts_and_any_one_can_be_asked_for_first() {
    let env = TestEnv::new();
    write_clip(&env, 7);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let item = item_id(&app, CLIP).await;
    let playlist = get(&app, &playlist_uri(&item, "480p")).send().await;
    let uris = segment_uris(&String::from_utf8(playlist.body).unwrap(), &item, "480p");

    // The last segment first: the encoder starts there.
    let last = get(&app, &uris[3]).send().await;
    assert_eq!(last.status, StatusCode::OK);
    assert_eq!(last.header("content-type"), Some("video/mp2t"));
    assert_eq!(last.body[0], 0x47, "MPEG-TS starts with a sync byte");
    // Then the first, which is a restart the other way.
    let first = get(&app, &uris[0]).send().await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body[0], 0x47);
    assert_ne!(first.body, last.body);
}

#[tokio::test]
async fn bad_requests_get_the_right_errors() {
    let env = TestEnv::new();
    write_clip(&env, 7);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let item = item_id(&app, CLIP).await;
    let session = "0192f3a0-0000-7000-8000-000000000001";
    let seg = |profile: &str, name: &str, s: &str| {
        format!("/api/v1/transcode/{item}/{profile}/{name}{s}")
    };
    let cases = [
        // No such profile: not the ladder's, and not `original` or `auto`.
        (playlist_uri(&item, "4k-ultra"), StatusCode::NOT_FOUND),
        (playlist_uri(&item, "original"), StatusCode::NOT_FOUND),
        (playlist_uri(&item, "auto"), StatusCode::NOT_FOUND),
        (playlist_uri("no-such-item", "720p"), StatusCode::NOT_FOUND),
        // Segments need the session.
        (seg("720p", "0.ts", ""), StatusCode::BAD_REQUEST),
        (seg("720p", "0.ts", "?s=nope"), StatusCode::BAD_REQUEST),
        // Not a segment name, or past the end (there are 4).
        (
            seg("720p", "zero.ts", &format!("?s={session}")),
            StatusCode::NOT_FOUND,
        ),
        (
            seg("720p", "0.mp4", &format!("?s={session}")),
            StatusCode::NOT_FOUND,
        ),
        (
            seg("720p", "4.ts", &format!("?s={session}")),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (uri, expect) in cases {
        let reply = get(&app, &uri).send().await;
        assert_eq!(reply.status, expect, "{uri}");
    }

    // A session belongs to the item and profile it was made for: it exists
    // once its first segment has been asked for.
    let playlist = get(&app, &playlist_uri(&item, "720p")).send().await;
    let text = String::from_utf8(playlist.body).unwrap();
    let first = segment_uris(&text, &item, "720p")[0].clone();
    assert_eq!(get(&app, &first).send().await.status, StatusCode::OK);
    let id = first.split("?s=").nth(1).unwrap();
    let other = get(&app, &seg("480p", "0.ts", &format!("?s={id}")))
        .send()
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_session_the_server_has_forgotten_is_made_again() {
    let env = TestEnv::new();
    write_clip(&env, 7);
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let item = item_id(&app, CLIP).await;
    // A player resuming after the server dropped its idle session (or
    // restarted) asks for a segment under an ID the server has not got.
    let uri = format!("/api/v1/transcode/{item}/720p/1.ts?s=0192f3a0-0000-7000-8000-0000000000aa");
    let reply = get(&app, &uri).send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body[0], 0x47);
}

#[tokio::test]
async fn an_item_ffprobe_cannot_read_is_a_bad_gateway() {
    let env = TestEnv::new();
    let Some(app) = transcoding_app(&env, "").await else {
        return;
    };
    let item = item_id(&app, FAKE).await;
    let reply = get(&app, &playlist_uri(&item, "720p")).send().await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
    assert_eq!(reply.error_code(), "upstream");
}

#[tokio::test]
async fn only_max_sessions_run_at_once() {
    let env = TestEnv::new();
    write_clip(&env, 7);
    let Some(app) = transcoding_app(&env, "max_sessions = 1").await else {
        return;
    };
    let item = item_id(&app, CLIP).await;

    // Playlists cost nothing: a player may fetch several.
    let mut uris = Vec::new();
    for _ in 0..3 {
        let playlist = get(&app, &playlist_uri(&item, "720p")).send().await;
        assert_eq!(playlist.status, StatusCode::OK);
        uris.push(segment_uris(
            &String::from_utf8(playlist.body).unwrap(),
            &item,
            "720p",
        ));
    }
    // The first segment asked for takes the one slot.
    let first = get(&app, &uris[0][0]).send().await;
    assert_eq!(first.status, StatusCode::OK);
    // A second session cannot start, and no longer gets a playlist either.
    let second = get(&app, &uris[1][0]).send().await;
    assert_eq!(second.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(second.error_code(), "unavailable");
    let refused = get(&app, &playlist_uri(&item, "720p")).send().await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    // The session that has the slot carries on.
    let more = get(&app, &uris[0][1]).send().await;
    assert_eq!(more.status, StatusCode::OK);
}

#[tokio::test]
async fn without_transcoding_the_routes_are_unavailable() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert!(!app.transcode_enabled());
    let id = item_id(&app, CLIP).await;
    let playlist = get(&app, &playlist_uri(&id, "720p")).send().await;
    assert_eq!(playlist.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(playlist.error_code(), "unavailable");
    let segment = get(
        &app,
        &format!("/api/v1/transcode/{id}/720p/0.ts?s=0192f3a0-0000-7000-8000-000000000001"),
    )
    .send()
    .await;
    assert_eq!(segment.status, StatusCode::SERVICE_UNAVAILABLE);
}
