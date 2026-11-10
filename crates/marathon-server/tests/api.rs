//! Integration tests for every route in PLAN.md §8, driven in-process with
//! `tower::ServiceExt::oneshot`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::http::{Method, StatusCode};
use common::{SUBTITLE_TEXT, TestEnv, VIDEOS, advance, get, item_id, now, path_of, post};
use serde_json::{Value, json};

// --- /health, /version ------------------------------------------------------

#[tokio::test]
async fn health_and_version() {
    let env = TestEnv::new();
    let app = env.app().await;

    let reply = get(&app, "/api/v1/health").send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json::<Value>(), json!({ "status": "ok" }));

    let reply = get(&app, "/api/v1/version").send().await;
    assert_eq!(reply.status, StatusCode::OK);
    let version: Value = reply.json();
    assert_eq!(version["api_version"], 1);
    assert_eq!(version["server_version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn unknown_routes_are_json_404s() {
    let env = TestEnv::new();
    let app = env.app().await;
    let reply = get(&app, "/api/v1/nope").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "not_found");
}

// --- /channels ----------------------------------------------------------------

#[tokio::test]
async fn channels_lists_every_channel_by_number_with_current_item() {
    let env = TestEnv::new();
    let app = env.app().await;

    let reply = get(&app, "/api/v1/channels").send().await;
    assert_eq!(reply.status, StatusCode::OK);
    let channels: Vec<Value> = reply.json();
    let ids: Vec<_> = channels.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["show", "essays", "seasons", "misc", "empty"]);

    let show = &channels[0];
    assert_eq!(show["number"], 1);
    assert_eq!(show["name"], "Test Show");
    assert_eq!(show["item_count"], 4);
    assert_eq!(show["current"]["path"], VIDEOS[0]);
    assert_eq!(show["position_ms"], 0);

    let empty = &channels[4];
    assert_eq!(empty["item_count"], 0);
    assert_eq!(empty["current"], Value::Null);
}

// --- /channels/{id}/now -------------------------------------------------------

#[tokio::test]
async fn now_returns_item_stream_subtitles_and_next() {
    let env = TestEnv::new();
    let app = env.app().await;

    let now = now(&app, "show").await;
    let pilot = item_id(&app, VIDEOS[0]).await;
    let second = item_id(&app, VIDEOS[1]).await;
    assert_eq!(now["channel_id"], "show");
    assert_eq!(now["channel_number"], 1);
    assert_eq!(now["channel_name"], "Test Show");
    assert_eq!(now["item"]["id"], pilot);
    assert_eq!(now["item"]["show"], "Test Show");
    assert_eq!(now["item"]["season"], 1);
    assert_eq!(now["item"]["episode"], 1);
    assert_eq!(now["item"]["title"], "Pilot");
    assert_eq!(now["position_ms"], 0);
    assert_eq!(now["stream_url"], format!("/api/v1/stream/{pilot}"));
    assert_eq!(
        now["item"]["subtitles"],
        json!([{
            "index": 0,
            "language": "en",
            "label": "en",
            "format": "srt",
            "url": format!("/api/v1/subtitles/{pilot}/0"),
        }])
    );
    assert_eq!(now["next"]["item"]["id"], second);
    assert_eq!(
        now["next"]["stream_url"],
        format!("/api/v1/stream/{second}")
    );
}

#[tokio::test]
async fn now_is_stable_and_records_tuned_events() {
    let env = TestEnv::new();
    let app = env.app().await;

    // A shuffled channel picks its first item once, then sticks with it.
    let first = now(&app, "essays").await;
    for _ in 0..3 {
        assert_eq!(now(&app, "essays").await["item"]["id"], first["item"]["id"]);
    }
    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    assert_eq!(history.len(), 4);
    assert!(history.iter().all(|e| e["kind"] == "tuned"));
}

#[tokio::test]
async fn now_errors() {
    let env = TestEnv::new();
    let app = env.app().await;

    let reply = get(&app, "/api/v1/channels/nope/now").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "not_found");

    let reply = get(&app, "/api/v1/channels/empty/now").send().await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error_code(), "channel_empty");
}

#[tokio::test]
async fn now_moves_on_when_the_current_item_goes_missing() {
    let env = TestEnv::new();
    let app = env.app().await;
    assert_eq!(path_of(&now(&app, "show").await), VIDEOS[0]);

    std::fs::remove_file(env.library().join(VIDEOS[0])).unwrap();
    app.rescan().await.unwrap();
    let now = now(&app, "show").await;
    assert_eq!(path_of(&now), VIDEOS[1]);
    assert_eq!(now["position_ms"], 0);
}

// --- /channels/{id}/progress --------------------------------------------------

#[tokio::test]
async fn progress_updates_state_and_appends_events() {
    let env = TestEnv::new();
    let app = env.app().await;
    let current = now(&app, "show").await["item"]["id"].clone();

    let report = |position: u64, paused: bool| {
        post(&app, "/api/v1/channels/show/progress")
            .header("X-Marathon-Client", "client-a")
            .json(json!({
                "item_id": current,
                "position_ms": position,
                "duration_ms": 1_320_000,
                "paused": paused,
            }))
    };
    assert_eq!(
        report(5000, false).send().await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        report(10000, false).send().await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        report(12000, true).send().await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        report(12000, false).send().await.status,
        StatusCode::NO_CONTENT
    );

    // channel_state holds the latest position…
    let (position,): (i64,) =
        sqlx::query_as("SELECT position_ms FROM channel_state WHERE channel_id = 'show'")
            .fetch_one(app.db().pool())
            .await
            .unwrap();
    assert_eq!(position, 12000);
    let now = now(&app, "show").await;
    assert_eq!(now["position_ms"], 12000);
    // …and the reported duration is stored on the item.
    assert_eq!(now["item"]["duration_ms"], 1_320_000);

    // …and every report appended an event, newest first in history.
    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    let kinds: Vec<_> = history
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "tuned", "resumed", "paused", "progress", "progress", "tuned"
        ]
    );
    let positions: Vec<_> = history
        .iter()
        .map(|e| e["position_ms"].as_u64().unwrap())
        .collect();
    assert_eq!(positions, [12000, 12000, 12000, 10000, 5000, 0]);
    assert_eq!(history[1]["client_id"], "client-a");
    assert_eq!(history[1]["channel_id"], "show");
    assert_eq!(history[1]["item_id"], current);
    assert_eq!(history[0]["client_id"], Value::Null);
    // Timestamps never go backwards in newest-first order.
    let ts: Vec<_> = history
        .iter()
        .map(|e| e["ts_ms"].as_i64().unwrap())
        .collect();
    assert!(ts.windows(2).all(|w| w[0] >= w[1]), "{ts:?}");
}

#[tokio::test]
async fn progress_for_a_stale_item_is_rejected() {
    let env = TestEnv::new();
    let app = env.app().await;
    let old = now(&app, "show").await["item"]["id"].clone();
    advance(&app, "show").await;

    let reply = post(&app, "/api/v1/channels/show/progress")
        .json(json!({ "item_id": old, "position_ms": 99_000 }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.error_code(), "stale_item");
    assert_eq!(now(&app, "show").await["position_ms"], 0);
}

#[tokio::test]
async fn malformed_bodies_are_400s() {
    let env = TestEnv::new();
    let app = env.app().await;
    let reply = post(&app, "/api/v1/channels/show/progress")
        .json(json!({ "position_ms": "soon" }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.error_code(), "bad_request");

    let reply = post(&app, "/api/v1/channels/show/advance")
        .json(json!({ "from_item_id": "x", "reason": "bored" }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

// --- /channels/{id}/advance ---------------------------------------------------

#[tokio::test]
async fn advance_walks_the_ordered_channel_and_loops() {
    let env = TestEnv::new();
    let app = env.app().await;
    let mut seen = vec![path_of(&now(&app, "show").await).to_owned()];
    for _ in 0..4 {
        seen.push(path_of(&advance(&app, "show").await).to_owned());
    }
    assert_eq!(
        seen,
        [VIDEOS[0], VIDEOS[1], VIDEOS[2], VIDEOS[3], VIDEOS[0]]
    );
}

#[tokio::test]
async fn advance_is_idempotent() {
    let env = TestEnv::new();
    let app = env.app().await;
    let first = now(&app, "show").await;
    let body = json!({ "from_item_id": first["item"]["id"], "reason": "ended" });

    let a = post(&app, "/api/v1/channels/show/advance")
        .json(body.clone())
        .send()
        .await;
    let b = post(&app, "/api/v1/channels/show/advance")
        .json(body)
        .send()
        .await;
    assert_eq!(a.status, StatusCode::OK);
    assert_eq!(b.status, StatusCode::OK);
    let (a, b): (Value, Value) = (a.json(), b.json());
    assert_eq!(path_of(&a), VIDEOS[1]);
    assert_eq!(
        a["item"]["id"], b["item"]["id"],
        "second call must not advance again"
    );
    assert_eq!(path_of(&now(&app, "show").await), VIDEOS[1]);

    let history: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    let ended = history.iter().filter(|e| e["kind"] == "ended").count();
    assert_eq!(ended, 1);
}

#[tokio::test]
async fn concurrent_advances_from_the_same_item_advance_once() {
    let env = TestEnv::new();
    let app = env.app().await;
    let first = now(&app, "show").await;
    let body = json!({ "from_item_id": first["item"]["id"], "reason": "ended" });

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let app = app.clone();
        let body = body.clone();
        tasks.push(tokio::spawn(async move {
            post(&app, "/api/v1/channels/show/advance")
                .json(body)
                .send()
                .await
                .json::<Value>()
        }));
    }
    for task in tasks {
        assert_eq!(path_of(&task.await.unwrap()), VIDEOS[1]);
    }
}

#[tokio::test]
async fn advance_records_reason_and_position() {
    let env = TestEnv::new();
    let app = env.app().await;
    let first = now(&app, "show").await["item"]["id"].clone();
    post(&app, "/api/v1/channels/show/progress")
        .json(json!({ "item_id": first, "position_ms": 7000 }))
        .send()
        .await;
    let reply = post(&app, "/api/v1/channels/show/advance")
        .header("X-Marathon-Client", "client-b")
        .json(json!({ "from_item_id": first, "reason": "skipped" }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let now: Value = reply.json();
    assert_eq!(now["position_ms"], 0, "the next item starts from the top");

    let history: Vec<Value> = get(&app, "/api/v1/history?limit=1").send().await.json();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["kind"], "skipped");
    assert_eq!(history[0]["item_id"], first);
    assert_eq!(history[0]["position_ms"], 7000);
    assert_eq!(history[0]["client_id"], "client-b");
}

#[tokio::test]
async fn shuffled_and_grouped_channels_follow_their_rules() {
    let env = TestEnv::new();
    let app = env.app().await;

    // Shuffled: three essays, each once per cycle.
    let mut cycle = vec![path_of(&now(&app, "essays").await).to_owned()];
    for _ in 0..2 {
        cycle.push(path_of(&advance(&app, "essays").await).to_owned());
    }
    cycle.sort();
    assert_eq!(
        cycle,
        [
            "Essays/Deep Dives/why_doors_matter.webm",
            "Essays/Ep 10 - Difficulty Curves.mp4",
            "Essays/Ep 2 - Pacing.mkv",
        ]
    );

    // Grouped by season: a season's episodes play together, in order.
    let mut seq = vec![now(&app, "seasons").await];
    for _ in 0..3 {
        seq.push(advance(&app, "seasons").await);
    }
    let codes: Vec<_> = seq
        .iter()
        .map(|n| {
            (
                n["item"]["season"].as_u64().unwrap(),
                n["item"]["episode"].as_u64().unwrap(),
            )
        })
        .collect();
    assert!(
        codes == [(1, 1), (1, 2), (2, 1), (2, 2)] || codes == [(2, 1), (2, 2), (1, 1), (1, 2)],
        "{codes:?}"
    );
}

#[tokio::test]
async fn single_item_channel_repeats() {
    let env = TestEnv::new();
    let app = env.app().await;
    let first = now(&app, "misc").await;
    assert_eq!(first["next"]["item"]["id"], first["item"]["id"]);
    assert_eq!(
        advance(&app, "misc").await["item"]["id"],
        first["item"]["id"]
    );
}

// --- /channels/{id}/previous --------------------------------------------------

#[tokio::test]
async fn previous_steps_back_and_is_idempotent() {
    let env = TestEnv::new();
    let app = env.app().await;
    now(&app, "show").await;
    advance(&app, "show").await;
    let at_second = advance(&app, "show").await;
    assert_eq!(path_of(&at_second), VIDEOS[2]);

    let body = json!({ "from_item_id": at_second["item"]["id"] });
    let back: Value = post(&app, "/api/v1/channels/show/previous")
        .json(body.clone())
        .send()
        .await
        .json();
    assert_eq!(path_of(&back), VIDEOS[1]);
    // Repeating the same request does not step back twice.
    let again: Value = post(&app, "/api/v1/channels/show/previous")
        .json(body)
        .send()
        .await
        .json();
    assert_eq!(path_of(&again), VIDEOS[1]);

    let history: Vec<Value> = get(&app, "/api/v1/history?limit=5").send().await.json();
    assert_eq!(
        history.iter().filter(|e| e["kind"] == "previous").count(),
        1
    );

    // And `next` from there returns to where we were.
    assert_eq!(path_of(&advance(&app, "show").await), VIDEOS[2]);
}

#[tokio::test]
async fn previous_with_nothing_to_go_back_to_stays_put() {
    let env = TestEnv::new();
    let app = env.app().await;
    // A fresh shuffle has no history before its first item.
    let first = now(&app, "essays").await;
    let reply = post(&app, "/api/v1/channels/essays/previous")
        .json(json!({ "from_item_id": first["item"]["id"] }))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json::<Value>()["item"]["id"], first["item"]["id"]);
}

// --- /items/{id} --------------------------------------------------------------

#[tokio::test]
async fn items_returns_metadata() {
    let env = TestEnv::new();
    let app = env.app().await;
    let id = item_id(&app, VIDEOS[2]).await;

    let reply = get(&app, &format!("/api/v1/items/{id}")).send().await;
    assert_eq!(reply.status, StatusCode::OK);
    let item: Value = reply.json();
    assert_eq!(item["id"], id);
    assert_eq!(item["source_id"], "lib");
    assert_eq!(item["path"], VIDEOS[2]);
    assert_eq!(item["season"], 2);
    assert_eq!(item["episode"], 1);
    assert_eq!(item["title"], "New Beginnings");
    assert_eq!(item["duration_ms"], Value::Null);
    assert_eq!(item["subtitles"], json!([]));
    assert_eq!(item["missing"], false);

    let reply = get(&app, "/api/v1/items/nope").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

// --- /stream/{item_id} --------------------------------------------------------

#[tokio::test]
async fn stream_serves_whole_file() {
    let env = TestEnv::new();
    let app = env.app().await;
    let path = VIDEOS[0];
    let id = item_id(&app, path).await;
    let expected = common::video_bytes(path);

    let reply = get(&app, &format!("/api/v1/stream/{id}")).send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, expected);
    assert_eq!(reply.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        reply.header("content-length"),
        Some(expected.len().to_string().as_str())
    );
    assert_eq!(reply.header("content-type"), Some("video/x-matroska"));
}

#[tokio::test]
async fn stream_serves_ranges() {
    let env = TestEnv::new();
    let app = env.app().await;
    let path = VIDEOS[0];
    let id = item_id(&app, path).await;
    let expected = common::video_bytes(path);
    let len = expected.len();

    let reply = get(&app, &format!("/api/v1/stream/{id}"))
        .header("Range", "bytes=10-19")
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        reply.header("content-range"),
        Some(format!("bytes 10-19/{len}").as_str())
    );
    assert_eq!(reply.body, &expected[10..20]);

    // Open-ended and suffix ranges, as players use when seeking.
    let reply = get(&app, &format!("/api/v1/stream/{id}"))
        .header("Range", format!("bytes={}-", len - 5).as_str())
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        reply.header("content-range"),
        Some(format!("bytes {}-{}/{len}", len - 5, len - 1).as_str())
    );
    assert_eq!(reply.body, &expected[len - 5..]);

    let reply = get(&app, &format!("/api/v1/stream/{id}"))
        .header("Range", "bytes=-3")
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(reply.body, &expected[len - 3..]);
}

#[tokio::test]
async fn stream_rejects_unsatisfiable_ranges() {
    let env = TestEnv::new();
    let app = env.app().await;
    let path = VIDEOS[0];
    let id = item_id(&app, path).await;
    let len = common::video_bytes(path).len();

    let reply = get(&app, &format!("/api/v1/stream/{id}"))
        .header("Range", format!("bytes={len}-{}", len + 100).as_str())
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        reply.header("content-range"),
        Some(format!("bytes */{len}").as_str())
    );
}

#[tokio::test]
async fn stream_supports_head_and_odd_file_names() {
    let env = TestEnv::new();
    let app = env.app().await;
    let path = "Misc/Café Society's Ünïcode Clip — Ørsted.mkv";
    let id = item_id(&app, path).await;

    let reply = common::call(&app, Method::HEAD, &format!("/api/v1/stream/{id}"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.body.is_empty());
    let len = common::video_bytes(path).len().to_string();
    assert_eq!(reply.header("content-length"), Some(len.as_str()));

    let reply = get(&app, &format!("/api/v1/stream/{id}")).send().await;
    assert_eq!(reply.body, common::video_bytes(path));
}

#[tokio::test]
async fn stream_404s_for_unknown_and_missing_items() {
    let env = TestEnv::new();
    let app = env.app().await;
    let reply = get(&app, "/api/v1/stream/nope").send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let id = item_id(&app, VIDEOS[5]).await;
    std::fs::remove_file(env.library().join(VIDEOS[5])).unwrap();
    // Even before a rescan notices, a vanished file is a 404, not a 500.
    let reply = get(&app, &format!("/api/v1/stream/{id}")).send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    app.rescan().await.unwrap();
    let reply = get(&app, &format!("/api/v1/stream/{id}")).send().await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.error_code(), "not_found");
}

// --- /subtitles/{item_id}/{n} -------------------------------------------------

#[tokio::test]
async fn subtitles_serves_sidecar_files() {
    let env = TestEnv::new();
    let app = env.app().await;
    let id = item_id(&app, VIDEOS[0]).await;

    let reply = get(&app, &format!("/api/v1/subtitles/{id}/0")).send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, SUBTITLE_TEXT.as_bytes());
    assert_eq!(reply.header("content-type"), Some("application/x-subrip"));

    for bad in [
        format!("/api/v1/subtitles/{id}/1"),
        "/api/v1/subtitles/nope/0".to_owned(),
    ] {
        let reply = get(&app, &bad).send().await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{bad}");
    }
    let reply = get(&app, &format!("/api/v1/subtitles/{id}/first"))
        .send()
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

// --- /history -----------------------------------------------------------------

#[tokio::test]
async fn history_is_newest_first_and_limited() {
    let env = TestEnv::new();
    let app = env.app().await;
    for channel in ["show", "essays", "misc"] {
        now(&app, channel).await;
    }

    let all: Vec<Value> = get(&app, "/api/v1/history").send().await.json();
    let channels: Vec<_> = all
        .iter()
        .map(|e| e["channel_id"].as_str().unwrap())
        .collect();
    assert_eq!(channels, ["misc", "essays", "show"]);

    let two: Vec<Value> = get(&app, "/api/v1/history?limit=2").send().await.json();
    assert_eq!(two, all[..2]);

    let reply = get(&app, "/api/v1/history?limit=lots").send().await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
}

// --- /library/rescan ----------------------------------------------------------

#[tokio::test]
async fn rescan_picks_up_changes_and_updates_channels() {
    let env = TestEnv::new();
    let app = env.app().await;
    let new = "TV/Test Show/Season 2/Test Show - S02E03 - Encore.mkv";
    env.write_video(new);
    std::fs::remove_file(env.library().join(VIDEOS[6])).unwrap();

    let reply = post(&app, "/api/v1/library/rescan").send().await;
    assert_eq!(reply.status, StatusCode::OK);
    let report: Value = reply.json();
    assert_eq!(report["sources_scanned"], 1);
    assert_eq!(report["added"], 1);
    assert_eq!(report["newly_missing"], 1);
    assert_eq!(report["items_present"], VIDEOS.len());
    assert_eq!(report["errors"], json!([]));

    let channels: Vec<Value> = get(&app, "/api/v1/channels").send().await.json();
    assert_eq!(channels[0]["item_count"], 5, "new episode joins the show");
    assert_eq!(
        channels[1]["item_count"], 2,
        "missing essay leaves the shuffle"
    );
}

// --- /openapi.json ------------------------------------------------------------

#[tokio::test]
async fn openapi_route_serves_the_document() {
    let env = TestEnv::new();
    let app = env.app().await;
    let reply = get(&app, "/api/v1/openapi.json").send().await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), Some("application/json"));
    let doc: Value = reply.json();
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
}
